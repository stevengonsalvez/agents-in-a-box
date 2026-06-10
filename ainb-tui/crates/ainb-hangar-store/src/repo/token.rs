//! Typed repository wrappers over the `pat` and `daemon_token` tables.
//!
//! Both tables follow the same hashing discipline (`0005_pat_daemon_token...`):
//! the plaintext token is never stored, only its SHA-256 hex digest in the
//! `sha256_token` column (UNIQUE, for O(1) lookup-by-hash). Authentication
//! re-hashes the presented plaintext and compares constant-time via the
//! [`ainb_hangar_core::token`] primitives — see [`PatRepo::verify`] /
//! [`DaemonTokenRepo::verify`].
//!
//! # Minting
//!
//! [`mint_pat`] / [`mint_daemon_token`] are the orchestrating entry points: they
//! draw randomness from an injectable [`rand::RngCore`] source, stamp
//! `created_at` from a [`HangarClock`], allocate the row id from an [`IdGen`],
//! persist only the digest, and return the plaintext to show **once**. Tests
//! inject [`ainb_hangar_core::clock::FixedClock`] /
//! [`ainb_hangar_core::idgen::FixedIdGen`] plus a seeded RNG so every field but
//! the (asserted-only-by-invariant) random body is deterministic.
//!
//! # Revoke
//!
//! Revocation is a hard `DELETE` of the row. The schema carries no `revoked`
//! flag, and a deleted hash can never be re-hashed-to, so a revoked token
//! verifies as `None` immediately and the UNIQUE `sha256_token` index is freed.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::token::{self, MintedToken, TokenKind};
use rand::RngCore;
use sqlx::SqlitePool;

/// A persisted `pat` row (the digest, never the plaintext).
///
/// Mirrors the `pat` table columns one-to-one. `scope` is a nullable opaque
/// string (`read`/`write`/`admin` at the CLI layer, but the store treats it as
/// opaque). `last_used` is `None` until the first successful auth touches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatRecord {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning user (`user.id`).
    pub user_id: String,
    /// Lower-case hex SHA-256 of the (never-stored) plaintext token.
    pub sha256_token: String,
    /// Opaque scope string, or `None`.
    pub scope: Option<String>,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
    /// Last successful-auth timestamp (epoch milliseconds), or `None`.
    pub last_used: Option<i64>,
}

/// A persisted `daemon_token` row (the digest, never the plaintext).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonTokenRecord {
    /// Primary key (ULID string).
    pub id: String,
    /// Lower-case hex SHA-256 of the (never-stored) plaintext token.
    pub sha256_token: String,
    /// Runtime this token is bound to (`agent_runtime.id`).
    pub runtime_id: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// Stateless typed wrapper over the `pat` table.
pub struct PatRepo;

/// Stateless typed wrapper over the `daemon_token` table.
pub struct DaemonTokenRepo;

/// Mint a fresh PAT for `user_id`, persist only its digest, and return the row
/// alongside the plaintext to show **once**.
///
/// Randomness comes from `rng`, the row id from `idgen`, and `created_at` from
/// `clock`, so tests inject deterministic doubles. The returned
/// [`MintedToken::plaintext`] is the only copy of the plaintext — it is never
/// written to the database. On the (astronomically unlikely) event of a
/// `sha256_token` UNIQUE collision the insert errors rather than silently
/// overwriting.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the insert fails — most commonly a `user_id` FK
/// violation, or a `sha256_token` UNIQUE collision.
pub async fn mint_pat<R: RngCore>(
    pool: &SqlitePool,
    user_id: &str,
    scope: Option<&str>,
    clock: &dyn HangarClock,
    idgen: &dyn IdGen,
    rng: &mut R,
) -> Result<(PatRecord, MintedToken), sqlx::Error> {
    let minted = token::mint(TokenKind::Pat, rng);
    let record = PatRecord {
        id: idgen.new_ulid(),
        user_id: user_id.to_string(),
        sha256_token: minted.sha256_hex.clone(),
        scope: scope.map(ToString::to_string),
        created_at: clock.now_ms(),
        last_used: None,
    };
    PatRepo::insert(pool, &record).await?;
    Ok((record, minted))
}

/// Mint a fresh daemon token bound to `runtime_id`, persist only its digest,
/// and return the row alongside the plaintext to show **once**.
///
/// See [`mint_pat`] for the injection contract.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the insert fails — most commonly a `runtime_id`
/// FK violation, or a `sha256_token` UNIQUE collision.
pub async fn mint_daemon_token<R: RngCore>(
    pool: &SqlitePool,
    runtime_id: &str,
    clock: &dyn HangarClock,
    idgen: &dyn IdGen,
    rng: &mut R,
) -> Result<(DaemonTokenRecord, MintedToken), sqlx::Error> {
    let minted = token::mint(TokenKind::Daemon, rng);
    let record = DaemonTokenRecord {
        id: idgen.new_ulid(),
        sha256_token: minted.sha256_hex.clone(),
        runtime_id: runtime_id.to_string(),
        created_at: clock.now_ms(),
    };
    DaemonTokenRepo::insert(pool, &record).await?;
    Ok((record, minted))
}

