//! The autopilot ACCOUNTABILITY LEDGER at the store layer (multica parity #14,
//! migration 0061).
//!
//! Before this item hangar had no versioning table, no `published_by`, and — the
//! deeper gap — **no edit surface at all**: an autopilot's cron, instructions,
//! agent or policy could not be changed without hand-editing sqlite, so the
//! acceptance sentence ("editing an autopilot rule creates a new version row
//! with who changed it") had no code path to hang off.
//!
//! These tests pin the ledger's contract against real sqlite with an injected
//! clock:
//!
//! - creation is a TRANSACTION that also writes v1, so there is never an
//!   autopilot with no accountable human;
//! - a SUBSTANTIVE edit mints a version naming the EDITING human (who may be a
//!   different person from the creator — the headline assert);
//! - a COSMETIC edit (a rename) mints NONE, while still landing the rename;
//! - a rejected edit (malformed cron) leaves both the row AND the ledger
//!   untouched;
//! - the legacy no-actor delegates still mint a version, unattributed, so the
//!   invariant has no hole;
//! - everything is workspace-scoped.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    AutopilotEdit, AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot, UpdateOutcome,
};
use ainb_hangar_store::repo::autopilot_rule_version::{AutopilotRuleVersionRepo, RuleVersion};

/// Fixed clock instant every test publishes at (epoch-ms, 2026-01-01T00:00:00Z).
const T0: i64 = 1_767_225_600_000;

fn ws1() -> WorkspaceId {
    WorkspaceId::from_str("ws-1").unwrap()
}
fn ws2() -> WorkspaceId {
    WorkspaceId::from_str("ws-2").unwrap()
}
fn alice() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-alice").unwrap()
}
fn bob() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-bob").unwrap()
}

/// Seed the workspace + user + runtime + agent FK chain an autopilot needs.
async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','beta','Beta',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-alice','alice@example.com',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-bob','bob@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-alice')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-2','ws-1','Other','rt-1','workspace','user-alice')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

async fn open(dir: &tempfile::TempDir) -> Store {
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    store
}

fn new_autopilot(name: &str) -> NewAutopilot {
    NewAutopilot {
        workspace_id: ws1(),
        agent_id: AgentId::from_str("agent-1").unwrap(),
        name: name.to_string(),
        instructions: Some("v1 instructions".to_string()),
        cron_expr: "0 9 * * 1-5".to_string(),
        max_concurrent_runs: 1,
        execution_mode: ExecutionMode::RunOnly,
        concurrency_policy: ConcurrencyPolicy::Skip,
        api_trigger_enabled: false,
    }
}

/// Every ledger row for `id`, OLDEST-first (the list read is newest-first).
async fn versions(store: &Store, id: &AutopilotId) -> Vec<RuleVersion> {
    let mut rows = AutopilotRuleVersionRepo::list(store.pool(), &ws1(), id, 100)
        .await
        .expect("list versions");
    rows.reverse();
    rows
}

/// The `changed` array of a ledger row's `config_summary`.
fn changed(v: &RuleVersion) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(&v.config_summary)
        .expect("config_summary is JSON")
        .get("changed")
        .and_then(|c| c.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|x| x.as_str().unwrap_or_default().to_string())
        .collect()
}

/// A.1 — creation is a TRANSACTION that also writes rule-version v1. There is no
/// window in which an autopilot exists with no accountable human.
#[tokio::test]
async fn create_as_writes_exactly_one_v1_row_naming_the_creator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;

    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create autopilot");

    let rows = versions(&store, &id).await;
    assert_eq!(rows.len(), 1, "creation mints exactly one version");
    assert_eq!(rows[0].version, 1);
    assert_eq!(rows[0].change_kind, "created");
    assert_eq!(rows[0].published_by.as_deref(), Some("member:user-alice"));
    assert_eq!(rows[0].created_at, T0, "the injected clock is used");
    assert!(
        changed(&rows[0]).is_empty(),
        "a create has no before-state, so nothing is 'changed'"
    );

    // `config_summary` round-trips the created config, so the ledger is a real
    // snapshot, not just a pointer at the (mutable) live row.
    let summary: serde_json::Value =
        serde_json::from_str(&rows[0].config_summary).expect("summary JSON");
    assert_eq!(summary["cron_expr"], "0 9 * * 1-5");
    assert_eq!(summary["instructions"], "v1 instructions");
    assert_eq!(summary["agent_id"], "agent-1");
    assert_eq!(summary["enabled"], true);
}

