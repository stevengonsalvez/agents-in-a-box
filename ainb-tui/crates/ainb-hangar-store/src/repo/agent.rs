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

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::skill::SkillName;
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
    /// schema stores this as a 0/1 INTEGER (migration 0015). This flag — NOT
    /// [`archived_at`](Self::archived_at) — is the authoritative discriminant.
    pub archived: bool,
    /// When the agent was archived (epoch ms, migration 0052), or `None` when the
    /// agent is active — or when it was archived BEFORE 0052 existed. That second
    /// case is an honest unknown; a historical archive is never given a
    /// fabricated timestamp.
    pub archived_at: Option<i64>,
    /// Who archived the agent, as a canonical actor-ref (migration 0052), or
    /// `None` when the agent is active / the archive predates 0052 / the archive
    /// was unattributed. A malformed stored value decodes to `None` rather than
    /// failing the whole read.
    pub archived_by: Option<ActorRef>,
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
    /// Short blurb rendered next to the agent in rosters/pickers; `""` when unset.
    /// Capped at 255 characters by the schema (migration 0050, multica 060).
    pub description: String,
    /// Optional avatar token; hangar mints `"emoji:🦊"`-style values at create so an
    /// agent is never avatar-less (multica `newAgentAvatar`). `None` only for rows
    /// created before migration 0050.
    pub avatar_url: Option<String>,
    /// `"user"` (an ordinary agent) or `"system"` (a hidden carrier agent, e.g. the
    /// agent-builder). Roster/picker reads filter to `"user"` (migration 0050).
    pub kind: String,
    /// Identity key for a system agent (e.g. `"agent_builder:<flow>"`); `None` for user
    /// agents. Unique per `(workspace, owner, runtime)` where not null (migration 0050).
    pub system_key: Option<String>,
    /// Optional per-agent Codex service-tier override (runtime-native catalog id such as
    /// `"priority"`); `None` = inherit the local Codex config. Stored + surfaced only in
    /// this milestone — dispatch-time consumption is a later feature (as `token_budget` was).
    pub service_tier: Option<String>,
    /// Skill NAMES this agent must never receive at runtime, independent of the
    /// `agent_skill` junction (migration 0051, multica 206). Empty by default.
    ///
    /// Distinct from attach/detach and from `agent_skill.enabled`: this is a
    /// by-name suppression list, so it can pre-emptively name a skill the agent
    /// is not (yet) attached to. Honoured at dispatch-time materialisation
    /// (hangar has no live tool registry to gate — deviation D1), so a named
    /// skill's directory is never written into the agent's task tree.
    ///
    /// Stored as a JSON-array TEXT column; a corrupt cell degrades to empty
    /// rather than failing the read.
    pub disabled_runtime_skills: Vec<String>,
}

impl Default for Agent {
    /// The neutral, never-inserted-as-is shape: an ordinary `"user"`-kind,
    /// `"private"` agent with no metadata. Fixture sites spread this
    /// (`Agent { name: …, ..Default::default() }`) so a later schema add is one
    /// struct edit, not another brittle-fixture sweep.
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            name: String::new(),
            runtime_id: String::new(),
            instructions: None,
            visibility: "private".to_string(),
            permission_mode: "private".to_string(),
            owner_id: String::new(),
            archived: false,
            archived_at: None,
            archived_by: None,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
            provider: None,
            token_budget: None,
            description: String::new(),
            avatar_url: None,
            kind: AGENT_KIND_USER.to_string(),
            system_key: None,
            service_tier: None,
            disabled_runtime_skills: Vec::new(),
        }
    }
}

/// The ordinary agent kind — the only one rosters, pickers and search surface.
pub const AGENT_KIND_USER: &str = "user";
/// The hidden carrier kind (e.g. the agent-builder). Never in a roster; reached
/// only by [`AgentRepo::find_system`].
pub const AGENT_KIND_SYSTEM: &str = "system";

/// Maximum `description` length in CHARACTERS (code points), matching multica's
/// `utf8.RuneCountInString` cap and the schema `CHECK (length(description) <= 255)`.
/// Callers validate against this so the user sees a clear message; the schema
/// CHECK is the last line of defence.
pub const MAX_DESCRIPTION_CHARS: usize = 255;

