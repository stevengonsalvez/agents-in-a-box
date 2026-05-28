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