/// A.2 — THE HEADLINE ACCEPTANCE: editing an autopilot rule creates a new
/// version row naming WHO CHANGED IT — a different human from the creator. The
/// ledger records the editor, not merely the owner.
#[tokio::test]
async fn update_as_mints_v2_naming_the_editing_human_not_the_creator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");

    let outcome = AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            instructions: Some(Some("v2 instructions".to_string())),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("update");
    assert_eq!(outcome, UpdateOutcome::Updated { version: Some(2) });

    let rows = versions(&store, &id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].version, 2);
    assert_eq!(rows[1].change_kind, "instructions");
    assert_eq!(
        rows[1].published_by.as_deref(),
        Some("member:user-bob"),
        "the ledger names the EDITOR (bob), not the creator (alice)"
    );
    assert_eq!(changed(&rows[1]), vec!["instructions".to_string()]);

    // ...and the edit actually landed.
    let ap = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(ap.instructions.as_deref(), Some("v2 instructions"));
}

/// A.3 — THE COSMETIC RULE, proven BOTH ways: a rename-only edit mints NO new
/// version row, AND the name really does change. Cosmetic means *unversioned*,
/// not *rejected* — a title tweak must never re-assign blame for an unattended
/// run.
#[tokio::test]
async fn a_rename_only_edit_lands_but_mints_no_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");
    AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            instructions: Some(Some("v2 instructions".to_string())),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("substantive update");
    assert_eq!(versions(&store, &id).await.len(), 2);

    let outcome = AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 2_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            name: Some("nightly-renamed".to_string()),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("cosmetic update");

    assert_eq!(
        outcome,
        UpdateOutcome::Updated { version: None },
        "a cosmetic edit reports no minted version"
    );
    assert_eq!(
        versions(&store, &id).await.len(),
        2,
        "the ledger did NOT grow for a rename"
    );
    let ap = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(
        ap.name, "nightly-renamed",
        "the rename still LANDED — cosmetic means unversioned, not rejected"
    );
}

/// A.4 + A.5 — pausing, resuming and arming a trigger are all SUBSTANTIVE
/// publishes, each carrying its own actor.
#[tokio::test]
async fn pause_resume_and_trigger_each_mint_their_own_attributed_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");

    AutopilotRepo::disable_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        Some(&bob()),
    )
    .await
    .expect("disable");
    AutopilotRepo::enable_as(
        store.pool(),
        &FixedClock(T0 + 2_000),
        &ws1(),
        &id,
        Some(&alice()),
    )
    .await
    .expect("enable");
    AutopilotRepo::set_api_trigger_enabled_as(
        store.pool(),
        &FixedClock(T0 + 3_000),
        &ws1(),
        &id,
        true,
        Some(&bob()),
    )
    .await
    .expect("arm api trigger");

    let rows = versions(&store, &id).await;
    let seq: Vec<(i64, &str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.version, r.change_kind.as_str(), r.published_by.as_deref()))
        .collect();
    assert_eq!(
        seq,
        vec![
            (1, "created", Some("member:user-alice")),
            (2, "paused", Some("member:user-bob")),
            (3, "resumed", Some("member:user-alice")),
            (4, "trigger", Some("member:user-bob")),
        ]
    );

    // The newest row is what dispatch reads for the accountable human.
    let latest = AutopilotRuleVersionRepo::latest(store.pool(), &ws1(), &id)
        .await
        .expect("latest")
        .expect("present");
    assert_eq!(latest.version, 4);
    assert_eq!(latest.published_by.as_deref(), Some("member:user-bob"));
}

