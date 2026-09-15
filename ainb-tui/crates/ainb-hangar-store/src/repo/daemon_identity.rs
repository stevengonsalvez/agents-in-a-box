//! The daemon's own identity (migration 0100, spec D11, #1066).
//!
//! ```text
//! boot ──▶ mint_or_read ──▶ INSERT OR IGNORE singleton ──▶ read it back
//!                      └──▶ fleet_session 'local' rows ──▶ this host id
//! after boot ──▶ adopt_local_events (bounded batches) ──▶ until 0
//! ```
//!
//! The id is a ULID minted once and kept. Every writer of a `host_id` column
//! reads it from here: [`host_id_on`] inside the writer's own transaction, so a
//! row and the identity it names can never come from two different reads.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use sqlx::{Row, SqliteConnection, SqlitePool};

/// The `host_id` every column held before a daemon minted one, and what a
/// writer still stamps on a home that has no identity yet.
pub const UNMINTED_HOST_ID: &str = "local";

/// Rows adopted per `fleet_event` batch. Small enough that one batch holds the
/// write lock for milliseconds, not the minutes a single statement over a
/// gigabyte table would.
pub const EVENT_ADOPTION_BATCH: i64 = 5_000;

/// This daemon's identity row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonIdentity {
    /// The ULID this daemon names itself with.
    pub host_id: String,
    /// The operator-facing label. `None` until R1 lets an operator set one.
    pub display_name: Option<String>,
    /// Unix milliseconds of the mint.
    pub created_at: i64,
}

/// What [`DaemonIdentityRepo::mint_or_read`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintOutcome {
    /// The identity in force after the call.
    pub identity: DaemonIdentity,
    /// Whether this call minted it. `false` on every boot after the first.
    pub minted: bool,
    /// `fleet_session` rows moved from [`UNMINTED_HOST_ID`] to this host.
    pub adopted_sessions: u64,
}

/// Stateless typed wrapper over the `daemon_identity` table.
pub struct DaemonIdentityRepo;

impl DaemonIdentityRepo {
    /// Mint this daemon's identity if the home has none, read it otherwise, and
    /// adopt every `fleet_session` row still named [`UNMINTED_HOST_ID`].
    ///
    /// One `IMMEDIATE` transaction, so two daemons racing on one home settle on
    /// one id, and no writer can stamp a session `local` between the mint and
    /// the adoption. Idempotent: a second call mints nothing and adopts only
    /// rows written since by a pre-#1066 binary.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if any statement fails; nothing is committed.
    pub async fn mint_or_read(
        pool: &SqlitePool,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<MintOutcome, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO daemon_identity (singleton, host_id, created_at) \
             VALUES (1, ?, ?)",
        )
        .bind(idgen.new_ulid())
        .bind(clock.now_ms())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let identity = read_on(&mut tx).await?.ok_or(sqlx::Error::RowNotFound)?;
        let adopted_sessions =
            sqlx::query("UPDATE fleet_session SET host_id = ? WHERE host_id = ?")
                .bind(&identity.host_id)
                .bind(UNMINTED_HOST_ID)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        Ok(MintOutcome {
            identity,
            minted: inserted == 1,
            adopted_sessions,
        })
    }

    /// Read this daemon's identity, or `None` on a home no daemon has booted.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn read(pool: &SqlitePool) -> Result<Option<DaemonIdentity>, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        read_on(&mut conn).await
    }

    /// Move up to `limit` `fleet_event` rows from [`UNMINTED_HOST_ID`] to
    /// `host_id`, returning how many moved. Call until it returns 0.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn adopt_local_events(
        pool: &SqlitePool,
        host_id: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "UPDATE fleet_event SET host_id = ? WHERE rowid IN \
             (SELECT rowid FROM fleet_event WHERE host_id = ? LIMIT ?)",
        )
        .bind(host_id)
        .bind(UNMINTED_HOST_ID)
        .bind(limit)
        .execute(pool)
        .await?
        .rows_affected())
    }
}

/// The `host_id` a writer stamps, read on the writer's own connection:
/// the minted id, or [`UNMINTED_HOST_ID`] on a home no daemon has booted.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the query fails.
pub async fn host_id_on(conn: &mut SqliteConnection) -> Result<String, sqlx::Error> {
    Ok(read_on(conn)
        .await?
        .map_or_else(|| UNMINTED_HOST_ID.to_string(), |identity| identity.host_id))
}

async fn read_on(conn: &mut SqliteConnection) -> Result<Option<DaemonIdentity>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT host_id, display_name, created_at FROM daemon_identity WHERE singleton = 1",
    )
    .fetch_optional(conn)
    .await?;
    row.map(|row| {
        Ok(DaemonIdentity {
            host_id: row.try_get("host_id")?,
            display_name: row.try_get("display_name")?,
            created_at: row.try_get("created_at")?,
        })
    })
    .transpose()
}
