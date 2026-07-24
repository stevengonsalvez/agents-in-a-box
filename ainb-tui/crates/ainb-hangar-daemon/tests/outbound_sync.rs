//! Integration tests for the Hangar → Beads outbound sync (P2.3).
//!
//! Each test drives [`OutboundSync`] against an ephemeral sqlite store and a
//! fake `bd` shell-script (reused from `beads_adapter::fake_bd`), asserting
//! three things end-to-end:
//!
//! 1. the right `bd` verb + flags reached the binary (via an argv probe file),
//! 2. a `beads_mapping` row was (or was not) persisted, with the bd id `bd`
//!    returned, and
//! 3. source-gating, idempotency, assignee crosswalk, and non-fatal failure all
//!    behave per the P2.3 contract.
//!
//! The fake-bd fixtures are hermetic — no live `bd` binary is required.

use std::fs;

use chrono::{TimeZone, Utc};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::beads_adapter::{BdClient, fake_bd};
use ainb_hangar_daemon::beads_sync::outbound::{MirrorOutcome, OutboundSync};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};

/// Frozen clock value used for every test's `last_synced` stamp.
const T0_MS: i64 = 1_700_000_000_000;

/// Seed the workspace row every issue references, then return its id.
async fn seed_workspace(store: &Store) -> String {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
    "ws-1".to_string()
}

/// Insert an open Hangar issue with the given id/title/assignee and return it.
async fn seed_issue(
    store: &Store,
    ws: &str,
    id: &str,
    title: &str,
    assignee: Option<ActorRef>,
) -> Issue {
    let new = NewIssue {
        id: id.to_string(),
        workspace_id: ws.to_string(),
        title: title.to_string(),
        description: Some("d".to_string()),
        state: "open".to_string(),
        assignee,
        creator: ActorRef::new(ActorKind::Member, "stevie").expect("actor"),
        created_at: T0_MS,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        parent_issue_id: None,
        stage: None,
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");
    IssueRepo::get_by_id(store.pool(), id)
        .await
        .expect("get issue")
        .expect("issue present")
}

/// Read the argv probe file the capturing fake-bd wrote (one arg per line).
fn probe_args(probe: &std::path::Path) -> Vec<String> {
    fs::read_to_string(probe)
        .expect("probe file written")
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn test_issue_create_mirrors_to_bd() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_1", "ship it", None).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-abc-123", "ship it", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    let sync = OutboundSync::new(&bd, &mapping, &clock);
    let outcome = sync
        .mirror_create(&issue, MappingSource::Hangar, &["foo".into()])
        .await
        .expect("mirror create");

    assert!(matches!(outcome, MirrorOutcome::Mirrored));

    // fake-bd saw a `create` invocation.
    let args = probe_args(&probe);
    assert!(args.contains(&"create".to_string()), "args: {args:?}");
    assert!(args.contains(&"ship it".to_string()), "args: {args:?}");

    // A mapping row now correlates the two ids.
    let row = mapping
        .find_by_hangar("iss_1")
        .await
        .expect("query")
        .expect("mapping row present");
    assert_eq!(row.bd_id, "bd-abc-123");
    assert_eq!(row.source, MappingSource::Hangar);
}

#[tokio::test]
async fn test_issue_close_mirrors_to_bd_close() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_2", "close me", None).await;

    // First create so a mapping row exists, then close.
    let create_bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-close-1", "close me", "open");
    let bd_create = BdClient::new(create_bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);
    OutboundSync::new(&bd_create, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await
        .expect("seed create");

    // Now close: use a capturing fake that echoes a closed issue.
    let close_bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-close-1", "close me", "closed");
    let bd_close = BdClient::new(close_bin, beads_dir.path().to_path_buf()).expect("client");
    OutboundSync::new(&bd_close, &mapping, &clock)
        .mirror_close(&issue)
        .await
        .expect("mirror close");

    let args = probe_args(&probe);
    assert!(args.contains(&"close".to_string()), "args: {args:?}");
    assert!(args.contains(&"bd-close-1".to_string()), "args: {args:?}");
}

#[tokio::test]
async fn test_swarm_sourced_issue_skips_outbound() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_swarm", "swarm task", None).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-x", "swarm task", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    let outcome = OutboundSync::new(&bd, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Swarm, &[])
        .await
        .expect("mirror create");

    assert!(matches!(outcome, MirrorOutcome::SkippedSwarm));
    // Zero bd calls — the probe file was never written.
    assert!(!probe.exists(), "fake-bd must not have been invoked");
    // No mapping row.
    assert!(mapping.find_by_hangar("iss_swarm").await.expect("query").is_none());
}

