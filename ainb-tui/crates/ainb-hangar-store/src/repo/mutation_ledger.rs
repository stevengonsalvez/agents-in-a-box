//! The mutation ledger (migration 0097) — one durable row per operation, so a
//! retry is a READ (spec D18, critique amendments 15-19).
//!
//! The failure this table exists for is a LOST REPLY. The daemon committed, the
//! socket died, the client retried; without a ledger the second attempt is a
//! second execution, and for a mutation that types into a terminal that is a
//! second `yes` on an agent about to run a command.
//!
//! ```text
//! claim(host, principal, op_id)
//!   ├─ inserted        ──▶ Fresh          run the handler, then record_reply
//!   ├─ row, reply set  ──▶ Replay         return the stored reply verbatim
//!   ├─ row, in flight  ──▶ InFlight       another attempt owns it
//!   ├─ row, other body ──▶ BodyMismatch   different answer, same op id
//!   ├─ row, expired    ──▶ Expired        aged out; the effect is unknowable
//!   └─ row, other principal ──▶ Foreign   somebody else's op id
//! ```
//!
//! Every entry point exists twice: once taking a pool, once taking a
//! caller-owned transaction. The transaction form is not a convenience — it is
//! the whole tier-2 guarantee. A receipt written through the event outbox would
//! inherit that outbox's documented crash loss window, so the receipt has to be
//! in the SAME transaction as the state flip it describes.

use blake3::Hasher;
use serde_json::Value;
use sqlx::{Row, Sqlite, SqliteConnection, SqlitePool, Transaction};

/// The ledger's default `host_id` until R1 mints a per-daemon ULID.
///
/// A placeholder with a real column behind it: retrofitting a column INTO a
/// composite primary key is a table rebuild, and R1 backfilling a value is not.
pub const LOCAL_HOST_ID: &str = "local";

/// The principal for a connection on the local unix socket.
pub const LOCAL_PRINCIPAL: &str = "local";

/// Tier token for a mutation that only gets dispatch dedupe.
pub const TIER_DEDUPE: &str = "dedupe";
/// Tier token for a PTY-effecting mutation that also gets a receipt.
pub const TIER_RECEIPT: &str = "receipt";

/// Status token for a claim whose handler has not finished.
pub const STATUS_IN_FLIGHT: &str = "in_flight";
/// Status token for a mutation that was applied.
pub const STATUS_ACCEPTED: &str = "accepted";
/// Status token for a mutation the daemon refused.
pub const STATUS_REJECTED: &str = "rejected";
/// Status token for a mutation whose effect cannot be established.
pub const STATUS_UNKNOWN: &str = "unknown";

/// The `(host_id, principal, op_id)` ledger key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerKey {
    /// The daemon this row belongs to.
    pub host_id: String,
    /// `local` on the unix leg, `device:<id>` off-box.
    pub principal: String,
    /// The client-minted opaque op id.
    pub op_id: String,
}

impl LedgerKey {
    /// A key on the local unix leg.
    #[must_use]
    pub fn local(op_id: impl Into<String>) -> Self {
        Self {
            host_id: LOCAL_HOST_ID.to_string(),
            principal: LOCAL_PRINCIPAL.to_string(),
            op_id: op_id.into(),
        }
    }

    /// A key for a paired device.
    #[must_use]
    pub fn device(device_id: &str, op_id: impl Into<String>) -> Self {
        Self {
            host_id: LOCAL_HOST_ID.to_string(),
            principal: format!("device:{device_id}"),
            op_id: op_id.into(),
        }
    }
}

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRow {
    /// The row's key.
    pub key: LedgerKey,
    /// The wire method.
    pub method: String,
    /// The canonical body fingerprint.
    pub body_fingerprint: String,
    /// `dedupe` or `receipt`.
    pub tier: String,
    /// `in_flight`, `accepted`, `rejected` or `unknown`.
    pub status: String,
    /// Why, for a non-accepted status.
    pub reason: Option<String>,
    /// The serialized reply, replayed verbatim.
    pub reply: Option<String>,
    /// The receipt lifecycle token, for a tier-2 row.
    pub receipt_state: Option<String>,
    /// Operator-facing detail for a failed or unknown receipt.
    pub receipt_detail: Option<String>,
    /// Whether retention has dropped the reply.
    pub expired: bool,
    /// When the op id was first seen (epoch milliseconds).
    pub created_at: i64,
    /// When the row last changed (epoch milliseconds).
    pub updated_at: i64,
}

