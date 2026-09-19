//! P6e: the desktop executor's session-store write, in its own binary.
//!
//! `HOME` and the session store are process-wide, and a sibling test in
//! `host_contract.rs` asserts that a host touches no file under `HOME`, so
//! this write cannot share that process.

mod support;

use ainb_app::app::Effect;
use ainb_desktop::host::Executor;
use support::isolated_home as scratch_home;

/// P6e: the desktop executor hands a session-store write to its worker and
/// returns at once, reporting nothing on the tick; the write lands before the
/// executor is gone, because dropping it waits for the worker.
///
/// This box's own session source is the file (the capability is dark), so what
/// this pins is the queue and the join, not the daemon wait. The daemon wait
/// is pinned on the terminal host, against a daemon that stops answering
/// (`ainb-core/tests/host_write_with_a_hung_daemon.rs`); the desktop arm is
/// the same shape and has no daemon harness in this crate.
#[test]
fn a_session_store_write_is_queued_and_finished_when_the_executor_goes() {
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

    let mut executor = ainb_desktop::executor::DesktopExecutor::new(None);
    let reports = executor.execute(Effect::Persist(Persist::SessionHeadroom {
        tmux_session: tmux.clone(),
        expected: true,
        enabled: false,
    }));
    assert!(
        reports.is_empty(),
        "the write reported on the tick: {reports:?}"
    );

    drop(executor);
    assert!(
        !SessionStore::load().sessions[&tmux].headroom_enabled,
        "the queued write did not land before the executor went"
    );
}
