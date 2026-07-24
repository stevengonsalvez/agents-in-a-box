//! Integration tests for last-writer-wins conflict resolution + the reconcile
//! service (P2.5).
//!
//! [`ReconcileService::run`] walks the `beads_mapping` table, diffs each mapped
//! pair (`bd` issue vs Hangar issue), and resolves drift by the **last-writer-
//! wins** rule keyed on `last_synced` vs the `bd` issue's `updated_at`:
//!
//! - `bd` changed since the last sync (`bd.updated_at > mapping.last_synced`)
//!   and the two sides disagree → `bd` wins, Hangar follows.
//! - Hangar changed since the last sync (`bd.updated_at <= mapping.last_synced`)
//!   and the two sides disagree → Hangar wins, `bd close` is mirrored out.
//! - Equal timestamps → Hangar wins (local intent beats the mirror).
//! - A dangling mapping (both sides gone) is deleted.
//! - An orphan `bd` issue tagged for sync with no mapping row is adopted into
//!   Hangar.
//!
//! `--dry-run` performs the full diff but skips every write (both `bd` side
//! effects and `beads_mapping`/issue writes).
//!
//! The fake-bd fixtures are hermetic — no live `bd` binary is required.

use chrono::{DateTime, TimeZone, Utc};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::{FixedIdGen, SystemIdGen};
use ainb_hangar_daemon::beads_adapter::{BdClient, fake_bd};
use ainb_hangar_daemon::beads_sync::reconcile::{ReconcileOpts, ReconcileService};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

/// An early instant (t0) — used as a `bd` `updated_at` strictly *before* a
/// later `last_synced` so the Hangar-newer (`Less`) branch is actually driven.
const T0_MS: i64 = 1_700_000_000_000;
/// Frozen `last_synced` baseline (t1).
const T1_MS: i64 = 1_700_000_100_000;
/// A later instant (t2) — the clock value `reconcile` stamps when it writes.
const T2_MS: i64 = 1_700_000_200_000;
/// The default workspace id every seeded issue references.
const WS: &str = "ws-1";

/// The ISO-8601 timestamp string a fake-bd issue carries; controls the bd-side
/// `updated_at` the conflict resolver compares against `mapping.last_synced`.
///
/// Truncates to whole seconds ([`chrono::SecondsFormat::Secs`]); every `*_MS`
/// constant here is second-aligned so equality holds exactly. Callers that pass
/// a sub-second `ms` would silently lose precision and could flip a `Less`/
/// `Greater` ordering into an `Equal` (tie) — keep new constants second-aligned.
fn iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .expect("ts")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn at(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().expect("ts")
}

/// Seed the workspace row every issue references.
async fn seed_workspace(store: &Store) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS)
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
}

/// Insert a Hangar issue in `state` and return its id.
async fn seed_issue(store: &Store, id: &str, title: &str, state: &str) -> String {
    let new = NewIssue {
        id: id.to_string(),
        workspace_id: WS.to_string(),
        title: title.to_string(),
        description: None,
        state: state.to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "stevie").expect("actor"),
        created_at: T1_MS,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        parent_issue_id: None,
        stage: None,
        acceptance_criteria: Vec::new(),
        context_refs: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");
    id.to_string()
}

/// Insert a `Hangar`-sourced mapping row stamped at `last_synced_ms`.
async fn seed_mapping(
    mapping: &BeadsMappingRepo<'_>,
    hangar_id: &str,
    bd_id: &str,
    last_synced_ms: i64,
) {
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: hangar_id.to_string(),
            bd_id: bd_id.to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Hangar,
            last_synced: at(last_synced_ms),
        })
        .await
        .expect("seed mapping row");
}

/// Build a `ReconcileService` wired against a fake-bd `list` fixture whose issues
/// carry an explicit `updated_at`. Returns the service's owned deps via the
/// closure so the borrows outlive the call.
struct Harness {
    store: Store,
    _store_dir: tempfile::TempDir,
    _bin_dir: tempfile::TempDir,
    _beads_dir: tempfile::TempDir,
    bd: BdClient,
    clock: FixedClock,
    /// Default id generator for the pairs that never mint a Hangar id. Tests
    /// that adopt an orphan inject a seeded [`FixedIdGen`] via
    /// [`Harness::service_with_idgen`] so the minted id is deterministic.
    idgen: SystemIdGen,
}

