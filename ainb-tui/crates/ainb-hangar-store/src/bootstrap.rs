//! Fresh-home bootstrap: the idempotent default workspace + owner + runtime +
//! starter agent that make an empty `hangar.db` "just work".
//!
//! A brand-new `~/.agents-in-a-box/hangar.db` has no workspace, no runtime, and
//! no agent, so the TUI shows no runtime and the Squad screen rejects a create
//! with "no agent available to lead a squad". This module owns the ONE shared,
//! idempotent, non-destructive lay-down every entry point (the CLI's lazy
//! `issue create`, the daemon boot seed, and the `hangar/agent_create` RPC)
//! delegates to, so a workspace / runtime / owner is materialised in exactly one
//! place and the same way every time.
//!
//! # The stable-runtime invariant (correctness-critical)
//!
//! Every seeded / created agent binds [`default_runtime_id`], and the daemon's
//! claim loop + self-register key off the SAME id. If they diverged, an agent
//! would bind a runtime the daemon never claims for and its tasks would never
//! run. This module is the single source of that id, so the three callers cannot
//! drift.
//!
//! Every function here is idempotent and non-clobbering: it finds-or-creates and
//! never rewrites or deletes a user's own rows, so calling it on every boot is
//! safe.

use sqlx::SqlitePool;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};

use crate::repo::agent::{Agent, AgentRepo};

/// Slug of the workspace bootstrapped when the database has none.
pub const DEFAULT_WORKSPACE_SLUG: &str = "default";
/// Human name of the bootstrapped default workspace.
pub const DEFAULT_WORKSPACE_NAME: &str = "Default Workspace";
/// Email of the bootstrapped owner user.
pub const DEFAULT_OWNER_EMAIL: &str = "stevie@local";

/// Stable id of the single host runtime the daemon self-registers and claims
/// tasks for. The seed, the daemon's default claim id, and every
/// `agent_create` binding MUST resolve to this same value (see
/// [`default_runtime_id`]).
pub const DEFAULT_RUNTIME_ID: &str = "default";

/// The provider a freshly-seeded starter agent (and the self-registered runtime)
/// advertises by default.
pub const DEFAULT_PROVIDER: &str = "claude";

/// The providers `hangar/agent_create` accepts. The chosen value is recorded on
/// the agent row (migration 0041); the daemon still binds the single default
/// runtime so the task runs regardless (the runtime resolves the exec path).
pub const SUPPORTED_PROVIDERS: [&str; 3] = ["claude", "codex", "copilot"];

/// `daemon_id` recorded for the self-registered host runtime. Keyed with
/// `(workspace_id, daemon_id, provider)` for the runtime's unique index.
const SELF_DAEMON_ID: &str = "ainb-hangar-daemon";
/// Runtime mode of the self-registered runtime (a local daemon today).
const SELF_RUNTIME_MODE: &str = "local";

/// Resolve the runtime id the daemon claims for: `HANGAR_DAEMON_RUNTIME_ID` when
/// set + non-empty (an operator override), else the stable [`DEFAULT_RUNTIME_ID`].
///
/// This is the single seam the seed, the daemon claim loop, and `agent_create`
/// all take the runtime id through, so they cannot diverge.
#[must_use]
pub fn default_runtime_id() -> String {
    std::env::var("HANGAR_DAEMON_RUNTIME_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_RUNTIME_ID.to_string())
}

/// Normalise + validate a caller-supplied provider: trim, lower-case, default an
/// absent/empty value to [`DEFAULT_PROVIDER`], and reject anything outside
/// [`SUPPORTED_PROVIDERS`].
///
/// # Errors
///
/// Returns the offending value in an error message when the provider is not one
/// of `claude` / `codex` / `copilot`.
pub fn normalize_provider(provider: Option<&str>) -> Result<String, String> {
    let raw = provider.map(str::trim).filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return Ok(DEFAULT_PROVIDER.to_string());
    };
    let lowered = raw.to_ascii_lowercase();
    if SUPPORTED_PROVIDERS.contains(&lowered.as_str()) {
        Ok(lowered)
    } else {
        Err(format!(
            "unsupported provider `{raw}` (expected one of {})",
            SUPPORTED_PROVIDERS.join(", ")
        ))
    }
}

/// The oldest workspace's id (the default), or `None` when none exists yet.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the query fails.
pub async fn find_default_workspace(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM workspace ORDER BY created_at LIMIT 1")
        .fetch_optional(pool)
        .await
}

/// The default owner user id (the oldest user), or `None` when no user exists.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the query fails.
pub async fn default_owner_id(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM user ORDER BY created_at LIMIT 1")
        .fetch_optional(pool)
        .await
}

/// Return the default workspace id, lazily laying down a workspace + owner user
/// + owner member when the database has none.
///
/// Idempotent: a second call finds the existing workspace and returns the same
/// id without inserting anything. Non-destructive: it never rewrites an existing
/// workspace.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if a lookup or insert fails.
pub async fn ensure_default_workspace(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    if let Some(id) = find_default_workspace(pool).await? {
        return Ok(id);
    }
    let now = SystemClock.now_ms();
    let idgen = SystemIdGen;
    let workspace_id = idgen.new_ulid();
    let user_id = idgen.new_ulid();

    // `issue_prefix` is left NULL (the HGR display id lives at the render layer).
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(&workspace_id)
        .bind(DEFAULT_WORKSPACE_SLUG)
        .bind(DEFAULT_WORKSPACE_NAME)
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(&user_id)
        .bind(DEFAULT_OWNER_EMAIL)
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&workspace_id)
        .bind(&user_id)
        .execute(pool)
        .await?;
    Ok(workspace_id)
}