/// Whether a `sqlx` error is the `(workspace_id, name)` uniqueness violation from
/// migration 0050 — i.e. "an agent by that name already exists here" rather than
/// any other store fault. Callers map it to multica's 409-equivalent refusal.
///
/// SQLite's message is `UNIQUE constraint failed: agent.workspace_id, agent.name`;
/// the `agent.name` substring separates it from the `system_key` index and from an
/// id PK collision.
#[must_use]
pub fn is_duplicate_name(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|db| db.is_unique_violation() && db.message().contains("agent.name"))
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
    /// New description, or `None` to leave it unchanged. The column is NOT NULL
    /// (its "cleared" state is `""`), so this is a single `Option` like `name`.
    pub description: Option<String>,
    /// New avatar token: `None` leaves it, `Some(None)` clears it,
    /// `Some(Some(_))` sets it (migration 0050).
    pub avatar_url: Option<Option<String>>,
    /// New Codex service tier: `None` leaves it, `Some(None)` clears it (back to
    /// inheriting the local Codex config), `Some(Some(_))` sets it (migration 0050).
    pub service_tier: Option<Option<String>>,
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
            && self.description.is_none()
            && self.avatar_url.is_none()
            && self.service_tier.is_none()
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
              token_budget, description, avatar_url, kind, system_key, service_tier, \
              disabled_runtime_skills) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&agent.description)
        .bind(&agent.avatar_url)
        .bind(&agent.kind)
        .bind(&agent.system_key)
        .bind(&agent.service_tier)
        .bind(disabled_runtime_skills_to_json(
            &agent.disabled_runtime_skills,
        ))
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one [`Agent`] by primary key, or `None` if absent.
    ///
    /// Deliberately **kind-blind** (migration 0050): this is the internal by-id
    /// lookup every dispatch/edit path uses, so a `"system"` agent resolved by an
    /// id it already holds must still read back. Multica's `GetAgent` is likewise
    /// unfiltered — only the roster/picker LISTS filter to `kind = 'user'`.
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
            "{SELECT_COLS} WHERE workspace_id = ? AND archived = 0 AND kind = 'user' ORDER BY name"
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
            "{SELECT_COLS} WHERE workspace_id = ? AND kind = 'user' ORDER BY name"
        ))
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }

    /// List the ids of every ACTIVE agent bound to one runtime, ordered by
    /// `name`.
    ///
    /// The presence sweeper's event fan-out is the caller: when a runtime's
    /// liveness flips, every agent backed by it changed availability, and only
    /// the ids are needed to address the event. Archived agents are excluded —
    /// they are absent from the picker, so no surface would render their dot.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_ids_by_runtime(
        pool: &SqlitePool,
        runtime_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM agent WHERE runtime_id = ? AND archived = 0 AND kind = 'user' \
             ORDER BY name",
        )
        .bind(runtime_id)
        .fetch_all(pool)
        .await
    }

    /// Look up a hidden `"system"` agent by its identity key within a workspace
    /// (migration 0050, multica `chat.sql:433` — the agent-builder carrier lookup).
    ///
    /// This is the ONLY read that returns a system agent by search: every roster,
    /// picker and search query filters to `kind = 'user'`, so a carrier agent is
    /// invisible everywhere else.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails (a missing row is `Ok(None)`).
    pub async fn find_system(
        pool: &SqlitePool,
        workspace_id: &str,
        system_key: &str,
    ) -> Result<Option<Agent>, sqlx::Error> {
        sqlx::query_as::<_, Agent>(&format!(
            "{SELECT_COLS} WHERE workspace_id = ? AND system_key = ? AND kind = 'system'"
        ))
        .bind(workspace_id)
        .bind(system_key)
        .fetch_optional(pool)
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
    /// Returns a [`sqlx::Error`] if the update fails — including a RENAME onto a
    /// name another agent in the same workspace already holds, for which
    /// [`is_duplicate_name`] is `true` (migration 0050).
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
        if update.description.is_some() {
            sets.push("description = ?");
        }
        if update.avatar_url.is_some() {
            sets.push("avatar_url = ?");
        }
        if update.service_tier.is_some() {
            sets.push("service_tier = ?");
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
        if let Some(description) = &update.description {
            query = query.bind(description);
        }
        if let Some(avatar_url) = &update.avatar_url {
            query = query.bind(avatar_url);
        }
        if let Some(service_tier) = &update.service_tier {
            query = query.bind(service_tier);
        }
        let res = query.bind(id).bind(workspace_id).execute(pool).await?;
        Ok(res.rows_affected() == 1)
    }

    /// Set (or clear) one agent's `archived` flag **and its audit trail**, scoped
    /// to a workspace (e38.15; audit trail = migration 0052, multica gap #26).
    ///
    /// There is deliberately no audit-less archive path: `by` (the archiving
    /// actor, `None` = unattributed) and `now_ms` are required parameters, so a
    /// caller cannot flip the flag without deciding what to record.
    ///
    /// - archiving (`archived = true`) STAMPS `archived_at = now_ms` and
    ///   `archived_by = by`. Re-archiving RE-stamps: last archiver wins, which is
    ///   still an honest record of the most recent archive action.
    /// - un-archiving (`archived = false`) CLEARS both audit columns (multica
    ///   `RestoreAgent` parity) — a restored agent carries no stale stamp.
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
        by: Option<&ActorRef>,
        now_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE agent SET archived = ?, archived_at = ?, archived_by = ? \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(i64::from(archived))
        .bind(archived.then_some(now_ms))
        .bind(archived.then(|| by.map(ToString::to_string)).flatten())
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
    /// target reads back `"workspace"`; otherwise (`private`, or `public_to` with
    /// only member/team targets) it reads back `"private"`, so a legacy reader never
    /// sees a WIDENING. Both writes run in one transaction.
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

    /// Replace an agent's runtime skill-suppression list (migration 0051,
    /// multica 206).
    ///
    /// The supplied names are normalised (kebab-cased, de-duplicated, sorted)
    /// before storage, so `["Commit", "commit"]` collapses to `["commit"]` and
    /// the stored cell is canonical. A name that normalises to empty is dropped.
    /// Pass an empty slice to clear the list.
    ///
    /// Suppression is by NAME and independent of the `agent_skill` junction: a
    /// name here can pre-date the attachment. Honoured at dispatch-time
    /// materialisation (deviation D1).
    ///
    /// Returns `true` when the agent existed and was updated, `false` when `id`
    /// matched no agent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn set_disabled_runtime_skills(
        pool: &SqlitePool,
        id: &str,
        names: &[String],
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE agent SET disabled_runtime_skills = ? WHERE id = ?")
            .bind(disabled_runtime_skills_to_json(names))
            .bind(id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() > 0)
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

    /// Report whether a run may be enqueued for `agent` on behalf of `invoker`
    /// (multica `canInvokeAgent` parity, gap #8). **Deny-by-default.**
    ///
    /// `invoker_kind` / `invoker_user_id` are the EFFECTIVE invoking identity:
    ///   - a [`ActorKind::Member`] actor → that member's user id.
    ///   - an [`ActorKind::Agent`] actor → the top-of-chain human ORIGINATOR id if
    ///     resolved, else `None` (hangar has no originator column yet; an
    ///     agent-actor invoke passes `None` and relies on the workspace-target
    ///     exception below, failing closed for member/team targets — exactly
    ///     multica's unattributed case). Resolving the originator (multica 184/185)
    ///     is a separate downstream gap.
    ///
    /// Rules (ported 1:1 from `agent_access.go:48`):
    ///   1. `invoker_user_id == agent.owner_id` (non-empty) → allow (owner always).
    ///   2. `permission_mode != "public_to"` → deny (private / unknown =
    ///      deny-by-default; **no admin bypass, no A2A bypass** — the privacy-hole
    ///      fix).
    ///   3. `public_to`: OR-match the allow-list:
    ///      - `workspace` target → allow if the invoker is a workspace member, OR the
    ///        actor is [`ActorKind::Agent`] (the `workspaceBroad` exception, scoped
    ///        ONLY to workspace targets, so unattributed automation can trigger a
    ///        `public_to workspace` agent but fails closed against member/team).
    ///      - `member` target → allow if `invoker_user_id == target_id` (a resolved
    ///        user; an Agent with `None` originator never matches — fail closed).
    ///      - `team` target → inert in V1 (never admits).
    ///   4. no match → deny.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a lookup fails.
    pub async fn can_invoke(
        pool: &SqlitePool,
        agent: &Agent,
        invoker_kind: ainb_hangar_core::actor::ActorKind,
        invoker_user_id: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        use crate::repo::agent_invocation_target::AgentInvocationTargetRepo;
        use crate::repo::member::MemberRepo;
        use ainb_hangar_core::actor::ActorKind;
        use ainb_hangar_core::ids::WorkspaceId;

        // 1. Owner always — a non-empty invoker id matching the agent's owner.
        if let Some(uid) = invoker_user_id {
            if !agent.owner_id.is_empty() && uid == agent.owner_id {
                return Ok(true);
            }
        }
        // 2. Private / unknown mode = deny-by-default (no admin/A2A bypass).
        if agent.permission_mode != "public_to" {
            return Ok(false);
        }
        // 3. public_to: OR-match the allow-list.
        let targets = AgentInvocationTargetRepo::list(pool, &agent.id).await?;
        // Is the (resolved) invoker a member of the agent's workspace?
        let is_ws_member = match invoker_user_id {
            Some(uid) => match WorkspaceId::from_str(agent.workspace_id.clone()) {
                Ok(ws) => MemberRepo::role(pool, &ws, uid).await?.is_some(),
                // An empty workspace id cannot resolve a membership → not a member.
                Err(_) => false,
            },
            None => false,
        };
        // The workspaceBroad exception: unattributed automation (an Agent actor with
        // no resolved originator) may trigger a workspace-target agent, but nothing
        // narrower. (System actor joins this branch when hangar grows one.)
        let workspace_broad = invoker_kind == ActorKind::Agent;
        for t in &targets {
            match t.target_type.as_str() {
                "workspace" => {
                    if is_ws_member || workspace_broad {
                        return Ok(true);
                    }
                }
                "member" => {
                    if let Some(uid) = invoker_user_id {
                        if t.target_id == uid {
                            return Ok(true);
                        }
                    }
                }
                // `team` is reserved and inert in V1 (no team-membership source).
                _ => {}
            }
        }
        Ok(false)
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
    Ok(if has_workspace_target > 0 {
        "workspace"
    } else {
        "private"
    })
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
     provider, token_budget, description, avatar_url, kind, system_key, service_tier, \
     disabled_runtime_skills, archived_at, archived_by FROM agent";

/// Serialize a CLI-args list into the JSON-array text the `cli_args` column
/// stores. An empty list yields `"[]"` (the column default).
fn cli_args_to_json(args: &[String]) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string())
}

