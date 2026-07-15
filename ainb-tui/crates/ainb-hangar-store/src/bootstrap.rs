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
//! Every seeded / created agent binds [`resolve_runtime_id`], and the daemon's
//! claim loop + self-register resolve the SAME id. If they diverged, an agent
//! would bind a runtime the daemon never claims for and its tasks would never
//! run. This module is the single source of that id, so the callers cannot drift.
//!
//! A runtime **cannot be renamed** after first boot: `agent.runtime_id` is an
//! enforced `REFERENCES agent_runtime(id)` FK (sqlx sets `PRAGMA foreign_keys = ON`),
//! so an already-registered runtime's id always WINS over a changed
//! `HANGAR_DAEMON_RUNTIME_ID` / [`DEFAULT_RUNTIME_ID`]. Only a brand-new home
//! adopts the configured id ([`resolve_runtime_id`]); the daemon warns when it
//! ignores a configured id.
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

/// Stable id used for the host runtime on a BRAND-NEW home.
///
/// Once a runtime row exists its id wins forever (a runtime cannot be renamed —
/// see the module docs), so this is only the first-boot default; use
/// [`resolve_runtime_id`] to get the id actually in use.
pub const DEFAULT_RUNTIME_ID: &str = "default";

/// The provider a freshly-seeded starter agent (and the self-registered runtime)
/// advertises by default.
pub const DEFAULT_PROVIDER: &str = "claude";

/// The providers `hangar/agent_create` accepts.
///
/// The chosen value is recorded on the agent row (migration 0041) and HONOURED at
/// dispatch: the agent binds the single default runtime (an execution slot claimed
/// by id, not provider) and the daemon spawns the recorded provider's backend per
/// task.
pub const SUPPORTED_PROVIDERS: [&str; 3] = ["claude", "codex", "copilot"];

/// `daemon_id` recorded for the self-registered host runtime. Keyed with
/// `(workspace_id, daemon_id, provider)` for the runtime's unique index.
const SELF_DAEMON_ID: &str = "ainb-hangar-daemon";
/// Runtime mode of the self-registered runtime (a local daemon today).
const SELF_RUNTIME_MODE: &str = "local";

/// The CONFIGURED runtime id: `HANGAR_DAEMON_RUNTIME_ID` when set + non-empty (an
/// operator override), else the stable [`DEFAULT_RUNTIME_ID`].
///
/// This is only the first-boot identity. Once a runtime is registered its id wins
/// (a runtime cannot be renamed), so callers that need the id actually in use must
/// go through [`resolve_runtime_id`] — the seam the seed, the claim loop, and
/// `agent_create` all share.
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

    // The workspace + owner + membership land in ONE transaction so a partial
    // "workspace with no owner" can never persist. `workspace.slug` is NOT NULL
    // UNIQUE, so two concurrent fresh-home writers (the daemon autostart + a
    // racing `ainb hangar ...` CLI) can both pass the find-None above; the loser's
    // workspace INSERT then trips the slug UNIQUE. That is not an error to the
    // caller — the workspace exists — so on a unique violation we roll back and
    // return the winner's id (which is committed + visible on a fresh connection
    // under WAL).
    let mut tx = pool.begin().await?;
    // `issue_prefix` is left NULL (the HGR display id lives at the render layer).
    let insert_ws =
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(&workspace_id)
            .bind(DEFAULT_WORKSPACE_SLUG)
            .bind(DEFAULT_WORKSPACE_NAME)
            .bind(now)
            .execute(&mut *tx)
            .await;
    if let Err(e) = insert_ws {
        let lost_the_race = e
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation);
        if lost_the_race {
            drop(tx); // roll back this loser's transaction
            return find_default_workspace(pool).await?.ok_or(e);
        }
        return Err(e);
    }
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(&user_id)
        .bind(DEFAULT_OWNER_EMAIL)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&workspace_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(workspace_id)
}

/// Upsert the host runtime row for this daemon, attaching it to the default
/// (oldest) workspace and marking it `online`.
///
/// The conflict target is the REAL uniqueness key
/// `(workspace_id, daemon_id, provider)` (migration 0002's unique index), not the
/// primary key `id` — and the upsert NEVER changes the `id`. `agent.runtime_id`
/// is a NOT NULL `REFERENCES agent_runtime(id)` FK and SQLite enforces foreign
/// keys (sqlx sets `PRAGMA foreign_keys = ON`), so changing the runtime's `id`
/// while agents reference it would raise `FOREIGN KEY constraint failed`. A
/// runtime therefore CANNOT be renamed after first boot: if a caller passes a
/// different `runtime_id` for an existing `(workspace, daemon, provider)` tuple
/// (e.g. a changed `HANGAR_DAEMON_RUNTIME_ID`), this refreshes the EXISTING row's
/// `status`/`last_seen_at` and keeps its original `id`. Boot resolves the
/// effective claim id from the existing row up front (see the daemon's
/// `effective_runtime_id`), so the registered row, the agents bound to it, and the
/// claim loop all stay aligned — no drift, no stranding, no FK error.
///
/// Returns `Ok(true)` when a row was written/refreshed, `Ok(false)` when there is
/// no workspace to attach to yet (a benign no-op).
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
         ON CONFLICT(workspace_id, daemon_id, provider) DO UPDATE SET \
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

