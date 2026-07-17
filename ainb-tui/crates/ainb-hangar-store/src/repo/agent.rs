//! Typed repository wrapper over the `agent` table.
//!
//! [`AgentRepo`] is a thin, stateless sqlx wrapper.
//!
//! Every method takes a [`SqlitePool`] borrow so callers share the single
//! [`crate::Store`] pool. The row model [`Agent`] mirrors the
//! `0002_agent_runtime_skill.sql` columns plus the `0015_agent_archive_and_config.sql`
//! archive flag + runtime-config knobs.
//!
//! # FK invariant
//!
//! `agent.runtime_id` is **required** (NOT NULL): per the reference pattern an
//! agent always binds to exactly one [`crate::repo::agent_runtime`] row, since
//! an agent with nowhere to run is meaningless. Inserting an [`Agent`] whose
//! `runtime_id` (or `workspace_id`/`owner_id`) does not reference an existing
//! row fails at the `SQLite` foreign-key boundary.
//!
//! # Config columns (migration 0015)
//!
//! The `cli_args` and `agent_env` columns are JSON in the database but typed in
//! the Rust API: `cli_args` is a `Vec<String>` (a JSON array) and `agent_env`
//! is a `Vec<(String, String)>` (a JSON object, kept as an ordered key-value
//! list so serialisation is deterministic). `model` / `thinking` / `mcp_config`
//! are nullable TEXT carried as `Option<String>`. **Persistence only** — the
//! provider EXEC consumption of these knobs is a separate bead (e38.16).

use sqlx::SqlitePool;

/// An agent: a named, instruction-carrying actor bound to a runtime.
///
/// Field meanings track the `agent` table columns one-to-one. `instructions`
/// is the agent's free-form system prompt (nullable). `visibility` is one of
/// `"workspace"` or `"private"` (enforced by a `CHECK` constraint in the
/// schema, not by this type at v1). The trailing config fields land with
/// migration 0015 (the e38.15 edit/archive surface): `archived` hides the agent
/// from the active picker; `model` / `cli_args` / `mcp_config` / `thinking` /
/// `agent_env` are the per-agent runtime knobs the daemon will consume in e38.16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Human-readable agent name.
    pub name: String,
    /// Runtime this agent runs on (`agent_runtime.id`). Required by the reference
    /// pattern — never empty/absent.
    pub runtime_id: String,
    /// Free-form system prompt / instructions; `None` when unset.
    pub instructions: Option<String>,
    /// Visibility scope: `"workspace"` or `"private"`.
    pub visibility: String,
    /// Owning user (`user.id`).
    pub owner_id: String,
    /// `true` when the agent is archived (hidden from the active picker). The
    /// schema stores this as a 0/1 INTEGER (migration 0015).
    pub archived: bool,
    /// Optional provider model override (e.g. `claude-opus-4`); `None` = the
    /// runtime's provider default.
    pub model: Option<String>,
    /// Extra provider CLI arguments (stored as a JSON-array `cli_args` column).
    pub cli_args: Vec<String>,
    /// The agent's MCP-server configuration as a raw JSON-object string; `None`
    /// when unset (the schema default `'{}'` reads back as `None` → no config).
    pub mcp_config: Option<String>,
    /// Optional reasoning/thinking level (e.g. `low`/`medium`/`high`); `None` =
    /// provider default.
    pub thinking: Option<String>,
    /// Per-agent environment variables (stored as a JSON-object `agent_env`
    /// column), kept as an ordered key-value list for deterministic encoding.
    pub agent_env: Vec<(String, String)>,
    /// Optional per-agent provider override (`"claude"`/`"codex"`/`"copilot"`),
    /// recorded at create time (migration 0041); `None` = fall back to the
    /// runtime's advertised provider. Honoured at dispatch: the agent binds the
    /// single default runtime (an execution slot claimed by id, not by provider),
    /// and the daemon spawns THIS provider's backend per task, so a `codex` agent
    /// runs codex.
    pub provider: Option<String>,
    /// Optional token budget (rtk/headroom) for this agent's runs; `None` =
    /// unlimited (migration 0042). Stored + surfaced only in this milestone —
    /// dispatch-time enforcement is a later feature.
    pub token_budget: Option<i64>,
}

