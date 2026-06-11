//! Typed repository wrapper over the `agent` table.
//!
//! [`AgentRepo`] is a thin, stateless sqlx wrapper.
//!
//! Every method takes a [`SqlitePool`] borrow so callers share the single
//! [`crate::Store`] pool. The row model [`Agent`] mirrors the
//! `0002_agent_runtime_skill.sql` columns.
//!
//! # FK invariant
//!
//! `agent.runtime_id` is **required** (NOT NULL): per the Multica pattern an
//! agent always binds to exactly one [`crate::repo::agent_runtime`] row, since
//! an agent with nowhere to run is meaningless. Inserting an [`Agent`] whose
//! `runtime_id` (or `workspace_id`/`owner_id`) does not reference an existing
//! row fails at the `SQLite` foreign-key boundary.

use sqlx::SqlitePool;

/// An agent: a named, instruction-carrying actor bound to a runtime.
///
/// Field meanings track the `agent` table columns one-to-one. `instructions`
/// is the agent's free-form system prompt (nullable). `visibility` is one of
/// `"workspace"` or `"private"` (enforced by a `CHECK` constraint in the
/// schema, not by this type at v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Human-readable agent name.
    pub name: String,
    /// Runtime this agent runs on (`agent_runtime.id`). Required by the Multica
    /// pattern — never empty/absent.
    pub runtime_id: String,
    /// Free-form system prompt / instructions; `None` when unset.
    pub instructions: Option<String>,
    /// Visibility scope: `"workspace"` or `"private"`.
    pub visibility: String,
    /// Owning user (`user.id`).
    pub owner_id: String,
}

/// Stateless typed wrapper over the `agent` table.
pub struct AgentRepo;

impl AgentRepo {
    /// Insert one [`Agent`] row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — most commonly a foreign
    /// key violation (`runtime_id`/`workspace_id`/`owner_id` missing) or a
    /// `CHECK` violation on `visibility`.
    pub async fn insert(pool: &SqlitePool, agent: &Agent) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent \
             (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&agent.id)
        .bind(&agent.workspace_id)
        .bind(&agent.name)
        .bind(&agent.runtime_id)
        .bind(&agent.instructions)
        .bind(&agent.visibility)
        .bind(&agent.owner_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one [`Agent`] by primary key, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query itself fails (a missing row is
    /// `Ok(None)`, not an error).
    pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Agent>, sqlx::Error> {
        sqlx::query_as::<_, Agent>(
            "SELECT id, workspace_id, name, runtime_id, instructions, visibility, owner_id \
             FROM agent WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    /// List all agents in a workspace, ordered by `name`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<Agent>, sqlx::Error> {
        sqlx::query_as::<_, Agent>(
            "SELECT id, workspace_id, name, runtime_id, instructions, visibility, owner_id \
             FROM agent WHERE workspace_id = ? ORDER BY name",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for Agent {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            name: row.try_get("name")?,
            runtime_id: row.try_get("runtime_id")?,
            instructions: row.try_get("instructions")?,
            visibility: row.try_get("visibility")?,
            owner_id: row.try_get("owner_id")?,
        })
    }
}