/// The id of the host runtime already registered for this daemon's
/// `(workspace, daemon_id, provider)` tuple, or `None` when none exists yet.
///
/// Because a runtime cannot be renamed after first boot (its `id` is an FK
/// target), an existing row's id is the identity the claim loop, the registered
/// row, and every bound agent must all use.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the lookup fails.
pub async fn existing_host_runtime_id(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let Some(workspace_id) = find_default_workspace(pool).await? else {
        return Ok(None);
    };
    sqlx::query_scalar(
        "SELECT id FROM agent_runtime \
         WHERE workspace_id = ? AND daemon_id = ? AND provider = ? LIMIT 1",
    )
    .bind(&workspace_id)
    .bind(SELF_DAEMON_ID)
    .bind(DEFAULT_PROVIDER)
    .fetch_optional(pool)
    .await
}

/// Resolve the runtime id a new agent binds to (and the daemon claims for).
///
/// The EXISTING host runtime's id when one is registered — a runtime cannot be
/// renamed, so its id wins — else the configured/default id
/// ([`default_runtime_id`]) for a brand-new home. Keeping create and claim on this
/// one resolver is what keeps a created agent bound to the runtime the daemon
/// actually claims for.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the lookup fails.
pub async fn resolve_runtime_id(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    Ok(existing_host_runtime_id(pool).await?.unwrap_or_else(default_runtime_id))
}

/// Create one agent from scratch, filling every FK behind the scenes.
///
/// Binds the default runtime (ensuring its row exists), resolves the default
/// owner, mints a fresh id, and inserts. The caller supplies only the human
/// `name` (+ an already-normalised `provider` and optional `instructions`).
///
/// The returned [`Agent`] carries the minted id so a caller can route to it
/// (e.g. as a squad leader). `provider` is recorded on the row and HONOURED at
/// dispatch: the agent binds the single host runtime (an execution slot the claim
/// loop keys off by id, not by provider), and the daemon spawns the recorded
/// provider's backend per task — so a `codex` agent runs codex. The agent binds
/// the EXISTING runtime id when one is registered ([`resolve_runtime_id`]), so a
/// created agent is always on the runtime the daemon actually claims for.
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
    // Bind the runtime the daemon actually claims for: the existing host runtime
    // when one is registered (a runtime cannot be renamed), else the default.
    let runtime_id = resolve_runtime_id(pool).await?;
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

