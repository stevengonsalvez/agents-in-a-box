//! ACP session identity: the ACP-specific adjunct to `fleet_session`.
//!
//! `session_key` is daemon-minted and STABLE (`acp:<ulid>`); `acp_session_id`
//! is the adapter's MUTABLE id, swapped whenever the resume routine rebuilds
//! the adapter-side session. Receipts, scopes, and `fleet/action` all key on
//! `session_key`, so a rebuild never disturbs them.
//!
//! At most ONE live (`ACTIVE` or `IDLE`) session exists per scope, enforced by
//! the partial unique index `idx_fleet_acp_session_scope_active`; a
//! conflicting insert returns the existing live row, which is what makes
//! `fleet/acp_session_create` idempotent per live scope.
//!
//! `provider` is an adapter token validated against the adapter registry at
//! the RPC layer, NOT the schema (the 0071 `source` style), so the next
//! adapter needs no migration.

use ainb_hangar_core::idgen::IdGen;
use sqlx::{Row, SqlitePool};

/// One ACP session to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetAcpSession {
    /// Daemon-minted stable key (`acp:<ulid>`, see [`FleetAcpSessionRepo::mint_session_key`]).
    pub session_key: String,
    /// The scope this session serves (`session:<session_key>` by default).
    pub scope_key: String,
    /// Adapter token, for example `claude-agent-acp` or `codex-acp`.
    pub provider: String,
    /// Working directory handed to the adapter.
    pub cwd: String,
    /// The permission mode PINNED at `session/new`; never inherited.
    pub permission_mode: String,
    /// Initial lifecycle state (`IDLE` before the first spawn).
    pub state: String,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
    /// Last-activity time in epoch milliseconds.
    pub last_active_at: i64,
}

/// Persisted ACP session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetAcpSessionRow {
    /// Stable daemon-minted key.
    pub session_key: String,
    /// Scope the session serves.
    pub scope_key: String,
    /// Adapter token.
    pub provider: String,
    /// `agentInfo` version observed at the last successful initialize.
    pub provider_version: Option<String>,
    /// Adapter-owned session id; `None` until `session/new` succeeds.
    pub acp_session_id: Option<String>,
    /// Working directory.
    pub cwd: String,
    /// Pinned permission mode.
    pub permission_mode: String,
    /// `ACTIVE`, `IDLE`, `EVICTED`, or `DEAD`.
    pub state: String,
    /// Non-`None` while a turn is in flight.
    pub open_turn_id: Option<String>,
    /// When the open turn started (the deadline-sweep input).
    pub open_turn_started_at: Option<i64>,
    /// Creation time.
    pub created_at: i64,
    /// Last-activity time.
    pub last_active_at: i64,
}

/// One session's per-session adapter override (migration 0080).
///
/// Model, reasoning effort and persona ONLY. The permission mode is absent by
/// design and not by omission: part 1 pins it at `session/new` and re-asserts
/// it after load because an ambient `bypassPermissions` silently disables the
/// entire permission surface, so an overridable mode would be a remote
/// guardrail off-switch reachable by anyone holding `fleet.copilot.configure`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetAcpSessionConfig {
    /// Adapter model id; `None` means the daemon's static config.
    pub model: Option<String>,
    /// Adapter reasoning-effort token; `None` means the daemon's static config.
    pub reasoning_effort: Option<String>,
    /// Operator-supplied system prompt; `None` means none is stored.
    pub persona: Option<String>,
}