/// Re-assemble a CLI-args list from the `cli_args` column's JSON-array text.
fn cli_args_from_json(raw: &str) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(raw).map_err(|e| decode_err("cli_args", &e.to_string()))
}

/// Normalise a runtime-skill suppression list for storage: kebab-cased (via the
/// same [`SkillName`] rules the skill rows use), de-duplicated, sorted, then
/// encoded as a JSON array. Sorting + dedup make the stored cell canonical, so
/// re-writing the same set is a byte-identical no-op.
///
/// [`SkillName`]: ainb_hangar_core::skill::SkillName
fn disabled_runtime_skills_to_json(names: &[String]) -> String {
    let mut normalised: Vec<String> = names
        .iter()
        .filter_map(|n| SkillName::new(n).ok())
        .map(|n| n.as_str().to_string())
        .collect();
    normalised.sort();
    normalised.dedup();
    serde_json::to_string(&normalised).unwrap_or_else(|_| "[]".to_string())
}

/// Decode the `disabled_runtime_skills` JSON-array cell.
///
/// Deliberately tolerant (`unwrap_or_default`, like `issue.labels`): a corrupt
/// cell degrades to "nothing suppressed" instead of failing the agent read and
/// taking the daemon's dispatch path down with it.
fn disabled_runtime_skills_from_json(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
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
            description: row.try_get("description")?,
            avatar_url: row.try_get("avatar_url")?,
            kind: row.try_get("kind")?,
            system_key: row.try_get("system_key")?,
            service_tier: row.try_get("service_tier")?,
            disabled_runtime_skills: disabled_runtime_skills_from_json(
                &row.try_get::<String, _>("disabled_runtime_skills")?,
            ),
            archived_at: row.try_get("archived_at")?,
            archived_by: row
                .try_get::<Option<String>, _>("archived_by")?
                .and_then(|s| s.parse::<ActorRef>().ok()),
        })
    }
}