impl LedgerRow {
    /// The stored reply as JSON, when there is one this build can parse.
    #[must_use]
    pub fn reply_value(&self) -> Option<Value> {
        self.reply.as_deref().and_then(|raw| serde_json::from_str(raw).ok())
    }
}

/// What a [`MutationLedgerRepo::claim`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller owns the operation. Run the handler, then record the reply.
    Fresh,
    /// A committed row for this exact op id and body. Return its reply.
    Replay(Box<LedgerRow>),
    /// A claim exists whose handler has not answered yet.
    InFlight(Box<LedgerRow>),
    /// A committed row for this op id with a DIFFERENT body.
    BodyMismatch(Box<LedgerRow>),
    /// The row aged out of retention; the effect is no longer knowable.
    Expired(Box<LedgerRow>),
    /// The op id belongs to a different principal.
    Foreign {
        /// The principal that owns it.
        principal: String,
    },
}

/// How much history the ledger keeps (D18: 7 days or 100k rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Age past which a row's reply is dropped (milliseconds).
    pub expire_after_ms: i64,
    /// Live rows kept before the oldest are expired.
    pub max_live_rows: i64,
    /// Age past which an EXPIRED row is deleted outright (milliseconds).
    pub delete_after_ms: i64,
    /// Expired tombstones kept before the oldest are deleted.
    pub max_expired_rows: i64,
}

impl Default for RetentionPolicy {
    /// D18's numbers: 7 days or 100k rows for the reply corpus, and a second,
    /// wider bound on the tombstones that outlive it.
    fn default() -> Self {
        Self {
            expire_after_ms: 7 * 24 * 60 * 60 * 1000,
            max_live_rows: 100_000,
            delete_after_ms: 14 * 24 * 60 * 60 * 1000,
            max_expired_rows: 100_000,
        }
    }
}

/// What one retention pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Rows whose reply was dropped, leaving an answerable tombstone.
    pub expired: u64,
    /// Tombstones deleted outright.
    pub deleted: u64,
}

/// Stateless typed wrapper over `mutation_ledger`.
pub struct MutationLedgerRepo;

/// Canonical JSON for fingerprinting: object keys sorted, recursively.
///
/// The fingerprint decides `adopted` against `rejected{already_answered_by}`,
/// so two clients that serialize the same answer with different key order must
/// not read as two different answers.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