impl PatRepo {
    /// Insert one [`PatRecord`].
    ///
    /// Prefer [`mint_pat`] over calling this directly — the orchestrator owns
    /// the generate-then-store sequence so the plaintext is never separated
    /// from its digest by hand. This is exposed for the orchestrator and for
    /// tests that need to seed a known digest.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a `user_id` FK violation or a
    /// `sha256_token` UNIQUE collision.
    pub async fn insert(pool: &SqlitePool, record: &PatRecord) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO pat (id, user_id, sha256_token, scope, created_at, last_used) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.id)
        .bind(&record.user_id)
        .bind(&record.sha256_token)
        .bind(&record.scope)
        .bind(record.created_at)
        .bind(record.last_used)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Look up a PAT row by its stored SHA-256 hex digest, or `None`.
    ///
    /// This is the auth fast-path: hash the presented plaintext once, then a
    /// single indexed lookup on the UNIQUE `sha256_token` column.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails (a missing row is
    /// `Ok(None)`).
    pub async fn get_by_hash(
        pool: &SqlitePool,
        sha256_token: &str,
    ) -> Result<Option<PatRecord>, sqlx::Error> {
        sqlx::query_as::<_, PatRecord>(
            "SELECT id, user_id, sha256_token, scope, created_at, last_used \
             FROM pat WHERE sha256_token = ?",
        )
        .bind(sha256_token)
        .fetch_optional(pool)
        .await
    }

    /// List every PAT owned by `user_id`, newest first. Never exposes a
    /// plaintext (there is none to expose — only the digest is stored).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_user(
        pool: &SqlitePool,
        user_id: &str,
    ) -> Result<Vec<PatRecord>, sqlx::Error> {
        sqlx::query_as::<_, PatRecord>(
            "SELECT id, user_id, sha256_token, scope, created_at, last_used \
             FROM pat WHERE user_id = ? ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }

    /// Verify a presented `plaintext` against the stored digests.
    ///
    /// Hashes the plaintext, fetches the row by that hash, and — when found —
    /// confirms the match in constant time via [`token::verify`]. Returns the
    /// matched [`PatRecord`] on success, `None` on any miss (unknown hash,
    /// revoked token, or tampered plaintext). The constant-time re-compare
    /// guards against a future caller passing an attacker-influenced
    /// `sha256_token` lookup key.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the lookup query fails.
    pub async fn verify(
        pool: &SqlitePool,
        plaintext: &str,
    ) -> Result<Option<PatRecord>, sqlx::Error> {
        let hash = token::sha256_hex(plaintext);
        let Some(record) = Self::get_by_hash(pool, &hash).await? else {
            return Ok(None);
        };
        if token::verify(plaintext, &record.sha256_token) {
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    /// Stamp `last_used` on a PAT (called after a successful auth). Updating an
    /// absent id affects zero rows and is not an error.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn touch_last_used(
        pool: &SqlitePool,
        id: &str,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE pat SET last_used = ? WHERE id = ?")
            .bind(now_ms)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Revoke a PAT by primary key (hard `DELETE`). Returns `true` if a row was
    /// removed, `false` if the id was already absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the delete fails.
    pub async fn revoke(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM pat WHERE id = ?").bind(id).execute(pool).await?;
        Ok(result.rows_affected() > 0)
    }
}

impl DaemonTokenRepo {
    /// Insert one [`DaemonTokenRecord`]. Prefer [`mint_daemon_token`].
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a `runtime_id` FK violation or a
    /// `sha256_token` UNIQUE collision.
    pub async fn insert(pool: &SqlitePool, record: &DaemonTokenRecord) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO daemon_token (id, sha256_token, runtime_id, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&record.id)
        .bind(&record.sha256_token)
        .bind(&record.runtime_id)
        .bind(record.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Look up a daemon-token row by its stored SHA-256 hex digest, or `None`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get_by_hash(
        pool: &SqlitePool,
        sha256_token: &str,
    ) -> Result<Option<DaemonTokenRecord>, sqlx::Error> {
        sqlx::query_as::<_, DaemonTokenRecord>(
            "SELECT id, sha256_token, runtime_id, created_at \
             FROM daemon_token WHERE sha256_token = ?",
        )
        .bind(sha256_token)
        .fetch_optional(pool)
        .await
    }

    /// Verify a presented daemon-token `plaintext`. See [`PatRepo::verify`].
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the lookup query fails.
    pub async fn verify(
        pool: &SqlitePool,
        plaintext: &str,
    ) -> Result<Option<DaemonTokenRecord>, sqlx::Error> {
        let hash = token::sha256_hex(plaintext);
        let Some(record) = Self::get_by_hash(pool, &hash).await? else {
            return Ok(None);
        };
        if token::verify(plaintext, &record.sha256_token) {
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    /// Revoke a daemon token by primary key (hard `DELETE`). Returns `true` if a
    /// row was removed.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the delete fails.
    pub async fn revoke(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM daemon_token WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for PatRecord {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            sha256_token: row.try_get("sha256_token")?,
            scope: row.try_get("scope")?,
            created_at: row.try_get("created_at")?,
            last_used: row.try_get("last_used")?,
        })
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for DaemonTokenRecord {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            sha256_token: row.try_get("sha256_token")?,
            runtime_id: row.try_get("runtime_id")?,
            created_at: row.try_get("created_at")?,
        })
    }
}
