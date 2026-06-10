//! Cross-tenant DATA isolation for the snapshot list queries (e38.26).
//!
//! The Hangar parity review flagged that cross-tenant LIST leakage may pass
//! today because `id == slug` test fixtures mask a slug-vs-row-id resolution
//! bug: real workspaces are created with a ULID `id` distinct from their wire
//! `slug`, and the snapshot list queries scope by the resolved **row id**, not
//! the slug. A query that (wrongly) filtered by the wire slug would still pass
//! every fixture where `id == slug`.
//!
//! This test seeds TWO workspaces whose `id` deliberately differs from their
//! `slug` (`01JWXACME...`/`acme`, `01JWXGLOBEX...`/`globex`), puts one issue +
//! one agent in each, and proves the store-layer list queries are watertight:
//!
//! 1. POSITIVE: listing by workspace A's row id returns A's rows.
//! 2. NEGATIVE: that same list NEVER contains B's rows (and vice-versa).
//! 3. LEAK GUARD: listing by the wire **slug** (`"acme"`/`"globex"`) instead of
//!    the resolved row id returns ZERO rows — so a query that filtered by slug
//!    would fail assertion (2)/(3) loudly here, not leak silently in prod.
//!
//! The store list methods (`IssueRepo::list_by_workspace_state`,
//! `AgentRepo::list_by_workspace`) take a `workspace_id` row id; the daemon's
//! `resolve_workspace_id` maps a wire slug → row id *before* calling them. This
//! test exercises the store contract directly: the authoritative tenancy seam.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

/// One tenant's identifying pair: a ULID-style row `id` distinct from its wire
/// `slug`. Mirrors how real workspaces are minted (id != slug) — the exact
/// shape an `id == slug` fixture would mask.
struct Tenant {
    /// The ULID-style primary key the rows are actually scoped by.
    id: &'static str,
    /// The wire slug the plugin subscribes by (NEVER what rows are keyed on).
    slug: &'static str,
}

/// Workspace A: `acme`. id != slug on purpose.
const ACME: Tenant = Tenant {
    id: "01JWXACME00000000000000AA",
    slug: "acme",
};
/// Workspace B: `globex`. id != slug on purpose.
const GLOBEX: Tenant = Tenant {
    id: "01JWXGLOBEX000000000000BB",
    slug: "globex",
};

/// The issue title seeded into ACME (must never surface under GLOBEX).
const ACME_ISSUE_TITLE: &str = "Refactor API";
/// The issue title seeded into GLOBEX (must never surface under ACME).
const GLOBEX_ISSUE_TITLE: &str = "Acme Login Bug";

/// The agent name seeded into ACME (must never surface under GLOBEX).
const ACME_AGENT_NAME: &str = "acme-agent";
/// The agent name seeded into GLOBEX (must never surface under ACME).
const GLOBEX_AGENT_NAME: &str = "globex-agent";

/// The lifecycle state both seeded issues carry (the list query filters by it).
const STATE_OPEN: &str = "open";

/// Insert one workspace row with a row `id` distinct from its `slug`.
async fn seed_workspace(store: &Store, t: &Tenant) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(t.id)
        .bind(t.slug)
        .bind(t.slug) // display name irrelevant to the test
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
}

/// Insert one user (the agent's owner) so the agent row is well-formed.
async fn seed_user(store: &Store, user_id: &str) {
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(user_id)
        .bind(format!("{user_id}@example.com"))
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert user");
}

/// Insert one open issue (`title`) scoped to `workspace_id`.
async fn seed_issue(store: &Store, id: &str, workspace_id: &str, title: &str) {
    let creator = ActorRef::new(ActorKind::Member, "u-creator").expect("creator ref");
    IssueRepo::insert(
        store.pool(),
        &NewIssue {
            id: id.to_string(),
            workspace_id: workspace_id.to_string(),
            title: title.to_string(),
            description: None,
            state: STATE_OPEN.to_string(),
            assignee: None,
            creator,
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
        },
    )
    .await
    .expect("insert issue");
}

/// Insert one runtime + one agent (`name`) scoped to `workspace_id`.
async fn seed_agent(
    store: &Store,
    workspace_id: &str,
    runtime_id: &str,
    agent_id: &str,
    name: &str,
    owner_id: &str,
) {
    AgentRuntimeRepo::insert(
        store.pool(),
        &AgentRuntime {
            id: runtime_id.to_string(),
            workspace_id: workspace_id.to_string(),
            daemon_id: "daemon-1".to_string(),
            provider: "claude".to_string(),
            runtime_mode: "local".to_string(),
            last_seen_at: Some(0),
            status: "online".to_string(),
        },
    )
    .await
    .expect("insert runtime");
    AgentRepo::insert(
        store.pool(),
        &Agent {
            id: agent_id.to_string(),
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            runtime_id: runtime_id.to_string(),
            instructions: None,
            visibility: "workspace".to_string(),
            owner_id: owner_id.to_string(),
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
        },
    )
    .await
    .expect("insert agent");
}

