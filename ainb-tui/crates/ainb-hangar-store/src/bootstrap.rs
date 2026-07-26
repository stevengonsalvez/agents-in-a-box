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
//! Every seeded / created agent binds the id [`ensure_runtime`] returns, and the
//! daemon's claim loop + self-register resolve the SAME id through that same
//! call. If they diverged, an agent would bind a runtime the daemon never claims
//! for and its tasks would never run. That one atomic upsert is the single source
//! of the id, so the callers cannot drift.
//!
//! A runtime **cannot be renamed** after first boot: `agent.runtime_id` is an
//! enforced `REFERENCES agent_runtime(id)` FK (sqlx sets `PRAGMA foreign_keys = ON`),
//! so an already-registered runtime's id always WINS over a changed
//! `HANGAR_DAEMON_RUNTIME_ID` / [`crate::bootstrap::DEFAULT_RUNTIME_ID`]. Only a brand-new
//! home adopts the configured id (again via [`ensure_runtime`], which RETURNS the
//! id it settled on); the daemon warns when it ignores a configured id.
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
/// see the module docs), so this is only the first-boot default; take the id
/// actually in use from [`ensure_runtime`]'s return value.
pub const DEFAULT_RUNTIME_ID: &str = "default";

/// The provider a freshly-seeded starter agent (and the self-registered runtime)
/// advertises by default.
pub const DEFAULT_PROVIDER: &str = "claude";

/// The providers `hangar/agent_create` accepts.
///
/// Each has a real exec path in the daemon's runner, and the chosen value is
/// recorded on the agent row (migration 0041) and HONOURED at dispatch: the agent
/// binds the single host runtime (an execution slot claimed by id, not provider)
/// and the daemon spawns THIS provider's backend per task — a `codex` agent runs
/// codex, a `copilot` agent runs copilot.
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
/// take it from [`ensure_runtime`]'s return value — the seam the seed, the claim
/// loop, and `agent_create` all share.
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
/// is a NOT NULL `REFERENCES agent_runtime(id)` FK and `SQLite` enforces foreign
/// keys (sqlx sets `PRAGMA foreign_keys = ON`), so changing the runtime's `id`
/// while agents reference it would raise `FOREIGN KEY constraint failed`. A
/// runtime therefore CANNOT be renamed after first boot: if a caller passes a
/// different `runtime_id` for an existing `(workspace, daemon, provider)` tuple
/// (e.g. a changed `HANGAR_DAEMON_RUNTIME_ID`), this refreshes the EXISTING row's
/// `status`/`last_seen_at`, keeps its original `id`, and RETURNS that id. Boot
/// takes the daemon's claim id from this same call (see the daemon's
/// `effective_runtime_id`), so the registered row, the agents bound to it, and the
/// claim loop all stay aligned — no drift, no stranding, no FK error.
///
/// Returns `Ok(Some(id))` — the id the row ACTUALLY settled on, which is the
/// pre-existing id when one was already registered and `runtime_id` otherwise —
/// or `Ok(None)` when there is no workspace to attach to yet (a benign no-op).
///
/// Returning the settled id makes this the single atomic resolve+register: a
/// caller binds what demonstrably exists rather than re-reading (a read-then-write
/// race where a concurrent daemon registered a different id between the read and
/// the insert would otherwise FK-fail the caller's insert).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the workspace lookup or the upsert fails. It never
/// FK-errors (the `id` is never rewritten). A `UNIQUE constraint failed:
/// agent_runtime.id` can surface if the CONFIGURED id already exists under a
/// DIFFERENT `(workspace, daemon, provider)` tuple: the `ON CONFLICT` target is the
/// tuple, so a PK collision is not caught by it. That is unreachable in production
/// — nothing outside tests writes a second `agent_runtime` row — and it is a
/// genuine misconfiguration (two daemons/providers claiming one id), so erroring is
/// the right answer.
pub async fn ensure_runtime(
    pool: &SqlitePool,
    runtime_id: &str,
    now_ms: i64,
) -> Result<Option<String>, sqlx::Error> {
    let Some(workspace_id) = find_default_workspace(pool).await? else {
        return Ok(None);
    };
    // One statement: insert-or-refresh and hand back the id that now owns the
    // tuple. `DO UPDATE` (not `DO NOTHING`) is what makes `RETURNING` yield the
    // existing row on the conflict path.
    let settled: String = sqlx::query_scalar(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'online') \
         ON CONFLICT(workspace_id, daemon_id, provider) DO UPDATE SET \
           status = 'online', \
           last_seen_at = excluded.last_seen_at \
         RETURNING id",
    )
    .bind(runtime_id)
    .bind(&workspace_id)
    .bind(SELF_DAEMON_ID)
    .bind(DEFAULT_PROVIDER)
    .bind(SELF_RUNTIME_MODE)
    .bind(now_ms)
    .fetch_one(pool)
    .await?;
    Ok(Some(settled))
}

