//! T0-section (#1015): the host task keeps section 20 current from a real
//! daemon, one joined read per Fleet revision.
//!
//! A daemon serves on a temporary socket; the host task dials it exactly as the
//! TUI does, and hook events applied through the daemon's own apply path must
//! reach section 20 of a real `AppState` through the section's reducer.

use std::time::{Duration, Instant};

use ainb::agent_status_host::AgentStatusHost;
use ainb::app::state::AppState;
use ainb::fleet::bridge::daemon::DaemonClient;
use ainb_hangar_daemon::events::{EventBroker, EventSink};
use ainb_hangar_daemon::fleet::{HookObservation, apply_hook};
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::agent_status::AgentState;
use ainb_hangar_store::Store;

async fn start_daemon(home: &std::path::Path) -> (Store, EventSink, std::path::PathBuf, String) {
    let store = Store::open_in(home).await.expect("open store");
    rpc::auth::ensure_socket_token(store.pool(), home).await.expect("socket token");
    let socket = rpc::socket_path_in(home);
    let listener = rpc::bind(&socket).expect("bind socket");
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "test".to_string(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(home))
        .expect("token")
        .trim()
        .to_string();
    (store, sink, socket, token)
}

async fn hook(store: &Store, sink: &EventSink, event_id: &str, event_type: &str, at: i64) {
    let payload = serde_json::json!({
        "session_id": "host-1",
        "cwd": "/work/host",
        "hook_event_name": event_type,
        "tool_name": "AskUserQuestion",
    });
    apply_hook(
        store.pool(),
        sink,
        HookObservation {
            event_id: event_id.to_string(),
            provider: "claude",
            provider_session_id: "host-1",
            cwd: "/work/host",
            event_type,
            payload: &payload,
            observed_at: at,
            transcript_model: None,
        },
    )
    .await
    .expect("hook applies");
}

/// Drain the host into `state` until `done` holds, or fail after five seconds.
async fn wait_for(
    host: &mut AgentStatusHost,
    state: &mut AppState,
    what: &str,
    done: impl Fn(&AppState) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        host.drain_into(state);
        if done(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never reached section 20: view {:?}, absent {:?}",
            state.agent_status.view.as_ref().map(|view| (&view.health, view.cards.len())),
            state.agent_status.absent
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_task_fills_section_20_and_follows_each_revision() {
    let home = tempfile::tempdir().expect("home");
    let (store, sink, socket, token) = start_daemon(home.path()).await;
    hook(&store, &sink, "e-start", "SessionStart", 1_700_000_000_000).await;

    let mut host = AgentStatusHost::spawn(Box::new(move || {
        Ok(DaemonClient::with_parts(socket.clone(), token.clone()))
    }));
    let mut state = AppState::default();
    wait_for(&mut host, &mut state, "the first joined read", |state| {
        state
            .agent_status
            .view
            .as_ref()
            .is_some_and(|view| view.cards.contains_key("claude:host-1"))
    })
    .await;
    let first_version = state.agent_status.version();
    let first_view = state.agent_status.view.as_ref().expect("view");
    let first_revision = first_view.read_revision;
    let card = &first_view.cards["claude:host-1"];
    assert_eq!(card.status.host_id, "local");
    assert_ne!(card.status.state, AgentState::Waiting);

    // A new revision: the task reads again and section 20 follows it.
    hook(&store, &sink, "e-ask", "PreToolUse", 1_700_000_001_000).await;
    wait_for(
        &mut host,
        &mut state,
        "the revision after a hook",
        |state| {
            state
                .agent_status
                .view
                .as_ref()
                .is_some_and(|view| view.read_revision > first_revision)
        },
    )
    .await;
    assert!(
        state.agent_status.version() > first_version,
        "a rendered change bumps section 20"
    );
}