#[tokio::test]
async fn test_outbound_failure_does_not_block_hangar_write() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    // The hangar write already happened (seed_issue); mirror runs after.
    let issue = seed_issue(&store, &ws, "iss_fail", "boom", None).await;

    let bin = fake_bd::failing(bin_dir.path(), "bd exploded");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    let result = OutboundSync::new(&bd, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await;

    // Mirror surfaces the error (caller logs + continues) ...
    assert!(result.is_err(), "expected a SyncError on bd failure");
    // ... but the hangar issue is untouched and no half-written mapping row exists.
    assert!(
        IssueRepo::get_by_id(store.pool(), "iss_fail")
            .await
            .expect("get issue")
            .is_some()
    );
    assert!(mapping.find_by_hangar("iss_fail").await.expect("query").is_none());
}

#[tokio::test]
async fn test_polymorphic_assignee_agent_prefix() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let agent = ActorRef::new(ActorKind::Agent, "ag_42").expect("actor");
    let issue = seed_issue(&store, &ws, "iss_ag", "agent task", Some(agent)).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-ag", "agent task", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    OutboundSync::new(&bd, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await
        .expect("mirror create");

    let args = probe_args(&probe);
    let i = args.iter().position(|a| a == "--assignee").expect("--assignee flag present");
    assert_eq!(args[i + 1], "agent:ag_42");
}

#[tokio::test]
async fn test_polymorphic_assignee_user_raw() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let member = ActorRef::new(ActorKind::Member, "stevie").expect("actor");
    let issue = seed_issue(&store, &ws, "iss_user", "user task", Some(member)).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-user", "user task", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    OutboundSync::new(&bd, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await
        .expect("mirror create");

    let args = probe_args(&probe);
    let i = args.iter().position(|a| a == "--assignee").expect("--assignee flag present");
    assert_eq!(args[i + 1], "stevie");
}

#[tokio::test]
async fn test_labels_round_trip() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_lbl", "labelled", None).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-lbl", "labelled", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    OutboundSync::new(&bd, &mapping, &clock)
        .mirror_create(&issue, MappingSource::Hangar, &["foo".into(), "bar".into()])
        .await
        .expect("mirror create");

    // `bd create` takes a single comma-joined `--labels` flag (per cli.rs).
    let args = probe_args(&probe);
    let i = args.iter().position(|a| a == "--labels").expect("--labels flag present");
    assert_eq!(args[i + 1], "foo,bar");
}

#[tokio::test]
async fn test_idempotent_replay() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_idem", "once", None).await;

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-idem", "once", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);
    let sync = OutboundSync::new(&bd, &mapping, &clock);

    let first = sync
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await
        .expect("first create");
    assert!(matches!(first, MirrorOutcome::Mirrored));

    // Delete the probe so we can prove the 2nd call does NOT invoke bd again.
    fs::remove_file(&probe).expect("rm probe");

    let second = sync
        .mirror_create(&issue, MappingSource::Hangar, &[])
        .await
        .expect("second create");
    assert!(
        matches!(second, MirrorOutcome::AlreadyMirrored),
        "second call must short-circuit"
    );
    assert!(!probe.exists(), "second create must not invoke bd");
}

#[tokio::test]
async fn test_close_skips_swarm_sourced_mapping_row() {
    // A `Swarm`-sourced mapping row is owned by a swarm leader's own `bd`
    // lifecycle; `mirror_close` must short-circuit (return `SkippedSwarm`) and
    // never issue a `bd close`. This seeds such a row directly (the outbound
    // create path never persists one — Swarm short-circuits before insert) to
    // pin the defensive close-side guard.
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");
    let probe = bin_dir.path().join("argv.probe");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_swarm_close", "swarm close", None).await;

    let mapping = BeadsMappingRepo::new(store.pool());
    // Seed a Swarm-sourced correlation row by hand.
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: issue.id.clone(),
            bd_id: "bd-swarm-1".to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Swarm,
            last_synced: Utc.timestamp_millis_opt(T0_MS).single().expect("ts"),
        })
        .await
        .expect("seed swarm mapping row");

    let bin = fake_bd::capturing(bin_dir.path(), &probe, "bd-swarm-1", "swarm close", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T0_MS);

    let outcome = OutboundSync::new(&bd, &mapping, &clock)
        .mirror_close(&issue)
        .await
        .expect("mirror close");

    assert!(matches!(outcome, MirrorOutcome::SkippedSwarm));
    // No `bd close` happened — the probe file was never written.
    assert!(
        !probe.exists(),
        "fake-bd must not have been invoked for a swarm row"
    );
}