/// Create one agent from scratch, filling every FK behind the scenes.
///
/// Ensures the host runtime and binds the id that upsert SETTLED on, resolves the
/// default owner, mints a fresh id, and inserts. The caller supplies only the
/// human `name` (+ an already-normalised `provider` and optional `instructions`).
///
/// The returned [`Agent`] carries the minted id so a caller can route to it
/// (e.g. as a squad leader). `provider` is recorded on the row and HONOURED at
/// dispatch: the agent binds the single host runtime (an execution slot the claim
/// loop keys off by id, not by provider), and the daemon spawns the recorded
/// provider's backend per task — so a `codex` agent runs codex. Binding the id
/// [`ensure_runtime`] returned (rather than re-reading it) means the agent is
/// always on a runtime that demonstrably exists — no read-then-write window in
/// which a concurrent daemon could register a different id and FK-fail this insert.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if there is no workspace / owner user yet (the FK
/// could not be filled) or if the runtime upsert / agent insert fails.
pub async fn create_agent(
    pool: &SqlitePool,
    workspace_id: &str,
    name: &str,
    provider: &str,
    instructions: Option<String>,
) -> Result<Agent, sqlx::Error> {
    create_agent_from(
        pool,
        workspace_id,
        AgentDraft {
            name: name.to_string(),
            provider: provider.to_string(),
            instructions,
            ..AgentDraft::default()
        },
    )
    .await
}

/// Everything a create may specify about a new agent (migration 0050).
///
/// `Default` is the plain `claude`, user-kind, no-metadata agent, so a caller sets
/// only what it means and a later column add is one struct field, not a signature
/// break across ~20 call sites.
#[derive(Debug, Clone, Default)]
pub struct AgentDraft {
    /// Human-readable agent name. Must be unique within the workspace (migration
    /// 0050) — a collision surfaces as a `sqlx` error for which
    /// [`crate::repo::agent::is_duplicate_name`] is `true`.
    pub name: String,
    /// Provider token, already through [`normalize_provider`]. Empty = the
    /// [`DEFAULT_PROVIDER`].
    pub provider: String,
    /// Free-form system prompt / instructions.
    pub instructions: Option<String>,
    /// Short blurb (multica 060). Trimmed here; the ≤255-char cap is validated by
    /// the CALLER (handler/CLI) so the user sees a clear message, with the schema
    /// `CHECK` as the last line of defence.
    pub description: String,
    /// Avatar token. Absent/blank mints a random `"emoji:…"` value so an agent is
    /// never avatar-less (multica `newAgentAvatar`).
    pub avatar_url: Option<String>,
    /// Provider model override; `None` = the provider default.
    pub model: Option<String>,
    /// Codex service tier (runtime-native catalog id). Stored + surfaced only.
    pub service_tier: Option<String>,
    /// `None` (the default) = `"user"`. `Some("system")` mints a hidden carrier
    /// agent — internal callers only (gap #9-rest); no RPC exposes this.
    pub kind: Option<String>,
    /// Identity key for a system agent; `None` for user agents.
    pub system_key: Option<String>,
}

/// Multica's 24-emoji avatar palette (`agent_avatar.go:13-25`). One is picked at
/// create when the caller supplies no avatar, so every agent renders a glyph.
const AVATAR_EMOJI: [&str; 24] = [
    "🐙", "🦊", "🦉", "🐝", "🐼", "🐸", "🐯", "🦁", "🐨", "🐵", "🐧", "🐳", "🦋", "🌞", "🌙", "⭐",
    "🔥", "⚡", "🍀", "🌈", "🚀", "🤖", "👾", "🧠",
];

/// Mint a `"emoji:<glyph>"` avatar token from [`AVATAR_EMOJI`].
///
/// The pick is derived from the wall clock rather than an RNG dependency — the
/// value is cosmetic, so "varies between creates" is the whole requirement.
fn random_emoji_avatar() -> String {
    let idx = usize::try_from(SystemClock.now_ms().unsigned_abs() % AVATAR_EMOJI.len() as u64)
        .unwrap_or(0);
    format!("emoji:{}", AVATAR_EMOJI[idx])
}