/// Upsert the host runtime row keyed on `runtime_id`, attaching it to the
/// default (oldest) workspace and marking it `online`.
///
/// Idempotent: `ON CONFLICT(id)` refreshes an existing row (a daemon restart)
/// rather than tripping the PK or the `(workspace_id, daemon_id, provider)`
/// unique index. Returns `Ok(true)` when a row was written/refreshed, `Ok(false)`
/// when there is no workspace to attach to yet (a benign no-op).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the workspace lookup or the upsert fails.
pub async fn ensure_runtime(
    pool: &SqlitePool,
    runtime_id: &str,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    let Some(workspace_id) = find_default_workspace(pool).await? else {
        return Ok(false);
    };
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'online') \
         ON CONFLICT(id) DO UPDATE SET \
           status = 'online', \
           last_seen_at = excluded.last_seen_at",
    )
    .bind(runtime_id)
    .bind(&workspace_id)
    .bind(SELF_DAEMON_ID)
    .bind(DEFAULT_PROVIDER)
    .bind(SELF_RUNTIME_MODE)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(true)
}

/// Create one agent from scratch, filling every FK behind the scenes: bind the
/// default runtime (ensuring its row exists), resolve the default owner, mint a
/// fresh id, and insert. The caller supplies only the human `name` (+ an
/// already-normalised `provider` and optional `instructions`).
///
/// The returned [`Agent`] carries the minted id so a caller can route to it
/// (e.g. as a squad leader). `provider` is recorded on the row; the daemon binds
/// the default runtime regardless, so the task still runs.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the workspace has no owner user yet (the FK could
/// not be filled) or if the runtime upsert / agent insert fails.
pub async fn create_agent(
    pool: &SqlitePool,
    workspace_id: &str,
    name: &str,
    provider: &str,
    instructions: Option<String>,
) -> Result<Agent, sqlx::Error> {
    let runtime_id = default_runtime_id();
    let now = SystemClock.now_ms();
    // The agent's runtime FK must exist; ensure it before the insert. A fresh
    // home may not have registered a runtime yet (the CLI create path runs with
    // no daemon), so this makes the create self-sufficient.
    ensure_runtime(pool, &runtime_id, now).await?;

    let owner_id = default_owner_id(pool).await?.ok_or_else(|| sqlx::Error::RowNotFound)?;

    let agent = Agent {
        id: SystemIdGen.new_ulid(),
        workspace_id: workspace_id.to_string(),
        name: name.to_string(),
        runtime_id,
        instructions,
        visibility: "workspace".to_string(),
        owner_id,
        archived: false,
        model: None,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: Some(provider.to_string()),
    };
    AgentRepo::insert(pool, &agent).await?;
    Ok(agent)
}

/// Count the workspace's agents (active AND archived), the non-clobber guard the
/// boot seed reads before laying down a starter agent: a user who created,
/// renamed, or archived their own agent has count > 0, so the seed skips.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the query fails.
pub async fn agent_count(pool: &SqlitePool, workspace_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent WHERE workspace_id = ?")
        .bind(workspace_id)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[tokio::test]
    async fn ensure_default_workspace_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        let first = ensure_default_workspace(pool).await.unwrap();
        let second = ensure_default_workspace(pool).await.unwrap();
        assert_eq!(first, second, "second call returns the same workspace id");

        let ws_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspace").fetch_one(pool).await.unwrap();
        assert_eq!(ws_count, 1, "only one workspace row is ever created");
        let user_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user").fetch_one(pool).await.unwrap();
        assert_eq!(user_count, 1, "only one owner user is ever created");
    }

    #[tokio::test]
    async fn default_owner_id_after_ensure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        assert!(default_owner_id(pool).await.unwrap().is_none(), "no user before ensure");
        ensure_default_workspace(pool).await.unwrap();
        assert!(default_owner_id(pool).await.unwrap().is_some(), "an owner exists after ensure");
    }

    #[tokio::test]
    async fn ensure_runtime_noop_without_workspace_then_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        assert!(!ensure_runtime(pool, "default", 1_000).await.unwrap(), "no workspace ⇒ no-op");
        ensure_default_workspace(pool).await.unwrap();
        assert!(ensure_runtime(pool, "default", 1_000).await.unwrap(), "workspace ⇒ upsert");
        // A restart upserts, never duplicates.
        assert!(ensure_runtime(pool, "default", 2_000).await.unwrap());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "a restart upserts the same runtime row");
    }

    #[tokio::test]
    async fn create_agent_fills_every_fk_and_records_provider() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = ensure_default_workspace(pool).await.unwrap();

        let agent = create_agent(pool, &ws, "reviewer", "codex", None).await.unwrap();
        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.runtime_id, default_runtime_id(), "binds the default runtime");
        assert_eq!(agent.provider.as_deref(), Some("codex"), "records the chosen provider");
        assert!(!agent.owner_id.is_empty(), "owner FK filled");

        // The agent is readable back (all FKs satisfied at insert).
        let fetched = AgentRepo::get(pool, &agent.id).await.unwrap().expect("agent persisted");
        assert_eq!(fetched.provider.as_deref(), Some("codex"));
        assert_eq!(agent_count(pool, &ws).await.unwrap(), 1);
    }

    #[test]
    fn normalize_provider_defaults_and_validates() {
        assert_eq!(normalize_provider(None).unwrap(), "claude");
        assert_eq!(normalize_provider(Some("")).unwrap(), "claude");
        assert_eq!(normalize_provider(Some("  ")).unwrap(), "claude");
        assert_eq!(normalize_provider(Some("Codex")).unwrap(), "codex", "case-insensitive");
        assert_eq!(normalize_provider(Some("copilot")).unwrap(), "copilot");
        assert!(normalize_provider(Some("gpt5")).is_err(), "unknown provider is rejected");
    }
}
