//! P2.6 — end-to-end Beads round-trip tripwire.
//!
//! This is the capstone of P2: it wires the real building blocks (P2.1 mapping
//! repo, P2.2 `BdClient`, P2.3 [`OutboundSync`], P2.4 [`InboundSync`]) against a
//! **live `bd` binary** and proves the full two-way loop end-to-end:
//!
//! ```text
//!   OutboundSync::mirror_create ──▶ bd create (real)
//!                                    │
//!   bd list -l foo  ◀───────────────┘   (issue visible in bd)
//!   bd close <id>   ──▶ bd (real)
//!                                    │
//!   InboundSync::reconcile_once ◀────┘   (close lands back on Hangar)
//!   ⇒ Hangar issue.state == "done", mapping.last_synced bumped
//! ```
//!
//! Unlike the P2.3/P2.4 integration tests (which drive a hermetic fake-`bd`
//! script), this tripwire deliberately exercises the genuine `bd` CLI so the
//! adapter's wire-shape assumptions are validated against reality — the exact
//! failure mode a fake fixture cannot catch.
//!
//! # Determinism
//!
//! - [`FixedClock`] freezes wall time so `last_synced` is reproducible.
//! - [`FixedIdGen`] is unused here (no orphan adoption) but the seam is honoured:
//!   the Hangar issue id is a fixed literal, not minted.
//! - The store and `BEADS_DIR` are fresh tempdirs per run; the [`BdClient`]'s
//!   `O_EXCL` pidfile lock serialises `bd` writes against the same `BEADS_DIR` even
//!   when cargo runs integration binaries in parallel.
//!
//! # Real `bd` required
//!
//! The test resolves `bd` on `PATH` and **skips with a helpful hint** when it is
//! absent (local dev without the tracker installed); CI has `bd` by default.

use chrono::{TimeZone, Utc};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::beads_adapter::{BdClient, BdId, BdListFilter};
use ainb_hangar_daemon::beads_sync::inbound::{InboundSync, SYNC_LABEL};
use ainb_hangar_daemon::beads_sync::outbound::{MirrorOutcome, OutboundSync};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::beads_mapping::{BeadsMappingRepo, MappingSource};
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};

mod common;
use common::{bd_available, bd_init};

/// Frozen clock value used for `last_synced` stamps in this tripwire.
const T0_MS: i64 = 1_700_000_000_000;
/// The fixed Hangar issue id mirrored into `bd`.
const HANGAR_ID: &str = "iss_roundtrip";
/// The workspace the issue lives under.
const WORKSPACE_ID: &str = "ws-trip";

/// Seed the workspace row the issue FKs against.
async fn seed_workspace(store: &Store) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WORKSPACE_ID)
        .bind("trip")
        .bind("Trip")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
}

/// Insert the open Hangar issue and read it back.
async fn seed_issue(store: &Store) -> Issue {
    let new = NewIssue {
        id: HANGAR_ID.to_string(),
        workspace_id: WORKSPACE_ID.to_string(),
        title: "roundtrip".to_string(),
        description: Some("e2e tripwire issue".to_string()),
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "stevie").expect("actor"),
        created_at: T0_MS,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
    };
    IssueRepo::insert(store.pool(), &new).await.expect("insert issue");
    IssueRepo::get_by_id(store.pool(), HANGAR_ID)
        .await
        .expect("get issue")
        .expect("issue present")
}

#[tokio::test]
async fn tripwire_beads_roundtrip() {
    if !bd_available() {
        eprintln!(
            "SKIP tripwire_beads_roundtrip: `bd` not on PATH. \
             Install the beads CLI (e.g. `cargo install beads-cli`) to run this tripwire."
        );
        return;
    }

    // 1. tempdir = $HOME, $BEADS_DIR; 2. bd init.
    let home = tempfile::tempdir().expect("home dir");
    let store_dir = tempfile::tempdir().expect("store dir");
    let beads_dir = bd_init(home.path());

    let store = Store::open_in(store_dir.path()).await.expect("store");
    seed_workspace(&store).await;
    let issue = seed_issue(&store).await;

    let bd = BdClient::new("bd", beads_dir.clone()).expect("bd client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    // 4. mirror the issue out to bd with both the sync label and a probe label.
    let outbound = OutboundSync::new(&bd, &mapping, &clock);
    let outcome = outbound
        .mirror_create(
            &issue,
            MappingSource::Hangar,
            &[SYNC_LABEL.to_string(), "foo".to_string()],
        )
        .await
        .expect("mirror create");
    assert_eq!(outcome, MirrorOutcome::Mirrored, "issue must mirror to bd");

    // 5. assert the issue is visible via `bd list -l foo`.
    let listed = bd
        .list(BdListFilter {
            labels: vec!["foo".to_string()],
            since: None,
            // The issue is still open at this point — default (open-only) is fine.
            include_closed: false,
        })
        .expect("bd list -l foo");
    let titles: Vec<&str> = listed.iter().map(|i| i.title.as_str()).collect();
    assert!(
        titles.contains(&"roundtrip"),
        "bd list -l foo must contain the mirrored issue; got {titles:?}"
    );

    // 6. extract the bd id from the mapping row outbound persisted.
    let row = mapping
        .find_by_hangar(HANGAR_ID)
        .await
        .expect("query mapping")
        .expect("mapping row present");
    let bd_id = BdId::from(row.bd_id.as_str());

    // 7. close the bd issue out-of-band (simulating Stevie's `bd close`).
    let closed = bd.close(&bd_id, None).expect("bd close");
    assert_eq!(
        closed.status,
        ainb_hangar_daemon::beads_adapter::BdStatus::Closed,
        "bd close must report the issue closed"
    );

    // 8. drive the inbound sync directly (deterministic; no poll-loop wall clock).
    let inbound = InboundSync::new(&bd, &mapping, store.pool(), &clock);
    let stats = inbound.reconcile_once().await.expect("inbound reconcile");
    assert_eq!(
        stats.updated, 1,
        "inbound reconcile must flip exactly one Hangar issue to done; stats={stats:?}"
    );

    // 9. assert the Hangar issue is now `done`.
    let final_issue = IssueRepo::get_by_id(store.pool(), HANGAR_ID)
        .await
        .expect("get issue")
        .expect("issue present");
    assert_eq!(
        final_issue.state, "done",
        "the bd close must land back on the Hangar issue as done"
    );

    // 10. assert the mapping row carries both ids and a bumped last_synced.
    let final_row = mapping
        .find_by_hangar(HANGAR_ID)
        .await
        .expect("query mapping")
        .expect("mapping row present");
    assert_eq!(final_row.hangar_id, HANGAR_ID);
    assert_eq!(
        final_row.bd_id, row.bd_id,
        "bd id stable across the round-trip"
    );
    assert_eq!(
        final_row.last_synced,
        Utc.timestamp_millis_opt(T0_MS).single().expect("ts"),
        "last_synced is the injected FixedClock instant"
    );
    assert_eq!(final_row.source, MappingSource::Hangar);
}
