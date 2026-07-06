//! e38.22 scheduled-GC tests: the workspace-tree orphan sweep + its wiring into
//! the daemon sweeper scheduler.
//!
//! The bead's core gap is that [`cleanup`](ainb_hangar_daemon::execenv::cleanup)
//! (`OrphanScan` / `Full`) was implemented + unit-tested but **never scheduled**:
//! `spawn_sweepers` ran only the TTL row-sweepers, so leaked per-task dirs (no
//! `.gc_meta.json`, mtime past the 72h grace) accumulated on disk forever.
//!
//! These tests pin the two halves of the fix:
//! 1. [`sweep_workspaces_gc`] walks the live `{home}/.agents-in-a-box/hangar/workspaces/`
//!    tree and reclaims every orphan root while retaining every live (marked) one
//!    and every young orphan (inside the grace window).
//! 2. [`spawn_gc_sweeper`] drives that walk on a periodic, clock-injected tick so
//!    the daemon loop actually reclaims the leak (the schedule the bead is about).

// The epoch-ms arithmetic below casts `Duration::as_millis()` (u128) to `i64`;
// every value is a near-present wall-clock time, well within `i64` range.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::path::{Path, PathBuf};

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::execenv::{ORPHAN_GRACE_MS, sweep_workspaces_gc};
use ainb_hangar_daemon::run_loop::spawn_gc_sweeper;

/// The workspaces root the sweep walks: `{home}/.agents-in-a-box/hangar/workspaces/`.
fn workspaces_root(home: &Path) -> PathBuf {
    home.join(".agents-in-a-box").join("hangar").join("workspaces")
}

/// Materialise a per-task `{shortID}` dir under `workspaces/{slug}/`, optionally
/// writing a `.gc_meta.json` marker (a "live" dir) or leaving it unmarked (an
/// orphan candidate). Returns the dir path.
fn seed_task_dir(home: &Path, slug: &str, short_id: &str, marked: bool) -> PathBuf {
    let dir = workspaces_root(home).join(slug).join(short_id);
    std::fs::create_dir_all(dir.join("workdir")).expect("mkdir task dir");
    if marked {
        std::fs::write(dir.join(".gc_meta.json"), b"{}").expect("write marker");
    }
    dir
}

/// "Now" pushed far enough past real wall-clock that a just-created dir's mtime
/// is comfortably older than the 72h grace — deterministic without a `filetime`
/// dependency (the dir's real mtime ≈ real now, and `now_ms` is real now + 73h).
fn now_past_grace() -> i64 {
    let real_now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis() as i64;
    real_now + ORPHAN_GRACE_MS + 3_600 * 1_000
}

#[test]
fn gc_sweep_removes_orphan_dir_keeps_live_dir() {
    let home = tempfile::tempdir().expect("tempdir home");

    // An orphan: no marker, old enough (now_ms is 73h past its real mtime).
    let orphan = seed_task_dir(home.path(), "alpha", "0000orph", false);
    // A live task dir: marker present → must survive even though `now_ms` is far
    // in the future.
    let live = seed_task_dir(home.path(), "alpha", "1111live", true);

    let report = sweep_workspaces_gc(home.path(), now_past_grace()).expect("gc sweep");

    assert!(!orphan.exists(), "orphan task dir is reclaimed");
    assert!(live.is_dir(), "live (marked) task dir is retained");
    assert_eq!(report.reclaimed, 1, "exactly one dir reclaimed");
    assert_eq!(report.retained, 1, "exactly one dir retained");
}

#[test]
fn gc_sweep_keeps_young_orphan_inside_grace() {
    let home = tempfile::tempdir().expect("tempdir home");
    // An unmarked dir whose mtime is well inside the 72h grace (now_ms ≈ real
    // now) must NOT be removed — a freshly-leaked dir gets the grace window
    // before it is treated as a permanent orphan.
    let young = seed_task_dir(home.path(), "beta", "2222yng", false);
    let real_now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("after epoch")
        .as_millis() as i64;

    let report = sweep_workspaces_gc(home.path(), real_now).expect("gc sweep");

    assert!(
        young.is_dir(),
        "a young orphan inside the grace window is retained"
    );
    assert_eq!(report.reclaimed, 0);
    assert_eq!(report.retained, 1);
}

#[test]
fn gc_sweep_absent_tree_is_noop() {
    // A daemon that has never dispatched a task has no workspaces tree at all;
    // the sweep must be a clean no-op, not an error.
    let home = tempfile::tempdir().expect("tempdir home");
    let report = sweep_workspaces_gc(home.path(), now_past_grace()).expect("gc sweep over empty");
    assert_eq!(report.reclaimed, 0);
    assert_eq!(report.retained, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn scheduled_gc_tick_reclaims_orphan() {
    // The scheduling half (the bead's actual gap): the periodic GC sweeper,
    // driven on a real (short) interval, reclaims the seeded orphan on its first
    // tick. The grace comparison stays deterministic via the injected clock
    // (`now_ms` pinned past the 72h window); only the *interval firing* uses real
    // time, with a generous poll margin — matching the `usage_dir_watcher`
    // real-sleep convention in this workspace (which deliberately avoids pulling
    // in tokio's `test-util` feature just for `start_paused`).
    //
    // If `spawn_gc_sweeper` did NOT drive the sweep (an unscheduled tick — the
    // exact bead gap), the orphan would still exist when the poll times out and
    // the final assertion would break.
    let home = tempfile::tempdir().expect("tempdir home");
    let orphan = seed_task_dir(home.path(), "alpha", "0000orph", false);
    let live = seed_task_dir(home.path(), "alpha", "1111live", true);

    // The sweeper also runs the worktree GC, which needs a store to resolve task
    // status; this home has no `worktrees/` dir, so that pass is an inert no-op.
    let store = ainb_hangar_store::Store::open_in(&home.path().join(".agents-in-a-box"))
        .await
        .expect("store");

    let clock = std::sync::Arc::new(FixedClock(now_past_grace()));
    let handle = spawn_gc_sweeper(
        store.pool().clone(),
        home.path().to_path_buf(),
        std::time::Duration::from_millis(20),
        clock,
    );

    // Poll for the scheduled tick to land the reclaim (generous margin so a slow
    // CI box never flakes; a working schedule reclaims within the first interval).
    let mut reclaimed = false;
    for _ in 0..200 {
        if !orphan.exists() {
            reclaimed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    handle.abort();

    assert!(reclaimed, "scheduled GC tick reclaimed the orphan dir");
    assert!(live.is_dir(), "scheduled GC tick retained the live dir");
}