/// ACP session persistence failures.
#[derive(Debug, thiserror::Error)]
pub enum FleetAcpSessionError {
    /// The requested session is absent.
    #[error("acp session {session_key:?} was not found")]
    SessionNotFound {
        /// Missing session key.
        session_key: String,
    },
    /// `SQLite` failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

const COLUMNS: &str = "session_key, scope_key, provider, provider_version, acp_session_id, \
     cwd, permission_mode, state, open_turn_id, open_turn_started_at, created_at, \
     last_active_at";

/// The write set ONE ended ACP turn commits together.
///
/// See [`FleetAcpSessionRepo::commit_turn_end`] for why these three writes
/// share a transaction rather than landing one at a time.
#[derive(Debug, Clone, Copy)]
pub struct TurnEnd<'a> {
    /// The session whose turn ended.
    pub session_key: &'a str,
    /// The prompt whose delivery leg this turn answers.
    pub message_id: &'a str,
    /// Receipt-claim fingerprint: the single-winner guard.
    pub fingerprint: &'a str,
    /// Terminal delivery state (`DELIVERED`, `FAILED`, `UNKNOWN`).
    pub state: &'a str,
    /// Enumerated outcome detail, `None` for a clean delivery.
    pub detail: Option<&'a str>,
    /// The lifecycle state the session returns to, normally `IDLE`.
    pub session_state: &'a str,
    /// The agent's reply for the timeline, when the turn produced one.
    pub reply: Option<&'a super::fleet_message::NewFleetMessage>,
    /// Commit time in epoch milliseconds.
    pub now: i64,
}

/// What [`FleetAcpSessionRepo::commit_turn_end`] committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndOutcome {
    /// Receipt, reply and released session all committed.
    Committed {
        /// The committed reply's cursor, when the turn produced one. The SEQ
        /// rather than the row: it is what wakes the message stream, and the
        /// row is already the caller's own `NewFleetMessage`.
        reply_seq: Option<i64>,
    },
    /// Another resolver already owns this leg's receipt (convergence beat the
    /// turn home). Only the session release was written: no reply reaches the
    /// timeline under a receipt that says the turn never landed.
    AlreadyResolved,
}

/// Typed access to `fleet_acp_session`.
pub struct FleetAcpSessionRepo;

impl FleetAcpSessionRepo {
    /// Mint a stable ACP session key: `acp:<ulid>`.
    pub fn mint_session_key(id_gen: &dyn IdGen) -> String {
        format!("acp:{}", id_gen.new_ulid().to_ascii_lowercase())
    }