fn row_from(row: &sqlx::sqlite::SqliteRow) -> LedgerRow {
    LedgerRow {
        key: LedgerKey {
            host_id: row.get("host_id"),
            principal: row.get("principal"),
            op_id: row.get("op_id"),
        },
        method: row.get("method"),
        body_fingerprint: row.get("body_fingerprint"),
        tier: row.get("tier"),
        status: row.get("status"),
        reason: row.get("reason"),
        reply: row.get("reply"),
        receipt_state: row.get("receipt_state"),
        receipt_detail: row.get("receipt_detail"),
        expired: row.get::<i64, _>("expired") != 0,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

const SELECT_COLUMNS: &str = "host_id, principal, op_id, method, body_fingerprint, tier, \
     status, reason, reply, receipt_state, receipt_detail, expired, created_at, updated_at";

impl MutationLedgerRepo {
    /// Claim `key` for `method`, or report what already holds it.
    ///
    /// The foreign check runs FIRST, and that ordering is the point: the key
    /// includes the principal, so a row minted by a different device would not
    /// collide on insert and this caller would quietly execute under somebody
    /// else's op id (amendment 15).
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn claim_on(
        conn: &mut SqliteConnection,
        key: &LedgerKey,
        method: &str,
        body_fingerprint: &str,
        tier: &str,
        now_ms: i64,
    ) -> Result<ClaimOutcome, sqlx::Error> {
        if let Some(owner) = Self::holder_of_on(&mut *conn, &key.host_id, &key.op_id).await? {
            if owner.key.principal != key.principal {
                return Ok(ClaimOutcome::Foreign {
                    principal: owner.key.principal,
                });
            }
        }

        let receipt_state = (tier == TIER_RECEIPT).then_some("claimed");
        let inserted = sqlx::query(
            "INSERT INTO mutation_ledger \
             (host_id, principal, op_id, method, body_fingerprint, tier, status, \
              reply, receipt_state, expired, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'in_flight', NULL, ?, 0, ?, ?) \
             ON CONFLICT (host_id, principal, op_id) DO NOTHING",
        )
        .bind(&key.host_id)
        .bind(&key.principal)
        .bind(&key.op_id)
        .bind(method)
        .bind(body_fingerprint)
        .bind(tier)
        .bind(receipt_state)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&mut *conn)
        .await?;
        if inserted.rows_affected() > 0 {
            return Ok(ClaimOutcome::Fresh);
        }

        let Some(existing) = Self::get_on(&mut *conn, key).await? else {
            // The row vanished between the conflict and the read: a concurrent
            // retention sweep. Read as expired, which is the honest answer and
            // never a second execution.
            return Ok(ClaimOutcome::Expired(Box::new(LedgerRow {
                key: key.clone(),
                method: method.to_string(),
                body_fingerprint: body_fingerprint.to_string(),
                tier: tier.to_string(),
                status: STATUS_UNKNOWN.to_string(),
                reason: None,
                reply: None,
                receipt_state: None,
                receipt_detail: None,
                expired: true,
                created_at: now_ms,
                updated_at: now_ms,
            })));
        };
        if existing.expired {
            return Ok(ClaimOutcome::Expired(Box::new(existing)));
        }
        if existing.method != method || existing.body_fingerprint != body_fingerprint {
            return Ok(ClaimOutcome::BodyMismatch(Box::new(existing)));
        }
        if existing.status == STATUS_IN_FLIGHT {
            return Ok(ClaimOutcome::InFlight(Box::new(existing)));
        }
        Ok(ClaimOutcome::Replay(Box::new(existing)))
    }

    /// Read one row by its full key.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn get_on(
        conn: &mut SqliteConnection,
        key: &LedgerKey,
    ) -> Result<Option<LedgerRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM mutation_ledger \
             WHERE host_id = ? AND principal = ? AND op_id = ?"
        );
        Ok(sqlx::query(&sql)
            .bind(&key.host_id)
            .bind(&key.principal)
            .bind(&key.op_id)
            .fetch_optional(conn)
            .await?
            .as_ref()
            .map(row_from))
    }

    /// Read the row holding `op_id` on `host_id` under ANY principal.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn holder_of_on(
        conn: &mut SqliteConnection,
        host_id: &str,
        op_id: &str,
    ) -> Result<Option<LedgerRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM mutation_ledger \
             WHERE host_id = ? AND op_id = ? LIMIT 1"
        );
        Ok(sqlx::query(&sql)
            .bind(host_id)
            .bind(op_id)
            .fetch_optional(conn)
            .await?
            .as_ref()
            .map(row_from))
    }

    /// Record the terminal outcome and the reply a replay will serve.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn record_reply_on(
        conn: &mut SqliteConnection,
        key: &LedgerKey,
        status: &str,
        reason: Option<&str>,
        reply: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE mutation_ledger \
             SET status = ?, reason = ?, reply = ?, updated_at = ? \
             WHERE host_id = ? AND principal = ? AND op_id = ?",
        )
        .bind(status)
        .bind(reason)
        .bind(reply)
        .bind(now_ms)
        .bind(&key.host_id)
        .bind(&key.principal)
        .bind(&key.op_id)
        .execute(conn)
        .await?;
        Ok(res.rows_affected())
    }

    /// Move the receipt lifecycle on.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn set_receipt_on(
        conn: &mut SqliteConnection,
        key: &LedgerKey,
        receipt_state: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE mutation_ledger \
             SET receipt_state = ?, receipt_detail = COALESCE(?, receipt_detail), \
                 updated_at = ? \
             WHERE host_id = ? AND principal = ? AND op_id = ?",
        )
        .bind(receipt_state)
        .bind(detail)
        .bind(now_ms)
        .bind(&key.host_id)
        .bind(&key.principal)
        .bind(&key.op_id)
        .execute(conn)
        .await?;
        Ok(res.rows_affected())
    }

    /// Drop a claim whose handler produced no terminal outcome.
    ///
    /// Used only where re-execution is provably safe: the handler failed
    /// BEFORE touching any state (a parameter the dispatcher rejected, a store
    /// fault on the very first read). Anything past that point resolves to
    /// `unknown`, never to a deleted claim.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn abandon_on(
        conn: &mut SqliteConnection,
        key: &LedgerKey,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM mutation_ledger \
             WHERE host_id = ? AND principal = ? AND op_id = ? AND status = 'in_flight'",
        )
        .bind(&key.host_id)
        .bind(&key.principal)
        .bind(&key.op_id)
        .execute(conn)
        .await?;
        Ok(res.rows_affected())
    }
}

