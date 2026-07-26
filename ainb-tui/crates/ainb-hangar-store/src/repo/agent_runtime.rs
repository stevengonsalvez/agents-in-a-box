//! Typed repository wrapper over the `agent_runtime` table.
//!
//! An [`AgentRuntime`] is a registered provider endpoint within a workspace.
//!
//! Concretely, a daemon advertising a single provider. The schema enforces one
//! row per `(workspace_id, daemon_id, provider)` via a unique index, so
//! re-registering the same tuple is a conflict rather than a duplicate.

use sqlx::SqlitePool;

/// A registered provider runtime (one daemon advertising one provider).
///
/// Fields track the `agent_runtime` columns one-to-one. `last_seen_at` is the
/// last heartbeat (epoch ms; `None` if never seen). `status` defaults to
/// `"offline"` in the schema; here it is always materialised explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntime {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Identifier of the daemon hosting this runtime.
    pub daemon_id: String,
    /// Provider name (e.g. `"claude"`, `"codex"`).
    pub provider: String,
    /// Runtime mode: `"local"` or `"cloud"`.
    pub runtime_mode: String,
    /// Last heartbeat timestamp (epoch ms); `None` if never seen.
    pub last_seen_at: Option<i64>,
    /// Liveness status (e.g. `"offline"`, `"online"`).
    pub status: String,
}

/// One runtime moved by [`AgentRuntimeRepo::sweep_presence_by_age`].
///
/// Carries the workspace alongside the id so the caller can fan an event out on
/// the right workspace stream without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweptRuntime {
    /// The runtime whose `status` changed.
    pub id: String,
    /// The runtime's owning workspace (`workspace.id`).
    pub workspace_id: String,
}

/// The outcome of one [`AgentRuntimeRepo::sweep_presence_by_age`] pass.
///
/// Both bands are reported separately (and by row, not just by count) because
/// the daemon fans an `AgentPresence` event out per flipped runtime and needs to
/// know which state each landed in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresenceSweep {
    /// Runtimes moved `online → unstable` (stale past the unstable threshold).
    pub to_unstable: Vec<SweptRuntime>,
    /// Runtimes moved `online|unstable → offline` (past the grace window).
    pub to_offline: Vec<SweptRuntime>,
}

impl PresenceSweep {
    /// Whether the pass moved nothing (the steady state — used to keep the
    /// sweeper's log quiet on an idle tick).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to_unstable.is_empty() && self.to_offline.is_empty()
    }
}

/// Stateless typed wrapper over the `agent_runtime` table.
pub struct AgentRuntimeRepo;

impl AgentRuntimeRepo {
    /// Insert one [`AgentRuntime`] row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on failure — most commonly a unique-index
    /// conflict on `(workspace_id, daemon_id, provider)`, a `CHECK` violation
    /// on `runtime_mode`, or a `workspace_id` FK violation.
    pub async fn insert(pool: &SqlitePool, runtime: &AgentRuntime) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent_runtime \
             (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&runtime.id)
        .bind(&runtime.workspace_id)
        .bind(&runtime.daemon_id)
        .bind(&runtime.provider)
        .bind(&runtime.runtime_mode)
        .bind(runtime.last_seen_at)
        .bind(&runtime.status)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one [`AgentRuntime`] by primary key, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query itself fails.
    pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<AgentRuntime>, sqlx::Error> {
        sqlx::query_as::<_, AgentRuntime>(
            "SELECT id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status \
             FROM agent_runtime WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    /// List all runtimes in a workspace, ordered by `provider` then `daemon_id`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<AgentRuntime>, sqlx::Error> {
        sqlx::query_as::<_, AgentRuntime>(
            "SELECT id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status \
             FROM agent_runtime WHERE workspace_id = ? ORDER BY provider, daemon_id",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }

    /// Refresh one runtime's heartbeat: stamp `last_seen_at = now_ms` and return
    /// the row to `"online"`.
    ///
    /// The status write is what lets a runtime that decayed to
    /// `"unstable"`/`"offline"` self-heal the moment its daemon comes back,
    /// without a separate re-register. Targeted by primary key, so a daemon can
    /// only ever beat for its OWN runtime and never revives a foreign one.
    /// Returns `true` when the row existed and was updated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn heartbeat(pool: &SqlitePool, id: &str, now_ms: i64) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE agent_runtime SET last_seen_at = ?, status = 'online' WHERE id = ?",
        )
        .bind(now_ms)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Age-based presence sweep over every runtime that carries a heartbeat.
    ///
    /// Two source-status-constrained `UPDATE`s — `online → unstable` inside the
    /// grace window, then `online|unstable → offline` past it — so the pass is
    /// idempotent: a second run over the same backlog moves nothing. A runtime
    /// with a `NULL` `last_seen_at` is never touched (it carries no liveness
    /// signal to age; legacy / CLI-seeded rows must not decay).
    ///
    /// Both thresholds are PARAMETERS, not literals, so the store never depends
    /// on the wire crate that defines them; the daemon passes
    /// `PresenceState::UNSTABLE_AFTER_MS` / `OFFLINE_AFTER_MS`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if either statement fails.
    pub async fn sweep_presence_by_age(
        pool: &SqlitePool,
        now_ms: i64,
        unstable_after_ms: i64,
        offline_after_ms: i64,
    ) -> Result<PresenceSweep, sqlx::Error> {
        // Offline first: the wider band is evaluated against the pre-sweep
        // statuses, so a row past the grace window lands `offline` directly
        // rather than being reported in both bands on one pass.
        let to_offline = sqlx::query_as::<_, SweptRuntime>(
            "UPDATE agent_runtime SET status = 'offline' \
             WHERE last_seen_at IS NOT NULL AND status IN ('online', 'unstable') \
               AND (?1 - last_seen_at) > ?2 \
             RETURNING id, workspace_id",
        )
        .bind(now_ms)
        .bind(offline_after_ms)
        .fetch_all(pool)
        .await?;

        let to_unstable = sqlx::query_as::<_, SweptRuntime>(
            "UPDATE agent_runtime SET status = 'unstable' \
             WHERE last_seen_at IS NOT NULL AND status = 'online' \
               AND (?1 - last_seen_at) > ?2 AND (?1 - last_seen_at) <= ?3 \
             RETURNING id, workspace_id",
        )
        .bind(now_ms)
        .bind(unstable_after_ms)
        .bind(offline_after_ms)
        .fetch_all(pool)
        .await?;

        Ok(PresenceSweep {
            to_unstable,
            to_offline,
        })
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for SweptRuntime {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
        })
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for AgentRuntime {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            daemon_id: row.try_get("daemon_id")?,
            provider: row.try_get("provider")?,
            runtime_mode: row.try_get("runtime_mode")?,
            last_seen_at: row.try_get("last_seen_at")?,
            status: row.try_get("status")?,
        })
    }
}
