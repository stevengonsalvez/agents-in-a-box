//! T0-section (#1015): the host task keeps section 20 current from a real
//! daemon, one joined read per Fleet revision, and (#1031) is the only
//! agent-status reader in the process: the Fleet panel renders the envelope it
//! publishes.
//!
//! A daemon serves on a temporary socket; the host task dials it exactly as the
//! TUI does, and hook events applied through the daemon's own apply path must
//! reach section 20 of a real `AppState` through the section's reducer.

use std::time::{Duration, Instant};

#[allow(unused)]
#[path = "tripwire_helpers.rs"]
mod tripwire_helpers;

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

    let mut host = AgentStatusHost::spawn(
        Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), token.clone()))),
        false,
    );
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

/// #1031 budget, the #1015 criterion 9 number: one Fleet event costs this
/// process at most ONE whole-Fleet projection. The host task pays it; the
/// Fleet panel folds the published envelope and pays none, because it holds no
/// daemon client at all.
///
/// Current-thread on purpose: the daemon's projection counter is thread-local,
/// and here the daemon, the host task and the panel share the test's thread.
#[tokio::test]
async fn one_fleet_event_costs_this_process_one_projection() {
    use ainb_hangar_daemon::fleet::projection_reads;
    use ainb_hangar_proto::status_topic::AgentStatusEnvelope;
    use ainb_plugin_hangar::screen::fleet::FleetPaneState;

    let home = tempfile::tempdir().expect("home");
    let (store, sink, socket, token) = start_daemon(home.path()).await;
    hook(&store, &sink, "e-start", "SessionStart", 1_700_000_000_000).await;

    let mut host = AgentStatusHost::spawn(
        Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), token.clone()))),
        false,
    );
    let mut state = AppState::default();
    wait_for(&mut host, &mut state, "the first joined read", |state| {
        state
            .agent_status
            .view
            .as_ref()
            .is_some_and(|view| view.cards.contains_key("claude:host-1"))
    })
    .await;
    // Let the subscription open and any read already owed land first.
    tokio::time::sleep(Duration::from_millis(200)).await;
    host.drain_into(&mut state);

    let mut pane = FleetPaneState::default();
    let mut sequence = 0;
    let events = [
        ("e-ask", "PreToolUse", 1_700_000_001_000),
        ("e-answered", "PostToolUse", 1_700_000_002_000),
        ("e-stop", "Stop", 1_700_000_003_000),
    ];
    for (event_id, event_type, at) in events {
        let revision_before = state.agent_status.view.as_ref().expect("view").read_revision;
        let reads_before = projection_reads();
        hook(&store, &sink, event_id, event_type, at).await;
        wait_for(&mut host, &mut state, event_id, |state| {
            state
                .agent_status
                .view
                .as_ref()
                .is_some_and(|view| view.read_revision > revision_before)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        host.drain_into(&mut state);

        sequence += 1;
        let payload = ainb::agent_status_host::encode(&state.agent_status, sequence)
            .expect("section 20 publishes");
        let envelope: AgentStatusEnvelope = serde_json::from_slice(&payload).expect("envelope");
        assert!(pane.apply_envelope(envelope));

        let view = state.agent_status.view.as_ref().expect("view");
        let revisions = view.read_revision - revision_before;
        let reads = i64::try_from(projection_reads() - reads_before).expect("small");
        assert!(
            (1..=revisions).contains(&reads),
            "{event_id}: {reads} projection reads for {revisions} new revisions"
        );
        assert_eq!(
            pane.status_view(),
            Some(view),
            "{event_id}: the panel renders exactly section 20"
        );
    }
}

/// #1038 review item 5, end to end through the plugin runtime: a plugin
/// subscribed to `fleet.agent_status` receives section 20 as the host
/// publishes it (`publish_snapshot`, then `plugin/handle_event`), the Fleet pane
/// folds what arrived into exactly section 20's view, a later section change
/// arrives as a newer envelope, and `publish` holds a change while there is no
/// runtime.
///
/// The fixture plugin relays each delivery back under `fixture.received`, so
/// the assertion is on the bytes the plugin was handed, not on the host's own
/// copy. Its grant names the topic, as the hangar plugin's does.
#[test]
fn a_section_change_reaches_a_subscribed_plugin_through_the_runtime() {
    use ainb_hangar_proto::status_topic::{AGENT_STATUS_TOPIC, AgentStatusEnvelope};
    use ainb_plugin_hangar::screen::fleet::FleetPaneState;
    use ainb_plugin_protocol::manifest::{
        Capabilities, CapabilityGrant, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode,
        Subscribes,
    };
    use ainb_plugin_runtime::registry::RegisteredPlugin;
    use ainb_plugin_runtime::types::CliOutcome;

    let (rt, handle) = tripwire_helpers::fast_runtime();
    let manifest = Manifest {
        plugin: PluginMeta {
            name: "status-topic-fixture".into(),
            version: "0.1.0".into(),
            abi_version: 2,
            description: "agent-status topic subscriber".into(),
        },
        capabilities: Capabilities {
            event_bus: CapabilityGrant::List(vec![
                AGENT_STATUS_TOPIC.to_string(),
                "fixture.*".to_string(),
            ]),
            ..Capabilities::default()
        },
        provides: Provides {
            screens: vec![],
            commands: vec![],
            cli_namespaces: vec!["echo".into()],
            snapshots: vec!["fixture.received".into()],
        },
        subscribes: Subscribes {
            snapshots: vec![AGENT_STATUS_TOPIC.to_string()],
        },
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
        config: Vec::new(),
    };
    let plugin = RegisteredPlugin::new(
        manifest,
        tripwire_helpers::sibling_bin("ainb-fixture-plugin"),
        std::path::PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    tripwire_helpers::ensure_running(&rt, &handle, &id);
    let subscribed = handle.dispatch_cli(
        &id,
        "echo",
        vec!["subscribe".into(), AGENT_STATUS_TOPIC.into()],
    );
    let subscribed = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), subscribed)
            .await
            .expect("subscribe dispatch timed out")
            .expect("subscribe dispatch closed")
    });
    assert!(matches!(subscribed, CliOutcome::Ok(_)), "{subscribed:?}");

    let received = |sequence: u64| -> AgentStatusEnvelope {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(bytes) = handle.snapshot_get("fixture.received") {
                let envelope: AgentStatusEnvelope =
                    serde_json::from_slice(&bytes).expect("the plugin was handed an envelope");
                if envelope.sequence >= sequence {
                    return envelope;
                }
            }
            assert!(
                Instant::now() < deadline,
                "envelope {sequence} never reached the plugin"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let home = tempfile::tempdir().expect("home");
    let (store, sink, mut host, mut state) = rt.tokio_handle().block_on(async {
        let (store, sink, socket, token) = start_daemon(home.path()).await;
        hook(&store, &sink, "e-start", "SessionStart", 1_700_000_000_000).await;
        let mut host = AgentStatusHost::spawn(
            Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), token.clone()))),
            false,
        );
        let mut state = AppState::default();
        wait_for(&mut host, &mut state, "the first joined read", |state| {
            state
                .agent_status
                .view
                .as_ref()
                .is_some_and(|view| view.cards.contains_key("claude:host-1"))
        })
        .await;
        (store, sink, host, state)
    });

    assert!(
        !host.publish(&state, None),
        "no runtime yet: the change is held"
    );
    assert!(
        host.publish(&state, Some(&handle)),
        "and published once there is one"
    );
    let mut pane = FleetPaneState::default();
    assert!(pane.apply_envelope(received(1)));
    assert_eq!(pane.status_view(), state.agent_status.view.as_ref());

    let first_revision = state.agent_status.view.as_ref().expect("view").read_revision;
    rt.tokio_handle().block_on(async {
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
    });
    assert!(host.publish(&state, Some(&handle)));
    assert!(pane.apply_envelope(received(2)));
    assert_eq!(
        pane.status_view(),
        state.agent_status.view.as_ref(),
        "the section change reached the pane"
    );
    drop(host);
}