/// The pool-borrowing forms every caller outside a transaction uses.
///
/// Each acquires one connection and delegates to the `_on` core, so the SQL
/// exists once and the transactional and non-transactional paths cannot drift.
impl MutationLedgerRepo {
    /// [`Self::claim_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn claim(
        pool: &SqlitePool,
        key: &LedgerKey,
        method: &str,
        body_fingerprint: &str,
        tier: &str,
        now_ms: i64,
    ) -> Result<ClaimOutcome, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::claim_on(&mut conn, key, method, body_fingerprint, tier, now_ms).await
    }

    /// [`Self::get_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn get(pool: &SqlitePool, key: &LedgerKey) -> Result<Option<LedgerRow>, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::get_on(&mut conn, key).await
    }

    /// [`Self::holder_of_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn holder_of(
        pool: &SqlitePool,
        host_id: &str,
        op_id: &str,
    ) -> Result<Option<LedgerRow>, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::holder_of_on(&mut conn, host_id, op_id).await
    }

    /// [`Self::record_reply_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn record_reply(
        pool: &SqlitePool,
        key: &LedgerKey,
        status: &str,
        reason: Option<&str>,
        reply: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::record_reply_on(&mut conn, key, status, reason, reply, now_ms).await
    }

    /// [`Self::set_receipt_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn set_receipt(
        pool: &SqlitePool,
        key: &LedgerKey,
        receipt_state: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::set_receipt_on(&mut conn, key, receipt_state, detail, now_ms).await
    }

    /// [`Self::abandon_on`] against the pool.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn abandon(pool: &SqlitePool, key: &LedgerKey) -> Result<u64, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::abandon_on(&mut conn, key).await
    }
}

impl MutationLedgerRepo {
    /// The canonical fingerprint of a mutation body.
    ///
    /// The ENVELOPE is excluded: `op_id` and `fence` are how the operation is
    /// identified and guarded, not what it asks for, so a retry that re-sends
    /// the same answer with a refreshed fence is still the same body.
    #[must_use]
    pub fn fingerprint(method: &str, body: &Value) -> String {
        let mut stripped = body.clone();
        if let Some(map) = stripped.as_object_mut() {
            map.remove("op_id");
            map.remove("fence");
        }
        let mut hasher = Hasher::new();
        hasher.update(method.as_bytes());
        hasher.update(b"\0");
        hasher.update(canonical(&stripped).as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Claim inside a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn claim_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        key: &LedgerKey,
        method: &str,
        body_fingerprint: &str,
        tier: &str,
        now_ms: i64,
    ) -> Result<ClaimOutcome, sqlx::Error> {
        Self::claim_on(&mut *tx, key, method, body_fingerprint, tier, now_ms).await
    }

    /// Read one row inside a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn get_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        key: &LedgerKey,
    ) -> Result<Option<LedgerRow>, sqlx::Error> {
        Self::get_on(&mut *tx, key).await
    }

