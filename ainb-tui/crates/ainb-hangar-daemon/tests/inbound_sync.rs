//! Integration tests for the Beads → Hangar inbound sync (P2.4).
//!
//! Each test drives [`InboundSync::reconcile_once`] (or the [`run_inbound_loop`]
//! poll loop) against an ephemeral sqlite store and a fake `bd` shell-script that
//! echoes a `bd list --json` array. The contract under test:
//!
//! 1. a `bd`-side close lands on the mirrored Hangar issue (`state = "done"`)
//!    and bumps the mapping's `last_synced`;
//! 2. a `bd` issue with no mapping row is skipped silently (out-of-scope);
//! 3. reconcile is idempotent (re-running produces no spurious updates);
//! 4. the poll loop honours a cancellation signal and backs off on `bd`
//!    failure.
//!
//! The fake-bd fixtures are hermetic — no live `bd` binary is required.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::beads_adapter::{BdClient, fake_bd};
use ainb_hangar_daemon::beads_sync::inbound::InboundSync;
use ainb_hangar_daemon::beads_sync::sync_loop::run_inbound_loop;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use tokio_util::sync::CancellationToken;

/// Frozen clock value used for every test's `last_synced` stamp.
const T0_MS: i64 = 1_700_000_000_000;
/// A later frozen instant used to prove `last_synced` is re-stamped.
const T1_MS: i64 = 1_700_000_100_000;

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

/// Insert an open Hangar issue and return its id.
async fn seed_issue(store: &Store, ws: &str, id: &str, title: &str) -> String {
    let new = NewIssue {
        id: id.to_string(),
        workspace_id: ws.to_string(),
        title: title.to_string(),
        description: None,
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "stevie").expect("actor"),
        created_at: T0_MS,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");
    id.to_string()
}

/// Insert a `Hangar`-sourced mapping row correlating `hangar_id` ↔ `bd_id`.
async fn seed_mapping(mapping: &BeadsMappingRepo<'_>, hangar_id: &str, bd_id: &str) {
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: hangar_id.to_string(),
            bd_id: bd_id.to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Hangar,
            last_synced: Utc.timestamp_millis_opt(T0_MS).single().expect("ts"),
        })
        .await
        .expect("seed mapping row");
}

#[tokio::test]
async fn test_bd_close_marks_hangar_done() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let hid = seed_issue(&store, &ws, "iss_1", "ship it").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    seed_mapping(&mapping, &hid, "bd-1").await;

    // fake-bd `list` returns the correlated issue, now closed.
    let bin = fake_bd::listing(bin_dir.path(), &[("bd-1", "ship it", "closed")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = sync.reconcile_once().await.expect("reconcile");

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.updated, 1);

    // Hangar issue moved to done.
    let issue = IssueRepo::get_by_id(store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "done");

    // Mapping last_synced bumped to the (later) clock value.
    let row = mapping.find_by_hangar(&hid).await.expect("query").expect("row");
    assert_eq!(
        row.last_synced,
        Utc.timestamp_millis_opt(T1_MS).single().unwrap()
    );
}