impl Harness {
    /// `issues`: `(bd_id, title, status, updated_at_ms)`.
    async fn new(issues: &[(&str, &str, &str, i64)]) -> Self {
        let store_dir = tempfile::tempdir().expect("store dir");
        let bin_dir = tempfile::tempdir().expect("bin dir");
        let beads_dir = tempfile::tempdir().expect("beads dir");
        let store = Store::open_in(store_dir.path()).await.expect("store");
        seed_workspace(&store).await;

        let listed: Vec<(String, String, String, String)> = issues
            .iter()
            .map(|(id, t, s, ms)| {
                (
                    (*id).to_string(),
                    (*t).to_string(),
                    (*s).to_string(),
                    iso(*ms),
                )
            })
            .collect();
        let bin = fake_bd::listing_with_updated(
            bin_dir.path(),
            &listed
                .iter()
                .map(|(id, t, s, u)| (id.as_str(), t.as_str(), s.as_str(), u.as_str()))
                .collect::<Vec<_>>(),
        );
        let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");

        Self {
            store,
            _store_dir: store_dir,
            _bin_dir: bin_dir,
            _beads_dir: beads_dir,
            bd,
            clock: FixedClock(T2_MS),
            idgen: SystemIdGen,
        }
    }

    fn service<'a>(&'a self, mapping: &'a BeadsMappingRepo<'a>) -> ReconcileService<'a> {
        ReconcileService::new(
            &self.bd,
            mapping,
            self.store.pool(),
            &self.clock,
            &self.idgen,
        )
    }

    /// Build a service with a caller-supplied id generator so adoption tests can
    /// assert a fixed minted Hangar id.
    fn service_with_idgen<'a>(
        &'a self,
        mapping: &'a BeadsMappingRepo<'a>,
        idgen: &'a dyn ainb_hangar_core::idgen::IdGen,
    ) -> ReconcileService<'a> {
        ReconcileService::new(&self.bd, mapping, self.store.pool(), &self.clock, idgen)
    }
}

#[tokio::test]
async fn test_no_drift_zero_updates() {
    // bd open at t1, hangar open, mapping at t1 → everyone agrees → no writes.
    let h = Harness::new(&[("bd-1", "agree", "open", T1_MS)]).await;
    let hid = seed_issue(&h.store, "iss_1", "agree", "open").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-1", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.updated, 0);
    assert_eq!(report.stats.scanned, 1, "the one bd issue was scanned");
    assert_eq!(
        report.stats.in_sync, 1,
        "the agreeing pair counts as in_sync"
    );
    assert!(report.conflicts.is_empty());
    assert!(report.adopted.is_empty());
}

#[tokio::test]
async fn test_bd_newer_wins_when_bd_last_synced_gt_hangar() {
    // bd flipped closed at t2 (after last_synced t1); hangar still open.
    // bd is newer → hangar follows to done, mapping bumped to t2.
    let h = Harness::new(&[("bd-2", "drift", "closed", T2_MS)]).await;
    let hid = seed_issue(&h.store, "iss_2", "drift", "open").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-2", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.updated, 1);
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "done", "bd-newer should drag hangar to done");

    let row = mapping.find_by_hangar(&hid).await.expect("query").expect("row");
    assert_eq!(row.last_synced, at(T2_MS));
}

#[tokio::test]
async fn test_hangar_newer_wins_when_hangar_last_synced_gt_bd() {
    // hangar flipped closed (state=done) after last sync; bd still open from t0,
    // strictly BEFORE last_synced t1 (bd.updated_at < last_synced), so bd did NOT
    // change since the sync — this drives the genuine `Less` → Hangar-wins branch
    // (not the tie branch). Hangar is the newer writer → mirror `bd close`, bump
    // mapping to t2.
    let h = Harness::new(&[("bd-3", "local", "open", T0_MS)]).await;
    let hid = seed_issue(&h.store, "iss_3", "local", "done").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-3", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.updated, 1);
    // Hangar issue stays done (it was the winner).
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "done");
    // The conflict record records Hangar as the winner.
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].hangar_id, hid);

    let row = mapping.find_by_hangar(&hid).await.expect("query").expect("row");
    assert_eq!(row.last_synced, at(T2_MS));
}

#[tokio::test]
async fn test_tie_breaker_prefers_hangar() {
    // Both sides disagree, both stamped t1 (bd.updated_at == last_synced).
    // Tie → Hangar wins; bd is mirrored to match Hangar (close).
    let h = Harness::new(&[("bd-4", "tie", "open", T1_MS)]).await;
    let hid = seed_issue(&h.store, "iss_4", "tie", "done").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-4", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.updated, 1);
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "done", "tie resolves to Hangar's state");
}

