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
    /// Visibility scope: `"workspace"` or `"private"`. **Derived-legacy** since
    /// migration 0047: no code path gates invocation on it; it is kept in sync
    /// with [`permission_mode`](Self::permission_mode) purely so legacy readers
    /// never see a permission WIDENING (multica parity).
    pub visibility: String,
    /// Invocation-permission mode (migration 0047): `"private"` (owner-only,
    /// deny-by-default) or `"public_to"` (the [`agent_invocation_target`] allow-list
    /// decides). **Authoritative** invoke source — [`AgentRepo::can_invoke`] gates
    /// on this, not on [`visibility`](Self::visibility).
    ///
    /// [`agent_invocation_target`]: crate::repo::agent_invocation_target
    pub permission_mode: String,
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

/// Failure modes of [`AgentRepo::delete`] (Agents screen `x` remove, slice 2).
///
/// Mirrors [`crate::repo::issue::IssueDeleteError`]'s shape so the daemon handler
/// maps each arm to the same `INVALID_PARAMS` contract. An agent OWNS its task /
/// usage / autopilot rows by foreign key, so a hard delete is only safe once the
/// agent has no active run AND no FK-pinned history — the two guarded arms below.
#[derive(Debug)]
pub enum AgentDeleteError {
    /// No agent matched `(id, workspace_id)` — an unknown id or a foreign tenant.
    NotFound,
    /// The agent has one or more ACTIVE tasks (queued / dispatched / running);
    /// the run must be cancelled before the agent can be deleted. Carries the
    /// active count.
    ActiveTasks(i64),
    /// The agent still carries run history the schema pins by foreign key (past
    /// tasks, usage, autopilots): a hard delete would trip an FK constraint, so it
    /// is refused (archive the agent instead).
    HasHistory,
    /// An underlying store fault.
    Db(sqlx::Error),
}