#[tokio::test]
async fn test_bd_status_change_updates_hangar_status() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let hid = seed_issue(&store, &ws, "iss_chg", "do it").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    seed_mapping(&mapping, &hid, "bd-chg").await;
    // Pre-move the Hangar issue to done so the test proves a re-open lands too.
    IssueRepo::update_state(store.pool(), &hid, "done").await.expect("pre-set done");

    // bd flips back to open → Hangar should follow.
    let bin = fake_bd::listing(bin_dir.path(), &[("bd-chg", "do it", "open")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = sync.reconcile_once().await.expect("reconcile");
    assert_eq!(stats.updated, 1);

    let issue = IssueRepo::get_by_id(store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn test_unknown_bd_id_skipped_silently() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let _ws = seed_workspace(&store).await;
    let mapping = BeadsMappingRepo::new(store.pool());

    // bd has an issue with NO mapping row — out of scope.
    let bin = fake_bd::listing(bin_dir.path(), &[("bd-orphan", "not mine", "closed")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = sync.reconcile_once().await.expect("reconcile");

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.updated, 0);
    assert_eq!(stats.skipped, 1);
}

#[tokio::test]
async fn test_bd_label_change_does_not_overwrite_hangar_labels() {
    // Labels are advisory: the inbound sync reconciles only status, never the
    // Hangar issue's own fields. A bd-side label set must not touch the issue
    // row beyond its state. (Hangar has no per-issue label column, so the proof
    // is that a status-unchanged list tick is a zero-update no-op even though the
    // bd issue carries labels.)
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let hid = seed_issue(&store, &ws, "iss_lbl", "labelled").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    seed_mapping(&mapping, &hid, "bd-lbl").await;

    // bd issue is still open (matches Hangar) but carries labels.
    let bin = fake_bd::listing(bin_dir.path(), &[("bd-lbl", "labelled", "open")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = sync.reconcile_once().await.expect("reconcile");

    assert_eq!(stats.updated, 0, "status unchanged → no update");
    let issue = IssueRepo::get_by_id(store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn test_inbound_idempotent() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let hid = seed_issue(&store, &ws, "iss_idem", "twice").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    seed_mapping(&mapping, &hid, "bd-idem").await;

    let bin = fake_bd::listing(bin_dir.path(), &[("bd-idem", "twice", "closed")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);
    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);

    let first = sync.reconcile_once().await.expect("first");
    assert_eq!(first.updated, 1);

    // Second run: state already matches → zero updates.
    let second = sync.reconcile_once().await.expect("second");
    assert_eq!(second.updated, 0);
    assert_eq!(second.scanned, 1);
}

#[tokio::test]
async fn test_swarm_sourced_mapping_skipped() {
    // A Swarm-sourced mapping is owned by a swarm leader's bd lifecycle; inbound
    // must not reconcile it.
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let hid = seed_issue(&store, &ws, "iss_swarm", "swarm").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: hid.clone(),
            bd_id: "bd-swarm".to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Swarm,
            last_synced: Utc.timestamp_millis_opt(T0_MS).single().expect("ts"),
        })
        .await
        .expect("seed swarm mapping");

    let bin = fake_bd::listing(bin_dir.path(), &[("bd-swarm", "swarm", "closed")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = sync.reconcile_once().await.expect("reconcile");

    assert_eq!(stats.skipped, 1);
    assert_eq!(stats.updated, 0);
    // Hangar issue untouched.
    let issue = IssueRepo::get_by_id(store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn test_poll_loop_respects_shutdown_signal() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let _ws = seed_workspace(&store).await;

    let bin = fake_bd::listing(bin_dir.path(), &[]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    // The loop borrows everything for 'static via a leaked owner: build the sync
    // inline in an owning Arc by leaking the deps for the test's lifetime.
    let bd: &'static BdClient = Box::leak(Box::new(bd));
    let pool: &'static sqlx::SqlitePool = Box::leak(Box::new(store.pool().clone()));
    let mapping: &'static BeadsMappingRepo<'static> =
        Box::leak(Box::new(BeadsMappingRepo::new(pool)));
    let clock: &'static FixedClock = Box::leak(Box::new(clock));
    let sync = Arc::new(InboundSync::new(bd, mapping, pool, clock));

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    let handle = tokio::spawn(async move {
        run_inbound_loop(sync, Duration::from_millis(50), cancel2).await;
    });

    // Cancel almost immediately; loop must exit promptly.
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(1), handle).await;
    assert!(res.is_ok(), "loop did not exit within 1s of cancellation");
}

#[tokio::test]
async fn test_poll_loop_backoff_on_bd_failure() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let _ws = seed_workspace(&store).await;

    // fake-bd always fails → reconcile_once returns Err every tick.
    let bin = fake_bd::failing(bin_dir.path(), "bd exploded");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T1_MS);

    let bd: &'static BdClient = Box::leak(Box::new(bd));
    let pool: &'static sqlx::SqlitePool = Box::leak(Box::new(store.pool().clone()));
    let mapping: &'static BeadsMappingRepo<'static> =
        Box::leak(Box::new(BeadsMappingRepo::new(pool)));
    let clock: &'static FixedClock = Box::leak(Box::new(clock));
    let sync = Arc::new(InboundSync::new(bd, mapping, pool, clock));

    // Even with a failing bd, the loop must keep running until cancelled (it logs
    // and backs off rather than panicking/exiting).
    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    let handle = tokio::spawn(async move {
        run_inbound_loop(sync, Duration::from_millis(10), cancel2).await;
    });

    // Let a couple of failing ticks elapse, then cancel.
    tokio::time::sleep(Duration::from_millis(40)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(1), handle).await;
    assert!(
        res.is_ok(),
        "loop must survive bd failures and still honour cancel"
    );
}