/// A.6 — THE TRANSACTIONAL GUARANTEE. A malformed cron leaves the `autopilot`
/// row byte-identical AND adds no version row. (Delete the tx wrapper in
/// `update_as` and this goes red.)
#[tokio::test]
async fn a_rejected_cron_writes_neither_the_row_nor_a_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");
    let before = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");

    let err = AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            // A rename bundled with the bad cron: neither may land.
            name: Some("should-not-land".to_string()),
            cron_expr: Some("not a cron at all".to_string()),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect_err("a malformed cron must be rejected");
    assert!(
        matches!(
            err,
            ainb_hangar_store::repo::autopilot::AutopilotRepoError::Cron(_)
        ),
        "expected a Cron error, got {err:?}"
    );

    let after = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(before, after, "the autopilot row must be untouched");
    assert_eq!(
        versions(&store, &id).await.len(),
        1,
        "a rejected edit must leave no orphan version row"
    );
}

/// A.7 — the legacy no-actor delegates still mint a version, UNATTRIBUTED. The
/// invariant has no hole: every mutation is in the ledger; a legacy caller just
/// records an honest "we don't know who".
#[tokio::test]
async fn the_legacy_delegates_still_mint_unattributed_versions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;

    let id = AutopilotRepo::create(store.pool(), &FixedClock(T0), &new_autopilot("nightly"))
        .await
        .expect("legacy create");

    let rows = versions(&store, &id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].version, 1);
    assert_eq!(rows[0].change_kind, "created");
    assert_eq!(
        rows[0].published_by, None,
        "no actor means NULL, never a fabricated human"
    );
}

/// A.8 — tenant scoping. A foreign workspace can neither edit nor read the
/// ledger.
#[tokio::test]
async fn a_foreign_workspace_can_neither_edit_nor_read_the_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");

    let outcome = AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws2(),
        &id,
        &AutopilotEdit {
            instructions: Some(Some("hijacked".to_string())),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("update against a foreign workspace");
    assert_eq!(outcome, UpdateOutcome::NotFound);

    // Nothing written, nothing leaked.
    assert_eq!(versions(&store, &id).await.len(), 1);
    assert!(
        AutopilotRuleVersionRepo::list(store.pool(), &ws2(), &id, 100)
            .await
            .expect("foreign list")
            .is_empty(),
        "a foreign workspace reads an empty ledger"
    );
    assert!(
        AutopilotRuleVersionRepo::latest(store.pool(), &ws2(), &id)
            .await
            .expect("foreign latest")
            .is_none()
    );
    let ap = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(ap.instructions.as_deref(), Some("v1 instructions"));
}

/// A.9 — a multi-field edit mints EXACTLY ONE row (precedence picks the kind),
/// and `config_summary.changed` still lists every field that moved, so nothing
/// is lost.
#[tokio::test]
async fn a_multi_field_edit_mints_one_row_but_records_every_changed_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");

    let outcome = AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            cron_expr: Some("0 10 * * *".to_string()),
            instructions: Some(Some("v2 instructions".to_string())),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("multi-field update");
    assert_eq!(outcome, UpdateOutcome::Updated { version: Some(2) });

    let rows = versions(&store, &id).await;
    assert_eq!(rows.len(), 2, "one edit ⇒ exactly one ledger row");
    assert_eq!(
        rows[1].change_kind, "schedule",
        "Schedule outranks Instructions"
    );
    assert_eq!(
        changed(&rows[1]),
        vec!["cron_expr".to_string(), "instructions".to_string()],
        "the full changed list survives the single-kind collapse"
    );

    // The schedule change really recomputed the next tick, strictly after now.
    let ap = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(ap.cron_expr, "0 10 * * *");
    assert!(
        ap.next_tick_at.is_some_and(|t| t > T0 + 1_000),
        "next_tick_at recomputes strictly after the edit instant"
    );
}

/// Re-targeting the rule at a different agent is a `target` publish — the
/// highest-precedence substantive change, since it changes WHO runs unattended.
#[tokio::test]
async fn re_targeting_the_agent_is_a_target_publish() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&alice()),
    )
    .await
    .expect("create");

    AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &AutopilotEdit {
            agent_id: Some(AgentId::from_str("agent-2").unwrap()),
            // Bundled with a lower-precedence change, to pin the ordering.
            max_concurrent_runs: Some(4),
            ..AutopilotEdit::default()
        },
        Some(&bob()),
    )
    .await
    .expect("re-target");

    let rows = versions(&store, &id).await;
    assert_eq!(rows[1].change_kind, "target");
    assert_eq!(
        changed(&rows[1]),
        vec!["agent_id".to_string(), "max_concurrent_runs".to_string()]
    );
}
