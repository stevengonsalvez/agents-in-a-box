//! P6e: the desktop executor's session-store write, in its own binary.
//!
//! `HOME` and the session store are process-wide, and a sibling test in
//! `host_contract.rs` asserts that a host touches no file under `HOME`, so
//! this write cannot share that process.

mod support;

use std::time::{Duration, Instant};

use ainb_app::app::Effect;
use ainb_desktop::host::Executor;
use support::isolated_home as scratch_home;

/// How long the tick may take over a write that cannot proceed. The write is
/// queued, so this is the cost of a channel send, not of the write.
const ON_TICK: Duration = Duration::from_millis(250);

/// P6e: the desktop executor hands a session-store write to its worker and
/// returns at once, even while that write cannot proceed; the write lands once
/// it can, and the flush the app's exit paths call waits for it.
///
/// The write is held up by the `sessions.json` lock, taken here and held: any
/// executor that wrote on the tick would wait on that lock for
/// `SESSIONS_LOCK_WAIT` (10 s) and fail [`ON_TICK`]. This box's own session
/// source is the file (the capability is dark), so what a held lock stands in
/// for is a daemon that stopped answering, which the terminal host pins
/// directly (`ainb-core/tests/host_write_with_a_hung_daemon.rs`); there is no
/// daemon harness in this crate.
#[test]
fn a_session_store_write_is_queued_while_it_cannot_proceed_and_lands_after() {
    use ainb_app::app::Persist;
    use ainb_app::interactive::session_manager::{SessionMetadata, SessionStore};

    let home = scratch_home();
    let mut store = SessionStore::default();
    let tmux = "tmux_desktop-p6e".to_string();
    store.upsert(SessionMetadata {
        session_id: uuid::Uuid::new_v4(),
        tmux_session_name: tmux.clone(),
        worktree_path: home.join("work"),
        workspace_name: "ws".to_string(),
        created_at: serde_json::from_str("\"2026-09-19T00:00:00Z\"").expect("a timestamp"),
        agent_type: ainb_app::models::session::SessionAgentType::default(),
        headroom_enabled: true,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ainb_app::interactive::session_manager::ModelSource::default(),
        codex_model: None,
        codex_thread_id: None,
    });
    store.save().expect("seed sessions.json");

    // Held before the effect and released after the tick was measured: while
    // this guard lives, no write to the store can complete.
    let guard = SessionStore::try_lock()
        .expect("the sessions.json lock")
        .expect("the sessions.json lock was already held");

    let mut executor = ainb_desktop::executor::DesktopExecutor::new(None);
    let started = Instant::now();
    let reports = executor.execute(Effect::Persist(Persist::SessionHeadroom {
        tmux_session: tmux.clone(),
        expected: true,
        enabled: false,
    }));
    let on_tick = started.elapsed();
    assert!(
        on_tick < ON_TICK,
        "the session-store write held the tick: {on_tick:?}"
    );
    assert!(
        reports.is_empty(),
        "the write reported on the tick: {reports:?}"
    );
    assert!(
        SessionStore::load().sessions[&tmux].headroom_enabled,
        "the write ran on the tick, through a lock this test holds"
    );

    // Let the worker through, then wait for it as an exit path would.
    drop(guard);
    let dropped =
        executor.flush_session_store_writes(ainb_app::cli::util::SESSION_STORE_FLUSH_BOUND);
    assert_eq!(dropped, 0, "the queued write did not land within the bound");
    assert!(
        !SessionStore::load().sessions[&tmux].headroom_enabled,
        "the queued write did not land"
    );
}