#[tokio::test]
async fn test_hangar_open_wins_over_closed_bd_is_unreconcilable_not_silently_repaired() {
    // The combination the LWW resolver previously mishandled:
    //   hangar = open, bd = closed, and Hangar wins the timestamp comparison
    //   (bd.updated_at t0 < last_synced t1 → the drift is a Hangar-side reopen).
    // Hangar wins, but bd exposes no `reopen` verb, so the open-Hangar state is
    // unrepresentable on the bd side. This must surface as an error (NOT a
    // silent "updated"), must NOT bump last_synced (else the next pass would
    // flip the winner to Bd and drag Hangar to done), and must leave both sides
    // exactly as they were.
    let h = Harness::new(&[("bd-reopen", "reopened", "closed", T0_MS)]).await;
    let hid = seed_issue(&h.store, "iss_reopen", "reopened", "open").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-reopen", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    // The drift is NOT reported as resolved.
    assert_eq!(
        report.stats.updated, 0,
        "unrepairable drift must not count as updated"
    );
    assert_eq!(
        report.stats.errors, 1,
        "the unrepairable pair counts as an error"
    );

    // Hangar issue stays open (no write).
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "open", "Hangar issue must not be touched");

    // last_synced must stay at t1 so the next pass can re-detect the drift
    // (bumping it to t2 would permanently suppress re-detection).
    let row = mapping.find_by_hangar(&hid).await.expect("query").expect("row");
    assert_eq!(
        row.last_synced,
        at(T1_MS),
        "last_synced must NOT be bumped for an unrepairable pair"
    );
}

#[tokio::test]
async fn test_dangling_mapping_row_deletes_when_both_sides_gone() {
    // Mapping row exists but neither the bd issue (not in the list) nor the
    // Hangar issue (never seeded) exists → the mapping is dangling → deleted.
    let h = Harness::new(&[]).await; // bd lists nothing
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, "iss_gone", "bd-gone", T1_MS).await;

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.deleted, 1);
    assert!(
        mapping.find_by_hangar("iss_gone").await.expect("query").is_none(),
        "dangling mapping row should be deleted"
    );
}

#[tokio::test]
async fn test_orphan_bd_with_hangar_label_creates_hangar_issue() {
    // bd has an issue tagged hangar-v1 with NO mapping row → Hangar imports it.
    // The minted Hangar id comes from the injected id generator, so a seeded
    // FixedIdGen makes the adopted id deterministic (asserted directly below
    // rather than recovered via find_by_bd).
    const ADOPTED_ID: &str = "iss_adopted_fixed";
    let h = Harness::new(&[("bd-orphan", "imported", "open", T2_MS)]).await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    let idgen = FixedIdGen::new(vec![ADOPTED_ID.to_string()]);

    let report = h
        .service_with_idgen(&mapping, &idgen)
        .run(ReconcileOpts {
            workspace_id: WS.to_string(),
            ..ReconcileOpts::default()
        })
        .await
        .expect("reconcile");

    assert_eq!(report.adopted.len(), 1, "orphan bd issue should be adopted");
    assert_eq!(
        report.adopted[0], ADOPTED_ID,
        "adopted id must be the seeded (deterministic) id"
    );
    // A mapping row now correlates the orphan bd id with the new hangar issue,
    // keyed on the seeded id.
    let row = mapping.find_by_bd("bd-orphan").await.expect("query").expect("row");
    assert_eq!(row.source, MappingSource::Hangar);
    assert_eq!(row.hangar_id, ADOPTED_ID);
    let issue = IssueRepo::get_by_id(h.store.pool(), ADOPTED_ID)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(issue.title, "imported");
}

#[tokio::test]
async fn test_dry_run_skips_writes() {
    // bd newer wants to drag hangar to done — but --dry-run must skip every write
    // while still reporting the conflict it would have resolved.
    let h = Harness::new(&[("bd-dry", "dry", "closed", T2_MS)]).await;
    let hid = seed_issue(&h.store, "iss_dry", "dry", "open").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    seed_mapping(&mapping, &hid, "bd-dry", T1_MS).await;

    let report = h
        .service(&mapping)
        .run(ReconcileOpts {
            dry_run: true,
            ..ReconcileOpts::default()
        })
        .await
        .expect("reconcile");

    // Reported but not applied.
    assert_eq!(report.conflicts.len(), 1);
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(
        issue.state, "open",
        "dry-run must not write the hangar issue"
    );
    let row = mapping.find_by_hangar(&hid).await.expect("query").expect("row");
    assert_eq!(
        row.last_synced,
        at(T1_MS),
        "dry-run must not bump last_synced"
    );
}

#[tokio::test]
async fn test_swarm_sourced_mapping_skipped() {
    // A swarm-sourced mapping is owned by a leader's bd lifecycle; reconcile
    // never touches it even when the two sides disagree.
    let h = Harness::new(&[("bd-sw", "swarm", "closed", T2_MS)]).await;
    let hid = seed_issue(&h.store, "iss_sw", "swarm", "open").await;
    let mapping = BeadsMappingRepo::new(h.store.pool());
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: hid.clone(),
            bd_id: "bd-sw".to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Swarm,
            last_synced: at(T1_MS),
        })
        .await
        .expect("seed swarm mapping");

    let report = h.service(&mapping).run(ReconcileOpts::default()).await.expect("reconcile");

    assert_eq!(report.stats.updated, 0);
    let issue = IssueRepo::get_by_id(h.store.pool(), &hid).await.expect("get").expect("present");
    assert_eq!(issue.state, "open", "swarm mapping must be left untouched");
}