/// Stand up a store seeded with the two `id != slug` tenants, each carrying one
/// issue + one agent.
async fn seed_two_tenants(store: &Store) {
    seed_workspace(store, &ACME).await;
    seed_workspace(store, &GLOBEX).await;
    seed_user(store, "u-acme").await;
    seed_user(store, "u-globex").await;

    seed_issue(store, "issue-acme", ACME.id, ACME_ISSUE_TITLE).await;
    seed_issue(store, "issue-globex", GLOBEX.id, GLOBEX_ISSUE_TITLE).await;

    seed_agent(
        store,
        ACME.id,
        "rt-acme",
        "agent-acme",
        ACME_AGENT_NAME,
        "u-acme",
    )
    .await;
    seed_agent(
        store,
        GLOBEX.id,
        "rt-globex",
        "agent-globex",
        GLOBEX_AGENT_NAME,
        "u-globex",
    )
    .await;
}

#[tokio::test]
async fn issues_list_is_scoped_to_the_resolved_row_id_never_the_sibling_tenant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_two_tenants(&store).await;

    // Scope to ACME by its row id.
    let acme = IssueRepo::list_by_workspace_state(store.pool(), ACME.id, STATE_OPEN)
        .await
        .expect("list acme issues");
    // POSITIVE: ACME sees its own issue.
    assert!(
        acme.iter().any(|i| i.title == ACME_ISSUE_TITLE),
        "ACME must see its own '{ACME_ISSUE_TITLE}': {acme:?}"
    );
    // NEGATIVE: ACME NEVER sees GLOBEX's issue (cross-tenant leak guard).
    assert!(
        !acme.iter().any(|i| i.title == GLOBEX_ISSUE_TITLE),
        "cross-tenant leak: ACME saw GLOBEX's '{GLOBEX_ISSUE_TITLE}': {acme:?}"
    );
    assert!(
        acme.iter().all(|i| i.workspace_id == ACME.id),
        "every ACME row must carry ACME's workspace_id: {acme:?}"
    );

    // Scope to GLOBEX by its row id — the mirror image.
    let globex = IssueRepo::list_by_workspace_state(store.pool(), GLOBEX.id, STATE_OPEN)
        .await
        .expect("list globex issues");
    assert!(
        globex.iter().any(|i| i.title == GLOBEX_ISSUE_TITLE),
        "GLOBEX must see its own '{GLOBEX_ISSUE_TITLE}': {globex:?}"
    );
    assert!(
        !globex.iter().any(|i| i.title == ACME_ISSUE_TITLE),
        "cross-tenant leak: GLOBEX saw ACME's '{ACME_ISSUE_TITLE}': {globex:?}"
    );
    assert!(
        globex.iter().all(|i| i.workspace_id == GLOBEX.id),
        "every GLOBEX row must carry GLOBEX's workspace_id: {globex:?}"
    );

    // LEAK GUARD: the rows are keyed by row id, NOT the wire slug. A query that
    // (wrongly) scoped issues by `slug` would return ACME's issue when handed
    // `"acme"`; here, listing by the slug must return ZERO rows. If the store
    // ever regressed to slug-keying, the NEGATIVE assertions above would fire.
    let by_slug = IssueRepo::list_by_workspace_state(store.pool(), ACME.slug, STATE_OPEN)
        .await
        .expect("list by slug");
    assert!(
        by_slug.is_empty(),
        "rows must be keyed by row id, not the wire slug `{}`: {by_slug:?}",
        ACME.slug
    );
}

#[tokio::test]
async fn agents_list_is_scoped_to_the_resolved_row_id_never_the_sibling_tenant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_two_tenants(&store).await;

    // Scope to ACME by its row id.
    let acme = AgentRepo::list_by_workspace(store.pool(), ACME.id)
        .await
        .expect("list acme agents");
    // POSITIVE: ACME sees its own agent.
    assert!(
        acme.iter().any(|a| a.name == ACME_AGENT_NAME),
        "ACME must see its own '{ACME_AGENT_NAME}': {acme:?}"
    );
    // NEGATIVE: ACME NEVER sees GLOBEX's agent (cross-tenant leak guard).
    assert!(
        !acme.iter().any(|a| a.name == GLOBEX_AGENT_NAME),
        "cross-tenant leak: ACME saw GLOBEX's '{GLOBEX_AGENT_NAME}': {acme:?}"
    );
    assert!(
        acme.iter().all(|a| a.workspace_id == ACME.id),
        "every ACME agent must carry ACME's workspace_id: {acme:?}"
    );

    // Scope to GLOBEX by its row id — the mirror image.
    let globex = AgentRepo::list_by_workspace(store.pool(), GLOBEX.id)
        .await
        .expect("list globex agents");
    assert!(
        globex.iter().any(|a| a.name == GLOBEX_AGENT_NAME),
        "GLOBEX must see its own '{GLOBEX_AGENT_NAME}': {globex:?}"
    );
    assert!(
        !globex.iter().any(|a| a.name == ACME_AGENT_NAME),
        "cross-tenant leak: GLOBEX saw ACME's '{ACME_AGENT_NAME}': {globex:?}"
    );
    assert!(
        globex.iter().all(|a| a.workspace_id == GLOBEX.id),
        "every GLOBEX agent must carry GLOBEX's workspace_id: {globex:?}"
    );

    // LEAK GUARD: agents are keyed by row id, not the wire slug. Listing by the
    // slug must return ZERO rows; a slug-keyed regression would leak the sibling
    // tenant's agent into the NEGATIVE assertions above.
    let by_slug = AgentRepo::list_by_workspace(store.pool(), ACME.slug)
        .await
        .expect("list agents by slug");
    assert!(
        by_slug.is_empty(),
        "agents must be keyed by row id, not the wire slug `{}`: {by_slug:?}",
        ACME.slug
    );
}