/// Count the workspace's agents (active AND archived).
///
/// The non-clobber guard the boot seed reads before laying down a starter agent:
/// a user who created, renamed, or archived their own agent has count > 0, so the
/// seed skips.
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

        let ws_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(ws_count, 1, "only one workspace row is ever created");
        let user_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user").fetch_one(pool).await.unwrap();
        assert_eq!(user_count, 1, "only one owner user is ever created");
    }

    /// Two concurrent fresh-home writers (the daemon autostart + a racing CLI)
    /// can both pass the find-None; the loser's slug-UNIQUE collision must resolve
    /// to the winner's id, never an error or a duplicate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_ensure_default_workspace_converges_without_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool_a = store.pool().clone();
        let pool_b = store.pool().clone();

        let (a, b) = tokio::join!(
            tokio::spawn(async move { ensure_default_workspace(&pool_a).await }),
            tokio::spawn(async move { ensure_default_workspace(&pool_b).await }),
        );
        let a = a.unwrap().expect("racer A must not error");
        let b = b.unwrap().expect("racer B must not error (slug race resolves to the winner)");
        assert_eq!(a, b, "both racers converge on the one workspace id");

        let ws: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(ws, 1, "no duplicate workspace under concurrency");
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(
            users, 1,
            "the loser rolled back its owner insert (no orphan user)"
        );
    }

    #[tokio::test]
    async fn default_owner_id_after_ensure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        assert!(
            default_owner_id(pool).await.unwrap().is_none(),
            "no user before ensure"
        );
        ensure_default_workspace(pool).await.unwrap();
        assert!(
            default_owner_id(pool).await.unwrap().is_some(),
            "an owner exists after ensure"
        );
    }

    #[tokio::test]
    async fn ensure_runtime_noop_without_workspace_then_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        assert!(
            !ensure_runtime(pool, "default", 1_000).await.unwrap(),
            "no workspace ⇒ no-op"
        );
        ensure_default_workspace(pool).await.unwrap();
        assert!(
            ensure_runtime(pool, "default", 1_000).await.unwrap(),
            "workspace ⇒ upsert"
        );
        // A restart upserts, never duplicates.
        assert!(ensure_runtime(pool, "default", 2_000).await.unwrap());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "a restart upserts the same runtime row");
    }

    /// A runtime CANNOT be renamed after first boot: `agent.runtime_id` is an
    /// ENFORCED FK (sqlx sets `PRAGMA foreign_keys = ON`), so changing the
    /// runtime's `id` while an agent references it raises
    /// `FOREIGN KEY constraint failed`. A second ensure with a DIFFERENT configured
    /// id must therefore refresh the EXISTING row: no error, no rename, no orphan.
    ///
    /// This test deliberately binds a real agent to the runtime — the case a
    /// no-agent test dodges, and which made an earlier `id = excluded.id` upsert
    /// look green while it FK-errored on every populated home.
    #[tokio::test]
    async fn runtime_rename_is_refused_existing_id_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = ensure_default_workspace(pool).await.unwrap();

        // First boot registers the runtime; an agent binds it (the FK that makes a
        // rename impossible).
        assert!(ensure_runtime(pool, "runtime-a", 1_000).await.unwrap());
        let agent = create_agent(pool, &ws, "bound", "claude", None).await.unwrap();
        assert_eq!(
            agent.runtime_id, "runtime-a",
            "the agent binds the existing runtime"
        );

        // (a) A later boot with a DIFFERENT configured id must NOT error.
        assert!(
            ensure_runtime(pool, "runtime-b", 2_000).await.unwrap(),
            "a changed runtime id must refresh the existing row, never FK-error"
        );

        // (b) Still exactly one runtime row.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "still exactly one runtime row");

        // (c) The EXISTING id wins — the rename was refused.
        let row = crate::repo::agent_runtime::AgentRuntimeRepo::get(pool, "runtime-a")
            .await
            .unwrap()
            .expect("the original id still owns the row (a runtime cannot be renamed)");
        assert_eq!(
            row.status, "online",
            "the existing row was refreshed online"
        );
        assert_eq!(row.last_seen_at, Some(2_000), "…with the new heartbeat");
        assert!(
            crate::repo::agent_runtime::AgentRuntimeRepo::get(pool, "runtime-b")
                .await
                .unwrap()
                .is_none(),
            "the configured-but-rejected id never became a row"
        );
        assert_eq!(
            existing_host_runtime_id(pool).await.unwrap().as_deref(),
            Some("runtime-a"),
            "the resolver reports the existing id as the one to bind/claim"
        );

        // (d) The agent is not orphaned: its FK still resolves to a live runtime.
        let still = crate::repo::agent::AgentRepo::get(pool, &agent.id).await.unwrap().unwrap();
        assert_eq!(
            still.runtime_id, "runtime-a",
            "the bound agent is untouched"
        );
        assert!(
            crate::repo::agent_runtime::AgentRuntimeRepo::get(pool, &still.runtime_id)
                .await
                .unwrap()
                .is_some(),
            "the agent's runtime FK still resolves (never orphaned)"
        );
    }

    /// A new agent binds the EXISTING runtime id, not a changed configured/default
    /// one — so a created agent is always on the runtime the daemon claims for.
    #[tokio::test]
    async fn create_agent_binds_the_existing_runtime_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = ensure_default_workspace(pool).await.unwrap();
        // A runtime already exists under a non-default id.
        ensure_runtime(pool, "runtime-existing", 1_000).await.unwrap();

        let agent = create_agent(pool, &ws, "late", "codex", None).await.unwrap();
        assert_eq!(
            agent.runtime_id, "runtime-existing",
            "a created agent binds the existing runtime, not the default id"
        );
    }

    #[tokio::test]
    async fn create_agent_fills_every_fk_and_records_provider() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let ws = ensure_default_workspace(pool).await.unwrap();

        let agent = create_agent(pool, &ws, "reviewer", "codex", None).await.unwrap();
        assert_eq!(agent.name, "reviewer");
        assert_eq!(
            agent.runtime_id,
            default_runtime_id(),
            "binds the default runtime"
        );
        assert_eq!(
            agent.provider.as_deref(),
            Some("codex"),
            "records the chosen provider"
        );
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
        assert_eq!(
            normalize_provider(Some("Codex")).unwrap(),
            "codex",
            "case-insensitive"
        );
        assert_eq!(normalize_provider(Some("copilot")).unwrap(), "copilot");
        assert!(
            normalize_provider(Some("gpt5")).is_err(),
            "unknown provider is rejected"
        );
    }
}