/// A partial-edit instruction for one agent's mutable config (e38.15).
///
/// Each field is an `Option` of "leave unchanged" vs "set to this value". The
/// three nullable columns (`instructions`, `model`, `mcp_config`, `thinking`)
/// nest a second `Option` so the caller can distinguish *clear to NULL*
/// (`Some(None)`) from *leave unchanged* (`None`). The two non-null JSON columns
/// (`cli_args`, `agent_env`) and the non-null `name` use a single `Option`
/// (their "cleared" state is the empty collection, not NULL).
///
/// `Default` is all-`None` (a no-op edit); callers fill in only the fields they
/// touch (`AgentConfigUpdate { model: Some(Some("x".into())), ..Default::default() }`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentConfigUpdate {
    /// New name, or `None` to leave it unchanged.
    pub name: Option<String>,
    /// New instructions: `None` leaves it, `Some(None)` clears it,
    /// `Some(Some(_))` sets it.
    pub instructions: Option<Option<String>>,
    /// New model override: `None` leaves it, `Some(None)` clears it (back to the
    /// provider default), `Some(Some(_))` sets it.
    pub model: Option<Option<String>>,
    /// New CLI-args list, or `None` to leave it unchanged (an empty `Vec` is a
    /// valid "no args" value, distinct from leaving it).
    pub cli_args: Option<Vec<String>>,
    /// New MCP config: `None` leaves it, `Some(None)` clears it,
    /// `Some(Some(_))` sets the raw JSON-object string.
    pub mcp_config: Option<Option<String>>,
    /// New thinking level: `None` leaves it, `Some(None)` clears it,
    /// `Some(Some(_))` sets it.
    pub thinking: Option<Option<String>>,
    /// New per-agent env map, or `None` to leave it unchanged (an empty `Vec` is
    /// a valid "no env" value, distinct from leaving it).
    pub agent_env: Option<Vec<(String, String)>>,
    /// New token budget: `None` leaves it, `Some(None)` clears it (back to
    /// unlimited), `Some(Some(_))` sets it.
    pub token_budget: Option<Option<i64>>,
}

impl AgentConfigUpdate {
    /// `true` when no field is set, so [`AgentRepo::update_config`] would write
    /// nothing — the handler uses this to skip a pointless UPDATE.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.instructions.is_none()
            && self.model.is_none()
            && self.cli_args.is_none()
            && self.mcp_config.is_none()
            && self.thinking.is_none()
            && self.agent_env.is_none()
            && self.token_budget.is_none()
    }
}

/// Stateless typed wrapper over the `agent` table.
pub struct AgentRepo;

impl AgentRepo {
    /// Insert one [`Agent`] row, encoding the JSON config columns.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — most commonly a foreign
    /// key violation (`runtime_id`/`workspace_id`/`owner_id` missing) or a
    /// `CHECK` violation on `visibility`.
    pub async fn insert(pool: &SqlitePool, agent: &Agent) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent \
             (id, workspace_id, name, runtime_id, instructions, visibility, owner_id, \
              archived, model, cli_args, mcp_config, thinking, agent_env, provider, \
              token_budget) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&agent.id)
        .bind(&agent.workspace_id)
        .bind(&agent.name)
        .bind(&agent.runtime_id)
        .bind(&agent.instructions)
        .bind(&agent.visibility)
        .bind(&agent.owner_id)
        .bind(i64::from(agent.archived))
        .bind(&agent.model)
        .bind(cli_args_to_json(&agent.cli_args))
        .bind(agent.mcp_config.clone().unwrap_or_else(|| "{}".to_string()))
        .bind(&agent.thinking)
        .bind(env_to_json(&agent.agent_env))
        .bind(&agent.provider)
        .bind(agent.token_budget)
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
        sqlx::query_as::<_, Agent>(&format!("{SELECT_COLS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(pool)
            .await
    }

    /// List the **active** (non-archived) agents in a workspace, ordered by
    /// `name`.
    ///
    /// This is the picker-facing list: archived agents are deliberately excluded
    /// so a hidden agent never appears as an assignable actor. Use
    /// [`list_by_workspace_including_archived`](Self::list_by_workspace_including_archived)
    /// when the full roster (e.g. an admin/management view) is needed.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<Agent>, sqlx::Error> {
        sqlx::query_as::<_, Agent>(&format!(
            "{SELECT_COLS} WHERE workspace_id = ? AND archived = 0 ORDER BY name"
        ))
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }

    /// List **every** agent in a workspace — active and archived — ordered by
    /// `name`. The management/edit surface uses this to show archived rows.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace_including_archived(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<Agent>, sqlx::Error> {
        sqlx::query_as::<_, Agent>(&format!(
            "{SELECT_COLS} WHERE workspace_id = ? ORDER BY name"
        ))
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }

    /// Edit a subset of one agent's mutable config, scoped to a workspace
    /// (e38.15).
    ///
    /// Only the fields set in `update` are written; absent fields are left as-is.
    /// The nullable columns can be cleared (`Some(None)`) or set
    /// (`Some(Some(_))`). The write is **workspace-scoped**: the `WHERE` clause
    /// matches `(id, workspace_id)`, so an agent id from another tenant matches
    /// zero rows and changes nothing (a no-op, never a cross-tenant edit).
    ///
    /// Returns `true` when exactly one row was updated, `false` when the
    /// `(id, workspace_id)` pair matched no agent (a foreign tenant, an unknown
    /// id, or — defensively — an empty `update`, a deliberate no-op).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn update_config(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
        update: &AgentConfigUpdate,
    ) -> Result<bool, sqlx::Error> {
        // An empty edit is a deliberate no-op: building an UPDATE with no SET
        // clause would be invalid SQL, so short-circuit before constructing it.
        if update.is_empty() {
            return Ok(false);
        }

        // Build the SET list dynamically from only the present fields, binding
        // positionally in the same order so the `query.bind(...)` chain matches.
        let mut sets: Vec<&str> = Vec::new();
        if update.name.is_some() {
            sets.push("name = ?");
        }
        if update.instructions.is_some() {
            sets.push("instructions = ?");
        }
        if update.model.is_some() {
            sets.push("model = ?");
        }
        if update.cli_args.is_some() {
            sets.push("cli_args = ?");
        }
        if update.mcp_config.is_some() {
            sets.push("mcp_config = ?");
        }
        if update.thinking.is_some() {
            sets.push("thinking = ?");
        }
        if update.agent_env.is_some() {
            sets.push("agent_env = ?");
        }
        if update.token_budget.is_some() {
            sets.push("token_budget = ?");
        }
        let sql = format!(
            "UPDATE agent SET {} WHERE id = ? AND workspace_id = ?",
            sets.join(", ")
        );

        let mut query = sqlx::query(&sql);
        if let Some(name) = &update.name {
            query = query.bind(name);
        }
        if let Some(instructions) = &update.instructions {
            query = query.bind(instructions);
        }
        if let Some(model) = &update.model {
            query = query.bind(model);
        }
        if let Some(cli_args) = &update.cli_args {
            query = query.bind(cli_args_to_json(cli_args));
        }
        if let Some(mcp_config) = &update.mcp_config {
            // A cleared MCP config resets to the empty-object default `'{}'`, so
            // the column never holds SQL NULL (it is NOT NULL DEFAULT '{}').
            query = query.bind(mcp_config.clone().unwrap_or_else(|| "{}".to_string()));
        }
        if let Some(thinking) = &update.thinking {
            query = query.bind(thinking);
        }
        if let Some(agent_env) = &update.agent_env {
            query = query.bind(env_to_json(agent_env));
        }
        if let Some(token_budget) = &update.token_budget {
            query = query.bind(token_budget);
        }
        let res = query.bind(id).bind(workspace_id).execute(pool).await?;
        Ok(res.rows_affected() == 1)
    }

    /// Set (or clear) one agent's `archived` flag, scoped to a workspace
    /// (e38.15).
    ///
    /// Workspace-scoped at the SQL boundary: an agent id from another tenant
    /// matches no row and flips nothing. Returns `true` when exactly one row was
    /// updated, `false` when the `(id, workspace_id)` pair matched nothing (the
    /// not-found / cross-tenant case the caller surfaces as an error). Idempotent:
    /// archiving an already-archived agent still reports `true` (the row matched
    /// and was written), so re-archiving is not a spurious not-found.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn set_archived(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
        archived: bool,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent SET archived = ? WHERE id = ? AND workspace_id = ?")
            .bind(i64::from(archived))
            .bind(id)
            .bind(workspace_id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }
}