    /// Record the terminal outcome inside a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn record_reply_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        key: &LedgerKey,
        status: &str,
        reason: Option<&str>,
        reply: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        Self::record_reply_on(&mut *tx, key, status, reason, reply, now_ms).await
    }

    /// Move the receipt lifecycle on inside a caller-owned transaction — the
    /// tier-2 write boundary.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn set_receipt_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        key: &LedgerKey,
        receipt_state: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        Self::set_receipt_on(&mut *tx, key, receipt_state, detail, now_ms).await
    }

    /// Every row that never reached a terminal outcome, oldest first.
    ///
    /// Two shapes, one meaning: a `writing` receipt (bytes may have reached a
    /// PTY) and an `in_flight` claim of any tier (the handler may have
    /// committed). Both are `unknown{effects_ambiguous}` and neither may be
    /// re-executed.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn unresolved_at_boot(
        pool: &SqlitePool,
        host_id: &str,
    ) -> Result<Vec<LedgerRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM mutation_ledger \
             WHERE host_id = ? AND (status = 'in_flight' OR receipt_state = 'writing') \
             ORDER BY created_at ASC, op_id ASC"
        );
        Ok(sqlx::query(&sql)
            .bind(host_id)
            .fetch_all(pool)
            .await?
            .iter()
            .map(row_from)
            .collect())
    }

    /// Resolve one unresolved row to `unknown`, keeping its receipt evidence.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn resolve_unknown(
        pool: &SqlitePool,
        key: &LedgerKey,
        reason: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE mutation_ledger \
             SET status = 'unknown', reason = ?, \
                 receipt_state = CASE WHEN receipt_state IS NULL THEN NULL ELSE 'unknown' END, \
                 receipt_detail = COALESCE(?, receipt_detail), updated_at = ? \
             WHERE host_id = ? AND principal = ? AND op_id = ? \
               AND (status = 'in_flight' OR receipt_state = 'writing')",
        )
        .bind(reason)
        .bind(detail)
        .bind(now_ms)
        .bind(&key.host_id)
        .bind(&key.principal)
        .bind(&key.op_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Apply retention: expire old replies, then delete old tombstones.
    ///
    /// Two stages because D18 wants both a bound on storage AND an answerable
    /// `unknown{op_expired}` for a retry that arrives late. A single DELETE
    /// gives the first and silently breaks the second: once the key is gone the
    /// daemon cannot tell a stale retry from a new operation, and executes it.
    ///
    /// # Errors
    ///
    /// Propagates the `SQLite` failure.
    pub async fn retain(
        pool: &SqlitePool,
        host_id: &str,
        now_ms: i64,
        policy: RetentionPolicy,
    ) -> Result<RetentionReport, sqlx::Error> {
        let by_age = sqlx::query(
            "UPDATE mutation_ledger SET expired = 1, reply = NULL, updated_at = ? \
             WHERE host_id = ? AND expired = 0 AND created_at < ?",
        )
        .bind(now_ms)
        .bind(host_id)
        .bind(now_ms.saturating_sub(policy.expire_after_ms))
        .execute(pool)
        .await?
        .rows_affected();

        let by_count = sqlx::query(
            "UPDATE mutation_ledger SET expired = 1, reply = NULL, updated_at = ? \
             WHERE host_id = ? AND expired = 0 AND rowid IN ( \
                 SELECT rowid FROM mutation_ledger WHERE host_id = ? AND expired = 0 \
                 ORDER BY created_at DESC LIMIT -1 OFFSET ? \
             )",
        )
        .bind(now_ms)
        .bind(host_id)
        .bind(host_id)
        .bind(policy.max_live_rows)
        .execute(pool)
        .await?
        .rows_affected();

        let deleted_by_age = sqlx::query(
            "DELETE FROM mutation_ledger \
             WHERE host_id = ? AND expired = 1 AND created_at < ?",
        )
        .bind(host_id)
        .bind(now_ms.saturating_sub(policy.delete_after_ms))
        .execute(pool)
        .await?
        .rows_affected();

        let deleted_by_count = sqlx::query(
            "DELETE FROM mutation_ledger WHERE host_id = ? AND expired = 1 AND rowid IN ( \
                 SELECT rowid FROM mutation_ledger WHERE host_id = ? AND expired = 1 \
                 ORDER BY created_at DESC LIMIT -1 OFFSET ? \
             )",
        )
        .bind(host_id)
        .bind(host_id)
        .bind(policy.max_expired_rows)
        .execute(pool)
        .await?
        .rows_affected();

        Ok(RetentionReport {
            expired: by_age + by_count,
            deleted: deleted_by_age + deleted_by_count,
        })
    }
}