#[cfg(test)]
mod can_invoke_tests {
    use super::*;
    use crate::Store;
    use crate::bootstrap;
    use crate::repo::agent_invocation_target::AgentInvocationTargetRepo;
    use crate::repo::member::{MemberRepo, MemberRole};
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_core::ids::WorkspaceId;

    /// Seed a workspace with an owner-owned agent, a real workspace member `bob`,
    /// and a NON-member user id `carol`. Returns `(store, ws, agent, owner_id,
    /// bob_id, carol_id)`.
    async fn seed() -> (Store, String, Agent, String, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let dir = Box::leak(Box::new(dir));
        let store = Store::open_in(dir.path()).await.unwrap();
        let ws = bootstrap::ensure_default_workspace(store.pool()).await.unwrap();
        let owner_id = bootstrap::default_owner_id(store.pool()).await.unwrap().unwrap();
        let agent = bootstrap::create_agent(store.pool(), &ws, "secret-bot", "claude", None)
            .await
            .unwrap();
        let ws_id = WorkspaceId::from_str(ws.clone()).unwrap();
        let bob = MemberRepo::add(store.pool(), &ws_id, "bob@example.com", MemberRole::Member)
            .await
            .unwrap();
        // carol is a user id that is NOT a member of this workspace.
        (
            store,
            ws,
            agent,
            owner_id,
            bob.user_id,
            "u-carol-nonmember".to_string(),
        )
    }