impl std::fmt::Display for AgentDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no agent with that id in this workspace"),
            Self::ActiveTasks(n) => write!(
                f,
                "{n} active task(s) on this agent — cancel the run first, then delete"
            ),
            Self::HasHistory => write!(f, "agent has run history — archive it instead of deleting"),
            Self::Db(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for AgentDeleteError {}

impl From<sqlx::Error> for AgentDeleteError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
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
             (id, workspace_id, name, runtime_id, instructions, visibility, permission_mode, \
              owner_id, archived, model, cli_args, mcp_config, thinking, agent_env, provider, \
              token_budget) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&agent.id)
        .bind(&agent.workspace_id)
        .bind(&agent.name)
        .bind(&agent.runtime_id)
        .bind(&agent.instructions)
        .bind(&agent.visibility)
        .bind(&agent.permission_mode)
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

    /// Set one agent's [`permission_mode`](Agent::permission_mode) and re-derive the
    /// legacy [`visibility`](Agent::visibility) label to stay consistent (migration
    /// 0047, gap #8).
    ///
    /// `mode` must be `"private"` or `"public_to"` (the schema `CHECK` is the last
    /// line of defence). After the mode write, `visibility` is re-derived from the
    /// mode + the current allow-list: `public_to` with at least one `workspace`
    /// target reads back `"workspace"`; otherwise (private, or public_to with only
    /// member/team targets) it reads back `"private"`, so a legacy reader never sees
    /// a WIDENING. Both writes run in one transaction.
    ///
    /// Returns `true` when the agent existed and was updated, `false` when `id`
    /// matched no agent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if either write fails (e.g. a `CHECK` violation on
    /// an out-of-set `mode`).
    pub async fn set_permission_mode(
        pool: &SqlitePool,
        id: &str,
        mode: &str,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let res = sqlx::query("UPDATE agent SET permission_mode = ? WHERE id = ?")
            .bind(mode)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if res.rows_affected() != 1 {
            // No such agent — nothing to re-derive.
            tx.rollback().await?;
            return Ok(false);
        }
        let visibility = derive_visibility_in_tx(&mut tx, id, mode).await?;
        sqlx::query("UPDATE agent SET visibility = ? WHERE id = ?")
            .bind(visibility)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Re-derive the legacy [`visibility`](Agent::visibility) label from an agent's
    /// current mode + allow-list, keeping the two consistent after a target
    /// add/remove (migration 0047, gap #8). A no-op for an unknown id.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a lookup or the write fails.
    pub async fn rederive_visibility(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
        let mode: Option<String> =
            sqlx::query_scalar("SELECT permission_mode FROM agent WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        let Some(mode) = mode else {
            return Ok(());
        };
        let mut tx = pool.begin().await?;
        let visibility = derive_visibility_in_tx(&mut tx, id, &mode).await?;
        sqlx::query("UPDATE agent SET visibility = ? WHERE id = ?")
            .bind(visibility)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Hard-delete one agent, scoped to a workspace (Agents screen `x` remove,
    /// slice 2).
    ///
    /// Workspace-scoped at the SQL boundary: an `(id, workspace_id)` pair that
    /// matches no row is [`AgentDeleteError::NotFound`] (never a cross-tenant
    /// delete). The delete is GUARDED in two ways, mirroring
    /// [`crate::repo::issue::IssueRepo::delete_cascade`]:
    /// - refused while the agent has any ACTIVE task (queued / dispatched /
    ///   running) → [`AgentDeleteError::ActiveTasks`];
    /// - refused when the `DELETE` trips a foreign-key constraint because the agent
    ///   still owns FK-pinned history (past tasks, usage rows, autopilots) →
    ///   [`AgentDeleteError::HasHistory`]. `sqlx` runs `PRAGMA foreign_keys = ON`,
    ///   so the database itself refuses to orphan those rows — this never silently
    ///   dangles a reference.
    ///
    /// A fresh, never-run agent (the common case created from the Agents screen)
    /// has no active task and no history, so it deletes cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`AgentDeleteError`]: `NotFound` / `ActiveTasks` / `HasHistory` per
    /// the guards above, or `Db` on any other store fault.
    pub async fn delete(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
    ) -> Result<(), AgentDeleteError> {
        // Resolve the agent within its workspace; an unknown / foreign id is
        // NotFound rather than a silent no-op.
        let exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM agent WHERE id = ? AND workspace_id = ?")
                .bind(id)
                .bind(workspace_id)
                .fetch_optional(pool)
                .await?;
        if exists.is_none() {
            return Err(AgentDeleteError::NotFound);
        }

        // Refuse while any of the agent's tasks is live — deleting would orphan the
        // running attempt (and would trip the FK anyway; this yields the precise
        // "cancel first" message instead of the generic history one).
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_queue \
             WHERE agent_id = ? AND workspace_id = ? \
               AND status IN ('queued','dispatched','running')",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_one(pool)
        .await?;
        if active > 0 {
            return Err(AgentDeleteError::ActiveTasks(active));
        }

        match sqlx::query("DELETE FROM agent WHERE id = ? AND workspace_id = ?")
            .bind(id)
            .bind(workspace_id)
            .execute(pool)
            .await
        {
            Ok(_) => Ok(()),
            // The agent still carries FK-pinned history (terminal tasks, usage,
            // autopilots): the DB refuses the delete rather than orphaning those
            // rows. Surface it as the archive-instead guard, not a raw store error.
            Err(e) if is_foreign_key_violation(&e) => Err(AgentDeleteError::HasHistory),
            Err(e) => Err(AgentDeleteError::Db(e)),
        }
    }
}

/// Derive the legacy `visibility` label for an agent from its `permission_mode`
/// and current allow-list, inside a transaction (migration 0047 parity).
///
/// Rule (mirrors multica's derived-legacy field): a `public_to` agent with at
/// least one `workspace` invocation target is `"workspace"`; everything else
/// (`private`, or `public_to` with only member/team targets) is `"private"` — so
/// a legacy reader that still keys on `visibility` never sees a WIDENING relative
/// to the authoritative gate.
async fn derive_visibility_in_tx(
    tx: &mut sqlx::SqliteConnection,
    agent_id: &str,
    mode: &str,
) -> Result<&'static str, sqlx::Error> {
    if mode != "public_to" {
        return Ok("private");
    }
    let has_workspace_target: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_invocation_target \
         WHERE agent_id = ? AND target_type = 'workspace'",
    )
    .bind(agent_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(if has_workspace_target > 0 { "workspace" } else { "private" })
}

/// Whether a `sqlx` error is a SQLite foreign-key constraint violation. Used by
/// [`AgentRepo::delete`] to turn a history-pinned delete into the actionable
/// [`AgentDeleteError::HasHistory`] rather than an opaque store error.
fn is_foreign_key_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|db| db.message().contains("FOREIGN KEY"))
}

/// The full column list every `SELECT` reads, in [`Agent::from_row`] order. A
/// single constant keeps the read queries in lockstep with the `FromRow` impl.
const SELECT_COLS: &str = "SELECT id, workspace_id, name, runtime_id, instructions, visibility, \
     permission_mode, owner_id, archived, model, cli_args, mcp_config, thinking, agent_env, \
     provider, token_budget FROM agent";

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