    /// Insert one session, idempotent per LIVE scope: when the scope already
    /// holds an `ACTIVE`/`IDLE` session (the partial unique index fires), the
    /// existing live row is returned unchanged instead of an error.
    pub async fn insert(
        pool: &SqlitePool,
        session: &NewFleetAcpSession,
    ) -> Result<FleetAcpSessionRow, FleetAcpSessionError> {
        let inserted = sqlx::query(
            "INSERT INTO fleet_acp_session \
             (session_key, scope_key, provider, cwd, permission_mode, state, created_at, last_active_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.session_key)
        .bind(&session.scope_key)
        .bind(&session.provider)
        .bind(&session.cwd)
        .bind(&session.permission_mode)
        .bind(&session.state)
        .bind(session.created_at)
        .bind(session.last_active_at)
        .execute(pool)
        .await;
        match inserted {
            Ok(_) => Self::get(pool, &session.session_key).await?.ok_or_else(|| {
                FleetAcpSessionError::SessionNotFound {
                    session_key: session.session_key.clone(),
                }
            }),
            Err(e) if is_unique_violation(&e) => {
                // The one-live-session-per-scope index fired: hand back the
                // scope's existing live row (acp_session_create idempotency).
                Self::get_live_by_scope(pool, &session.scope_key)
                    .await?
                    .ok_or(FleetAcpSessionError::Sql(e))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Fetch one session by its stable key.
    pub async fn get(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Option<FleetAcpSessionRow>, sqlx::Error> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fleet_acp_session WHERE session_key = ?"
        ))
        .bind(session_key)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(row_from)
        .transpose()
    }

    /// Fetch the scope's live (`ACTIVE`/`IDLE`) session, if any.
    pub async fn get_live_by_scope(
        pool: &SqlitePool,
        scope_key: &str,
    ) -> Result<Option<FleetAcpSessionRow>, sqlx::Error> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fleet_acp_session \
             WHERE scope_key = ? AND state IN ('ACTIVE','IDLE')"
        ))
        .bind(scope_key)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(row_from)
        .transpose()
    }

    /// Swap the adapter-owned session id (rebuild keeps `session_key` stable).
    pub async fn set_acp_session_id(
        pool: &SqlitePool,
        session_key: &str,
        acp_session_id: Option<&str>,
    ) -> Result<(), FleetAcpSessionError> {
        Self::touch(
            pool,
            session_key,
            "UPDATE fleet_acp_session SET acp_session_id = ? WHERE session_key = ?",
            acp_session_id,
        )
        .await
    }

    /// Record the `agentInfo` version observed at a successful initialize, so
    /// a later resume can tell whether the adapter drifted underneath it.
    pub async fn set_provider_version(
        pool: &SqlitePool,
        session_key: &str,
        provider_version: &str,
    ) -> Result<(), FleetAcpSessionError> {
        Self::touch(
            pool,
            session_key,
            "UPDATE fleet_acp_session SET provider_version = ? WHERE session_key = ?",
            Some(provider_version),
        )
        .await
    }

    /// Move the session to a new lifecycle state, stamping `last_active_at`.
    pub async fn set_state(
        pool: &SqlitePool,
        session_key: &str,
        state: &str,
        now: i64,
    ) -> Result<(), FleetAcpSessionError> {
        let result = sqlx::query(
            "UPDATE fleet_acp_session SET state = ?, last_active_at = ? WHERE session_key = ?",
        )
        .bind(state)
        .bind(now)
        .bind(session_key)
        .execute(pool)
        .await?;
        Self::require_hit(result.rows_affected(), session_key)
    }

    /// Open a turn: records the turn id and stamps `open_turn_started_at`
    /// (the deadline-sweep input) plus `last_active_at`.
    pub async fn set_open_turn(
        pool: &SqlitePool,
        session_key: &str,
        turn_id: &str,
        now: i64,
    ) -> Result<(), FleetAcpSessionError> {
        let result = sqlx::query(
            "UPDATE fleet_acp_session \
             SET open_turn_id = ?, open_turn_started_at = ?, last_active_at = ? \
             WHERE session_key = ?",
        )
        .bind(turn_id)
        .bind(now)
        .bind(now)
        .bind(session_key)
        .execute(pool)
        .await?;
        Self::require_hit(result.rows_affected(), session_key)
    }

    /// Close the open turn: clears both turn columns and stamps
    /// `last_active_at`. Idempotent on an already-clear session.
    pub async fn clear_open_turn(
        pool: &SqlitePool,
        session_key: &str,
        now: i64,
    ) -> Result<(), FleetAcpSessionError> {
        let result = sqlx::query(
            "UPDATE fleet_acp_session \
             SET open_turn_id = NULL, open_turn_started_at = NULL, last_active_at = ? \
             WHERE session_key = ?",
        )
        .bind(now)
        .bind(session_key)
        .execute(pool)
        .await?;
        Self::require_hit(result.rows_affected(), session_key)
    }

    /// Read one session's per-session adapter override (migration 0080).
    ///
    /// `None` on every field means "no override": the resume path keeps the
    /// daemon's static adapter config, which is what every pre-0080 row says.
    pub async fn get_config(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Option<FleetAcpSessionConfig>, sqlx::Error> {
        sqlx::query(
            "SELECT model, reasoning_effort, persona FROM fleet_acp_session \
             WHERE session_key = ?",
        )
        .bind(session_key)
        .fetch_optional(pool)
        .await?
        .map(|row| {
            Ok(FleetAcpSessionConfig {
                model: row.try_get("model")?,
                reasoning_effort: row.try_get("reasoning_effort")?,
                persona: row.try_get("persona")?,
            })
        })
        .transpose()
    }

    /// Write one session's per-session adapter override (migration 0080).
    ///
    /// The three 0080 columns and NOTHING else: `permission_mode` is 0079's
    /// column and is deliberately not reachable from here, because a settable
    /// mode is a remote off-switch for the whole permission surface. There is
    /// no argument for it in this signature precisely so no future caller can
    /// pass one by accident.
    ///
    /// A `None` field CLEARS that override rather than leaving the previous
    /// value: `configure` states the full override, so an omitted model means
    /// "back to the daemon's static config", never "keep whatever was there".
    pub async fn set_config(
        pool: &SqlitePool,
        session_key: &str,
        config: &FleetAcpSessionConfig,
        now: i64,
    ) -> Result<(), FleetAcpSessionError> {
        let result = sqlx::query(
            "UPDATE fleet_acp_session \
             SET model = ?, reasoning_effort = ?, persona = ?, last_active_at = ? \
             WHERE session_key = ?",
        )
        .bind(&config.model)
        .bind(&config.reasoning_effort)
        .bind(&config.persona)
        .bind(now)
        .bind(session_key)
        .execute(pool)
        .await?;
        Self::require_hit(result.rows_affected(), session_key)
    }

    /// The ONE dirty-session query shared by the boot scan and the runtime
    /// convergence path: sessions with an open turn, or with any `PENDING`
    /// delivery leg still unresolved.
    pub async fn list_dirty(pool: &SqlitePool) -> Result<Vec<FleetAcpSessionRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fleet_acp_session s \
             WHERE s.open_turn_id IS NOT NULL \
                OR EXISTS (SELECT 1 FROM fleet_message_delivery d \
                           WHERE d.session_key = s.session_key AND d.state = 'PENDING') \
             ORDER BY s.session_key ASC"
        ))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// Sessions whose open turn started at or before `cutoff_ms` (the turn
    /// deadline sweep: caller computes `now - deadline` and cancels each hit).
    pub async fn list_open_turns_older_than(
        pool: &SqlitePool,
        cutoff_ms: i64,
    ) -> Result<Vec<FleetAcpSessionRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fleet_acp_session \
             WHERE open_turn_id IS NOT NULL AND open_turn_started_at <= ? \
             ORDER BY open_turn_started_at ASC"
        ))
        .bind(cutoff_ms)
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    async fn touch(
        pool: &SqlitePool,
        session_key: &str,
        sql: &str,
        value: Option<&str>,
    ) -> Result<(), FleetAcpSessionError> {
        let result = sqlx::query(sql).bind(value).bind(session_key).execute(pool).await?;
        Self::require_hit(result.rows_affected(), session_key)
    }

    /// Commit everything ONE ended ACP turn changes, in ONE transaction.
    ///
    /// A turn ends by writing four things: the transcript's completion marker,
    /// the delivery receipt, the agent's reply on the timeline, and the
    /// session's cleared `open_turn_id`. Committed separately, a daemon that
    /// died between them left states nothing repairs: a reply on the timeline
    /// whose receipt still said PENDING (convergence then resolves it UNKNOWN,
    /// so the operator reads a delivered answer under a failed receipt), or a
    /// resolved receipt for a session still marked mid-turn. The three writes
    /// that share a store transaction are joined here; the transcript marker is
    /// a different table with its own writer and is committed BEFORE this, so
    /// the only surviving crash window leaves a turn the boot scan still sees
    /// as dirty and converges.
    ///
    /// The leg is claimed and resolved by the SAME predicate the cross-process
    /// resolvers use, so a turn whose receipt convergence already took over
    /// commits [`TurnEndOutcome::AlreadyResolved`] and its reply is NOT
    /// written: a timeline answer whose receipt says UNKNOWN cannot be
    /// corrected afterwards, because the claim is spent. The session is still
    /// released in that case, so a lost race cannot leave a scope wedged.
    pub async fn commit_turn_end(
        pool: &SqlitePool,
        turn: &TurnEnd<'_>,
    ) -> Result<TurnEndOutcome, FleetAcpSessionError> {
        let mut tx = pool.begin().await?;
        let claimed = super::fleet_message::FleetMessageRepo::resolve_pending_in_tx(
            &mut tx,
            turn.message_id,
            turn.session_key,
            turn.fingerprint,
            turn.state,
            turn.detail,
            turn.now,
        )
        .await?;
        let reply_seq = match turn.reply.filter(|_| claimed) {
            Some(reply) => Some(
                super::fleet_message::FleetMessageRepo::insert_in_tx(&mut tx, reply).await?.seq,
            ),
            None => None,
        };
        sqlx::query(
            "UPDATE fleet_acp_session \
             SET open_turn_id = NULL, open_turn_started_at = NULL, state = ?, last_active_at = ? \
             WHERE session_key = ?",
        )
        .bind(turn.session_state)
        .bind(turn.now)
        .bind(turn.session_key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(if claimed {
            TurnEndOutcome::Committed { reply_seq }
        } else {
            TurnEndOutcome::AlreadyResolved
        })
    }

    /// Insert the `fleet_acp_session` row AND its `fleet_session` twin in ONE
    /// transaction, per the plan's Session identity rule.
    ///
    /// An ACP session is one identity spread over two tables: `fleet_session`
    /// is what the snapshot, attention rows, receipts and `fleet/action` key
    /// on, and this table is the ACP-specific adjunct. Writing them in two
    /// transactions would let a crash in the gap leave either a Fleet session
    /// no pool can drive or an ACP row no surface can see, and there is no
    /// reconciler for either shape.
    ///
    /// Idempotent per LIVE scope, exactly like [`FleetAcpSessionRepo::insert`]:
    /// when the scope already holds an `ACTIVE`/`IDLE` session the whole
    /// transaction rolls back and the existing row is returned, so a replayed
    /// `fleet/acp_session_create` never mints a second Fleet session either.
    pub async fn insert_with_fleet_session(
        pool: &SqlitePool,
        session: &NewFleetAcpSession,
        event: &super::fleet::NewFleetEvent,
    ) -> Result<(FleetAcpSessionRow, Option<i64>), FleetAcpSessionError> {
        let mut tx = pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO fleet_acp_session \
             (session_key, scope_key, provider, cwd, permission_mode, state, created_at, last_active_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.session_key)
        .bind(&session.scope_key)
        .bind(&session.provider)
        .bind(&session.cwd)
        .bind(&session.permission_mode)
        .bind(&session.state)
        .bind(session.created_at)
        .bind(session.last_active_at)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                drop(tx);
                let existing = Self::get_live_by_scope(pool, &session.scope_key)
                    .await?
                    .ok_or(FleetAcpSessionError::Sql(e))?;
                return Ok((existing, None));
            }
            Err(e) => return Err(e.into()),
        }
        let applied = super::fleet::FleetRepo::apply_event_in_tx(&mut tx, event, None)
            .await
            .map_err(|error| match error {
                super::fleet::FleetRepoError::Sql(sql) => FleetAcpSessionError::Sql(sql),
                other => FleetAcpSessionError::Sql(sqlx::Error::Protocol(other.to_string())),
            })?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fleet_acp_session WHERE session_key = ?"
        ))
        .bind(&session.session_key)
        .fetch_optional(&mut *tx)
        .await?
        .as_ref()
        .map(row_from)
        .transpose()?
        .ok_or_else(|| FleetAcpSessionError::SessionNotFound {
            session_key: session.session_key.clone(),
        })?;
        tx.commit().await?;
        Ok((row, Some(applied.revision)))
    }

    fn require_hit(rows_affected: u64, session_key: &str) -> Result<(), FleetAcpSessionError> {
        if rows_affected == 0 {
            return Err(FleetAcpSessionError::SessionNotFound {
                session_key: session_key.to_string(),
            });
        }
        Ok(())
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}