    /// The full multica `canInvokeAgent` truth table across the four allow-list
    /// shapes: private, member target, workspace target, team target.
    #[tokio::test]
    async fn can_invoke_truth_table() {
        let (store, _ws, agent, owner, bob, carol) = seed().await;
        let pool = store.pool();
        let ws_target = agent.workspace_id.clone();

        // ---- private (the create default): owner-only, deny everything else. ----
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&owner))
                .await
                .unwrap(),
            "private: the owner always invokes"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&bob))
                .await
                .unwrap(),
            "private: a non-owner member is denied (deny-by-default)"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Agent, None).await.unwrap(),
            "private: an unattributed agent actor is denied (no A2A bypass)"
        );

        // ---- public_to + MEMBER target bob: bob in, carol/None out, owner in. ----
        AgentRepo::set_permission_mode(pool, &agent.id, "public_to").await.unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent.id,
            "member",
            &bob,
            Some(&owner),
        )
        .await
        .unwrap();
        // Re-read so the struct carries the updated permission_mode.
        let agent = AgentRepo::get(pool, &agent.id).await.unwrap().unwrap();
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&bob))
                .await
                .unwrap(),
            "member target: bob (the listed member) invokes"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&carol))
                .await
                .unwrap(),
            "member target: carol (never listed) is denied"
        );
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&owner))
                .await
                .unwrap(),
            "member target: the owner still invokes (owner branch)"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Agent, None).await.unwrap(),
            "member target: an unattributed agent fails closed (not a workspace target)"
        );

        // ---- public_to + WORKSPACE target: members in, non-members out, ----
        // ---- unattributed agent in (workspaceBroad exception). ----
        AgentInvocationTargetRepo::remove(pool, &agent.id, "member", &bob)
            .await
            .unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent.id,
            "workspace",
            &ws_target,
            Some(&owner),
        )
        .await
        .unwrap();
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&bob))
                .await
                .unwrap(),
            "workspace target: bob (a member) invokes"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&carol))
                .await
                .unwrap(),
            "workspace target: carol (not a member) is denied"
        );
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Agent, None).await.unwrap(),
            "workspace target: an unattributed agent IS admitted (workspaceBroad)"
        );

        // ---- public_to + TEAM target only: inert — nobody but the owner. ----
        AgentInvocationTargetRepo::remove(pool, &agent.id, "workspace", &ws_target)
            .await
            .unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent.id,
            "team",
            "team-1",
            Some(&owner),
        )
        .await
        .unwrap();
        assert!(
            AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&owner))
                .await
                .unwrap(),
            "team target: owner still invokes (owner branch)"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Member, Some(&bob))
                .await
                .unwrap(),
            "team target: a member is denied (team is inert in V1)"
        );
        assert!(
            !AgentRepo::can_invoke(pool, &agent, ActorKind::Agent, None).await.unwrap(),
            "team target: an unattributed agent is denied (team is inert)"
        );
    }
}