/// The full column list every `SELECT` reads, in [`Agent::from_row`] order. A
/// single constant keeps the read queries in lockstep with the `FromRow` impl.
const SELECT_COLS: &str = "SELECT id, workspace_id, name, runtime_id, instructions, visibility, \
     owner_id, archived, model, cli_args, mcp_config, thinking, agent_env, provider, \
     token_budget FROM agent";

/// Serialize a CLI-args list into the JSON-array text the `cli_args` column
/// stores. An empty list yields `"[]"` (the column default).
fn cli_args_to_json(args: &[String]) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string())
}

/// Re-assemble a CLI-args list from the `cli_args` column's JSON-array text.
fn cli_args_from_json(raw: &str) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(raw).map_err(|e| decode_err("cli_args", &e.to_string()))
}

/// Serialize a per-agent env map into the JSON-object text the `agent_env`
/// column stores. The ordered key-value list encodes as a JSON object; an empty
/// list yields `"{}"` (the column default).
fn env_to_json(env: &[(String, String)]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
}

/// Re-assemble a per-agent env map from the `agent_env` column's JSON-object
/// text, preserving the stored object's key order.
fn env_from_json(raw: &str) -> Result<Vec<(String, String)>, sqlx::Error> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| decode_err("agent_env", &e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| decode_err("agent_env", "stored env is not a JSON object"))?;
    obj.iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|s| (k.clone(), s.to_string()))
                .ok_or_else(|| decode_err("agent_env", "env value is not a string"))
        })
        .collect()
}

/// Map the empty-object MCP-config default `'{}'` back to `None` so a config-less
/// agent reads as `mcp_config: None` (vs an explicit non-trivial object).
fn mcp_config_from_raw(raw: String) -> Option<String> {
    if raw == "{}" { None } else { Some(raw) }
}

/// Build a `sqlx` decode error for a malformed JSON config column.
fn decode_err(column: &str, detail: &str) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: column.to_string(),
        source: format!("malformed '{column}': {detail}").into(),
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
            archived: row.try_get::<i64, _>("archived")? != 0,
            model: row.try_get("model")?,
            cli_args: cli_args_from_json(&row.try_get::<String, _>("cli_args")?)?,
            mcp_config: mcp_config_from_raw(row.try_get::<String, _>("mcp_config")?),
            thinking: row.try_get("thinking")?,
            agent_env: env_from_json(&row.try_get::<String, _>("agent_env")?)?,
            provider: row.try_get("provider")?,
            token_budget: row.try_get("token_budget")?,
        })
    }
}