/// Create one agent from a [`AgentDraft`], filling every FK behind the scenes
/// (migration 0050's metadata-aware entry point).
///
/// Keeps every invariant of [`create_agent`] (ensure the runtime and bind the id
/// that upsert SETTLED on, resolve the default owner, mint a ULID,
/// `visibility: "workspace"`, `permission_mode: "private"`) and additionally
/// defaults the avatar to a random emoji token, trims the description, and
/// defaults `kind` to `"user"`.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if there is no workspace / owner user yet, if the
/// runtime upsert fails, or if the insert fails — notably the migration-0050
/// `(workspace_id, name)` UNIQUE violation, for which
/// [`crate::repo::agent::is_duplicate_name`] is `true`.
pub async fn create_agent_from(
    pool: &SqlitePool,
    workspace_id: &str,
    draft: AgentDraft,
) -> Result<Agent, sqlx::Error> {
    let now = SystemClock.now_ms();
    // ONE atomic upsert: ensure the runtime FK exists (a fresh home may have none
    // — the CLI create path runs with no daemon) and take back the id it settled
    // on, which is the pre-existing runtime's id when one is already registered
    // (a runtime cannot be renamed) and the configured default otherwise.
    let runtime_id = ensure_runtime(pool, &default_runtime_id(), now)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

    let owner_id = default_owner_id(pool).await?.ok_or_else(|| sqlx::Error::RowNotFound)?;

    let provider = if draft.provider.is_empty() {
        DEFAULT_PROVIDER.to_string()
    } else {
        draft.provider
    };
    // An agent is never avatar-less: a blank/absent token mints one (multica
    // `newAgentAvatar`), so every roster row renders a glyph.
    let avatar_url = draft
        .avatar_url
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(random_emoji_avatar);

    let agent = Agent {
        id: SystemIdGen.new_ulid(),
        workspace_id: workspace_id.to_string(),
        name: draft.name,
        runtime_id,
        instructions: draft.instructions,
        visibility: "workspace".to_string(),
        // Deny-by-default invocation (migration 0047): a freshly created agent is
        // private (owner-only) until explicitly shared via an invocation target.
        // The owner-invoked TUI Run always passes the gate, so this is invisible to
        // the single-operator path.
        permission_mode: "private".to_string(),
        owner_id,
        archived: false,
        model: draft.model,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: Some(provider),
        token_budget: None,
        description: draft.description.trim().to_string(),
        avatar_url: Some(avatar_url),
        kind: draft.kind.unwrap_or_else(|| crate::repo::agent::AGENT_KIND_USER.to_string()),
        system_key: draft.system_key,
        service_tier: draft.service_tier,
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

        assert_eq!(
            ensure_runtime(pool, "default", 1_000).await.unwrap(),
            None,
            "no workspace ⇒ no-op"
        );
        ensure_default_workspace(pool).await.unwrap();
        assert_eq!(
            ensure_runtime(pool, "default", 1_000).await.unwrap().as_deref(),
            Some("default"),
            "workspace ⇒ upsert, returning the id it settled on"
        );
        // A restart upserts, never duplicates.
        assert_eq!(
            ensure_runtime(pool, "default", 2_000).await.unwrap().as_deref(),
            Some("default")
        );
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
        assert_eq!(
            ensure_runtime(pool, "runtime-a", 1_000).await.unwrap().as_deref(),
            Some("runtime-a")
        );
        let agent = create_agent(pool, &ws, "bound", "claude", None).await.unwrap();
        assert_eq!(
            agent.runtime_id, "runtime-a",
            "the agent binds the existing runtime"
        );

        // (a) A later boot with a DIFFERENT configured id must NOT error, and must
        //     report back the EXISTING id it settled on (never the configured one).
        assert_eq!(
            ensure_runtime(pool, "runtime-b", 2_000).await.unwrap().as_deref(),
            Some("runtime-a"),
            "a changed runtime id must refresh the existing row (never FK-error) and \
             return the id actually in use"
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
        // A fresh agent created AFTER the rename attempt still binds the existing
        // runtime — create takes its id from the same atomic ensure.
        let later = create_agent(pool, &ws, "later", "codex", None).await.unwrap();
        assert_eq!(
            later.runtime_id, "runtime-a",
            "a later create binds the id in use, not the configured one"
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