#[cfg(test)]
mod delete_tests {
    use super::*;
    use crate::Store;
    use crate::bootstrap;

    /// Boot a fresh store with the default workspace + owner + runtime seeded, and
    /// create one agent on it. Returns `(store, workspace_id, agent)`; the store is
    /// kept alive by the caller so the temp DB outlives the test.
    async fn seed_agent(name: &str) -> (Store, String, Agent) {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir so the sqlite file survives for the store's lifetime.
        let dir = Box::leak(Box::new(dir));
        let store = Store::open_in(dir.path()).await.unwrap();
        let ws = bootstrap::ensure_default_workspace(store.pool()).await.unwrap();
        let agent = bootstrap::create_agent(store.pool(), &ws, name, "claude", None).await.unwrap();
        (store, ws, agent)
    }

    /// Insert one task for `agent` in `ws` at `status` (issue-less, so no issue FK
    /// is needed) — the fixture for the active-task and history guards.
    async fn seed_task(pool: &SqlitePool, ws: &str, agent: &Agent, id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, status, created_at) \
             VALUES (?, ?, ?, ?, ?, 1000)",
        )
        .bind(id)
        .bind(ws)
        .bind(&agent.runtime_id)
        .bind(&agent.id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A fresh, never-run agent deletes cleanly and vanishes from the roster.
    #[tokio::test]
    async fn delete_removes_a_fresh_agent() {
        let (store, ws, agent) = seed_agent("scratch").await;
        let pool = store.pool();

        AgentRepo::delete(pool, &ws, &agent.id).await.unwrap();

        assert!(AgentRepo::get(pool, &agent.id).await.unwrap().is_none());
        assert_eq!(bootstrap::agent_count(pool, &ws).await.unwrap(), 0);
    }

    /// An unknown id (never a real agent) is a not-found error, not a silent no-op.
    #[tokio::test]
    async fn delete_unknown_agent_is_not_found() {
        let (store, ws, _agent) = seed_agent("keep").await;
        let out = AgentRepo::delete(store.pool(), &ws, "no-such-agent").await;
        assert!(matches!(out, Err(AgentDeleteError::NotFound)));
    }

    /// A real agent id but a FOREIGN workspace matches no row — a not-found error,
    /// never a cross-tenant delete (the agent survives).
    #[tokio::test]
    async fn delete_is_workspace_scoped() {
        let (store, _ws, agent) = seed_agent("tenant-a").await;
        let pool = store.pool();

        let out = AgentRepo::delete(pool, "some-other-ws", &agent.id).await;
        assert!(matches!(out, Err(AgentDeleteError::NotFound)));
        assert!(
            AgentRepo::get(pool, &agent.id).await.unwrap().is_some(),
            "a cross-tenant delete must not remove the agent"
        );
    }

    /// An agent with a live (running) task is refused with the active-task count —
    /// cancel the run first (mirrors the issue delete guard).
    #[tokio::test]
    async fn delete_refused_while_a_task_is_active() {
        let (store, ws, agent) = seed_agent("busy").await;
        let pool = store.pool();
        seed_task(pool, &ws, &agent, "task-run-1", "running").await;

        let out = AgentRepo::delete(pool, &ws, &agent.id).await;
        assert!(
            matches!(out, Err(AgentDeleteError::ActiveTasks(1))),
            "a running task must block the delete with its count"
        );
        assert!(AgentRepo::get(pool, &agent.id).await.unwrap().is_some());
    }

    /// An agent whose only tasks are terminal still carries FK-pinned history, so a
    /// hard delete is refused as `HasHistory` (archive instead) — the DB never
    /// orphans the historical row.
    #[tokio::test]
    async fn delete_refused_when_history_is_fk_pinned() {
        let (store, ws, agent) = seed_agent("veteran").await;
        let pool = store.pool();
        seed_task(pool, &ws, &agent, "task-done-1", "done").await;

        let out = AgentRepo::delete(pool, &ws, &agent.id).await;
        assert!(
            matches!(out, Err(AgentDeleteError::HasHistory)),
            "a terminal task pins the agent by FK; delete must be refused, not orphaning it"
        );
        assert!(AgentRepo::get(pool, &agent.id).await.unwrap().is_some());
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
            permission_mode: row.try_get("permission_mode")?,
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