fn row_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetAcpSessionRow, sqlx::Error> {
    Ok(FleetAcpSessionRow {
        session_key: row.try_get("session_key")?,
        scope_key: row.try_get("scope_key")?,
        provider: row.try_get("provider")?,
        provider_version: row.try_get("provider_version")?,
        acp_session_id: row.try_get("acp_session_id")?,
        cwd: row.try_get("cwd")?,
        permission_mode: row.try_get("permission_mode")?,
        state: row.try_get("state")?,
        open_turn_id: row.try_get("open_turn_id")?,
        open_turn_started_at: row.try_get("open_turn_started_at")?,
        created_at: row.try_get("created_at")?,
        last_active_at: row.try_get("last_active_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use ainb_hangar_core::idgen::SystemIdGen;

    fn session(key: &str, scope: &str) -> NewFleetAcpSession {
        NewFleetAcpSession {
            session_key: key.to_string(),
            scope_key: scope.to_string(),
            provider: "claude-agent-acp".to_string(),
            cwd: "/tmp/w".to_string(),
            permission_mode: "default".to_string(),
            state: "IDLE".to_string(),
            created_at: 100,
            last_active_at: 100,
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    #[test]
    fn minted_session_key_is_acp_prefixed() {
        let key = FleetAcpSessionRepo::mint_session_key(&SystemIdGen);
        assert!(key.starts_with("acp:"));
        assert_eq!(key.len(), "acp:".len() + 26, "ULID payload");
    }

    #[tokio::test]
    async fn insert_is_idempotent_per_live_scope() {
        let (_dir, store) = store().await;
        let first = FleetAcpSessionRepo::insert(store.pool(), &session("acp:1", "session:acp:1"))
            .await
            .unwrap();

        // A second live insert for the SAME scope returns the existing row.
        let conflicting =
            FleetAcpSessionRepo::insert(store.pool(), &session("acp:2", "session:acp:1"))
                .await
                .unwrap();
        assert_eq!(conflicting, first);
        assert!(
            FleetAcpSessionRepo::get(store.pool(), "acp:2").await.unwrap().is_none(),
            "the conflicting key was never written"
        );

        // Once the live row leaves the live states, the scope frees up.
        FleetAcpSessionRepo::set_state(store.pool(), "acp:1", "DEAD", 200)
            .await
            .unwrap();
        let replacement =
            FleetAcpSessionRepo::insert(store.pool(), &session("acp:3", "session:acp:1"))
                .await
                .unwrap();
        assert_eq!(replacement.session_key, "acp:3");
    }

    #[tokio::test]
    async fn live_scope_constraint_fires_at_the_schema_layer() {
        let (_dir, store) = store().await;
        FleetAcpSessionRepo::insert(store.pool(), &session("acp:1", "session:acp:1"))
            .await
            .unwrap();
        let err = sqlx::query(
            "INSERT INTO fleet_acp_session \
             (session_key, scope_key, provider, cwd, permission_mode, state, created_at, last_active_at) \
             VALUES ('acp:raw', 'session:acp:1', 'codex-acp', '/tmp', 'default', 'ACTIVE', 0, 0)",
        )
        .execute(store.pool())
        .await
        .expect_err("a second live session for the scope must be refused");
        assert!(super::is_unique_violation(&err), "got: {err}");
    }

    #[tokio::test]
    async fn setters_swap_adapter_identity_without_touching_the_key() {
        let (_dir, store) = store().await;
        FleetAcpSessionRepo::insert(store.pool(), &session("acp:1", "session:acp:1"))
            .await
            .unwrap();

        FleetAcpSessionRepo::set_acp_session_id(store.pool(), "acp:1", Some("adapter-a"))
            .await
            .unwrap();
        FleetAcpSessionRepo::set_provider_version(store.pool(), "acp:1", "0.64.0")
            .await
            .unwrap();
        // The rebuild path: adapter id swaps, session_key stays.
        FleetAcpSessionRepo::set_acp_session_id(store.pool(), "acp:1", Some("adapter-b"))
            .await
            .unwrap();

        let row = FleetAcpSessionRepo::get(store.pool(), "acp:1").await.unwrap().unwrap();
        assert_eq!(row.acp_session_id.as_deref(), Some("adapter-b"));
        assert_eq!(row.provider_version.as_deref(), Some("0.64.0"));
        assert_eq!(row.session_key, "acp:1");

        assert!(matches!(
            FleetAcpSessionRepo::set_provider_version(store.pool(), "acp:missing", "1.0").await,
            Err(FleetAcpSessionError::SessionNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn open_turn_stamps_and_clears_both_columns() {
        let (_dir, store) = store().await;
        FleetAcpSessionRepo::insert(store.pool(), &session("acp:1", "session:acp:1"))
            .await
            .unwrap();

        FleetAcpSessionRepo::set_open_turn(store.pool(), "acp:1", "turn-1", 500)
            .await
            .unwrap();
        let open = FleetAcpSessionRepo::get(store.pool(), "acp:1").await.unwrap().unwrap();
        assert_eq!(open.open_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(open.open_turn_started_at, Some(500));
        assert_eq!(open.last_active_at, 500);

        FleetAcpSessionRepo::clear_open_turn(store.pool(), "acp:1", 600).await.unwrap();
        let closed = FleetAcpSessionRepo::get(store.pool(), "acp:1").await.unwrap().unwrap();
        assert_eq!(closed.open_turn_id, None);
        assert_eq!(closed.open_turn_started_at, None);
        assert_eq!(closed.last_active_at, 600);
    }

    #[tokio::test]
    async fn list_dirty_finds_open_turns_and_pending_deliveries() {
        let (_dir, store) = store().await;
        for (key, scope) in [
            ("acp:turn", "session:acp:turn"),
            ("acp:pending", "session:acp:pending"),
            ("acp:clean", "session:acp:clean"),
        ] {
            FleetAcpSessionRepo::insert(store.pool(), &session(key, scope)).await.unwrap();
        }
        FleetAcpSessionRepo::set_open_turn(store.pool(), "acp:turn", "turn-1", 500)
            .await
            .unwrap();
        crate::repo::fleet_message::FleetMessageRepo::insert_message(
            store.pool(),
            &crate::repo::fleet_message::NewFleetMessage {
                id: "msg-1".to_string(),
                request_id: None,
                request_fingerprint: None,
                scope_key: "session:acp:pending".to_string(),
                origin_message_id: None,
                sender: "operator".to_string(),
                kind: "user".to_string(),
                body: "hello".to_string(),
                created_at: 100,
            },
        )
        .await
        .unwrap();
        crate::repo::fleet_message::FleetMessageRepo::insert_delivery(
            store.pool(),
            "msg-1",
            "acp:pending",
        )
        .await
        .unwrap();

        let dirty = FleetAcpSessionRepo::list_dirty(store.pool()).await.unwrap();
        assert_eq!(
            dirty.iter().map(|s| s.session_key.as_str()).collect::<Vec<_>>(),
            vec!["acp:pending", "acp:turn"],
            "open turn OR pending delivery; the clean session stays out"
        );
    }

    #[tokio::test]
    async fn deadline_sweep_returns_only_expired_open_turns() {
        let (_dir, store) = store().await;
        for (key, scope) in [
            ("acp:old", "session:acp:old"),
            ("acp:fresh", "session:acp:fresh"),
        ] {
            FleetAcpSessionRepo::insert(store.pool(), &session(key, scope)).await.unwrap();
        }
        FleetAcpSessionRepo::set_open_turn(store.pool(), "acp:old", "turn-1", 1_000)
            .await
            .unwrap();
        FleetAcpSessionRepo::set_open_turn(store.pool(), "acp:fresh", "turn-2", 9_000)
            .await
            .unwrap();

        let expired = FleetAcpSessionRepo::list_open_turns_older_than(store.pool(), 5_000)
            .await
            .unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].session_key, "acp:old");
    }
}
