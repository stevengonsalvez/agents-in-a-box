//! Client invariants against the SCRIPTED FIXTURE adapter (never a real one).
//!
//! Covers I13 (mode pinning + env allowlist), the `-32602` surfacing rule, and
//! the handler-before-load ordering the plan calls the port's most likely bug.

mod support;

use std::path::Path;

use agent_client_protocol::schema::v1::SessionNotification;
use ainb_acp::client::{AcpError, AdapterProcess};
use tokio::sync::mpsc;

use support::{env, fake_config};

async fn spawn(
    mode: &str,
    script_env: Vec<(String, String)>,
) -> (
    Result<AdapterProcess, AcpError>,
    mpsc::UnboundedReceiver<SessionNotification>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let config = fake_config(mode, script_env);
    (
        AdapterProcess::spawn(&config, tx, permission_sink()).await,
        rx,
    )
}

#[tokio::test]
async fn initialize_records_agent_info_and_the_load_capability() {
    let (adapter, _rx) = spawn("default", env(&[])).await;
    let adapter = adapter.expect("spawn");
    assert_eq!(adapter.info().name, "fake-acp-adapter");
    assert_eq!(
        adapter.info().version.as_deref(),
        Some("0.0.0-fixture"),
        "agentInfo.version is what fleet_acp_session.provider_version records"
    );
    assert!(adapter.supports_load());
}

#[tokio::test]
async fn load_capability_is_reprobed_from_the_adapter_not_assumed() {
    let (adapter, _rx) = spawn("default", env(&[("FAKE_ACP_NO_LOAD", "1")])).await;
    let adapter = adapter.expect("spawn");
    assert!(!adapter.supports_load());
    let error = adapter
        .load_session("fake-session-1", Path::new("/tmp"))
        .await
        .expect_err("load must refuse without the capability");
    assert!(
        matches!(error, AcpError::LoadUnsupported { .. }),
        "{error:?}"
    );
}

/// I13, env half. A variable planted in the daemon's own environment must not
/// reach the child.
#[tokio::test]
async fn the_child_environment_is_an_allowlist_not_an_inheritance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dump = dir.path().join("env.json");
    // SAFETY-BY-CONVENTION: this test owns the variable name; nothing else in
    // the workspace reads it.
    std::env::set_var("AINB_ACP_PLANTED_SECRET", "leaked");

    let (adapter, _rx) = spawn(
        "default",
        env(&[("FAKE_ACP_ENV_DUMP", dump.to_str().expect("utf8 path"))]),
    )
    .await;
    let _adapter = adapter.expect("spawn");

    let dumped: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dump).expect("env dump")).expect("json");
    let child_env = dumped.as_object().expect("object");
    assert!(
        !child_env.contains_key("AINB_ACP_PLANTED_SECRET"),
        "planted ambient variable reached the adapter: {child_env:?}"
    );
    assert!(child_env.contains_key("PATH"), "PATH is allowlisted");
    assert_eq!(child_env["FAKE_ACP_ENV_DUMP"], dump.to_str().expect("utf8"));
    std::env::remove_var("AINB_ACP_PLANTED_SECRET");
}

/// I13, mode half. The adapter reports the mode it was asked for.
#[tokio::test]
async fn a_matching_mode_at_session_new_needs_no_set_call() {
    let (adapter, _rx) = spawn("default", env(&[("FAKE_ACP_MODE_ON_NEW", "default")])).await;
    let adapter = adapter.expect("spawn");
    let session = adapter.new_session(Path::new("/tmp")).await.expect("session/new");
    assert_eq!(adapter.observed_mode(&session).as_deref(), Some("default"));
}

/// I13, mode half. The spike's exact failure: the adapter reports an inherited
/// `bypassPermissions` and keeps reporting it after the set call.
#[tokio::test]
async fn an_adapter_echoing_a_different_mode_fails_the_spawn() {
    let (adapter, _rx) = spawn(
        "default",
        env(&[
            ("FAKE_ACP_MODE_ON_NEW", "bypassPermissions"),
            ("FAKE_ACP_MODE_ECHO", "bypassPermissions"),
        ]),
    )
    .await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .new_session(Path::new("/tmp"))
        .await
        .expect_err("a mode mismatch must fail the spawn, not warn");
    match error {
        AcpError::ModeMismatch {
            requested,
            observed,
        } => {
            assert_eq!(requested, "default");
            assert_eq!(observed.as_deref(), Some("bypassPermissions"));
        }
        other => panic!("expected ModeMismatch, got {other:?}"),
    }
}

/// I13, load half. The plan calls this the dangerous one: codex config
/// demonstrably does not survive a load, so a session that comes back in a
/// different regime must fail exactly like a bad `session/new`.
#[tokio::test]
async fn a_load_that_reports_a_different_mode_fails_the_spawn() {
    let (adapter, _rx) = spawn(
        "default",
        env(&[
            ("FAKE_ACP_MODE_ON_NEW", "bypassPermissions"),
            ("FAKE_ACP_MODE_ECHO", "bypassPermissions"),
        ]),
    )
    .await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .load_session("fake-session-1", Path::new("/tmp"))
        .await
        .expect_err("a loaded session in the wrong mode must fail");
    match error {
        AcpError::ModeMismatch {
            requested,
            observed,
        } => {
            assert_eq!(requested, "default");
            assert_eq!(observed.as_deref(), Some("bypassPermissions"));
        }
        other => panic!("expected ModeMismatch, got {other:?}"),
    }
}

/// I13 is a standing guarantee, not a spawn-time snapshot: an adapter that
/// flips a live session to `bypassPermissions` mid conversation is recorded
/// and readable, so the pool can refuse the next turn.
#[tokio::test]
async fn a_mid_conversation_mode_flip_marks_the_session_violating() {
    let (adapter, mut rx) = spawn(
        "default",
        env(&[
            ("FAKE_ACP_CHUNKS", "1"),
            ("FAKE_ACP_MODE_FLIP", "bypassPermissions"),
        ]),
    )
    .await;
    let adapter = adapter.expect("spawn");
    let session = adapter.new_session(Path::new("/tmp")).await.expect("session/new");
    assert!(
        !adapter.mode_violated(&session),
        "the spawn asserted the mode"
    );

    adapter.prompt(&session, "hello").await.expect("session/prompt");
    support::drain_notifications(&mut rx).await;

    assert_eq!(
        adapter.observed_mode(&session).as_deref(),
        Some("bypassPermissions")
    );
    assert!(
        adapter.mode_violated(&session),
        "a live session that changed permission regime must be readable as violating"
    );
}

/// An adapter that will not tell us its mode is treated exactly like one that
/// reports the wrong mode: unprovable is not the same as fine.
#[tokio::test]
async fn an_adapter_that_reports_no_modes_fails_the_spawn() {
    let (adapter, _rx) = spawn("default", env(&[("FAKE_ACP_NO_MODES", "1")])).await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .new_session(Path::new("/tmp"))
        .await
        .expect_err("no mode state means no proof");
    assert!(
        matches!(error, AcpError::ModeMismatch { observed: None, .. }),
        "{error:?}"
    );
}

/// Never fire and forget: a `-32602` is typed, not swallowed.
#[tokio::test]
async fn an_invalid_params_reply_is_surfaced_typed() {
    let (adapter, _rx) = spawn(
        "default",
        env(&[
            ("FAKE_ACP_MODE_ON_NEW", "bypassPermissions"),
            ("FAKE_ACP_REFUSE_SET_MODE", "1"),
        ]),
    )
    .await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .new_session(Path::new("/tmp"))
        .await
        .expect_err("the refused set call must fail the spawn");
    match error {
        AcpError::InvalidParams { method, .. } => {
            assert_eq!(method, "session/set_mode");
        }
        other => panic!("expected InvalidParams, got {other:?}"),
    }
}

/// The port's most likely bug: `session/load` replays history as notifications
/// BEFORE its own reply lands. The handler is registered on the builder, so it
/// is live before any request can be issued, and every replayed chunk arrives.
#[tokio::test]
async fn session_load_replay_that_precedes_the_reply_is_not_dropped() {
    let (adapter, mut rx) = spawn("default", env(&[("FAKE_ACP_LOAD_REPLAY", "5")])).await;
    let adapter = adapter.expect("spawn");
    adapter
        .load_session("fake-session-1", Path::new("/tmp"))
        .await
        .expect("session/load");

    let replayed = support::drain_notifications(&mut rx).await;
    assert_eq!(
        replayed.len(),
        5,
        "every pre-reply replay notification was routed"
    );
}

#[tokio::test]
async fn a_prompt_streams_its_chunks_and_ends_the_turn() {
    let (adapter, mut rx) = spawn("default", env(&[("FAKE_ACP_CHUNKS", "3")])).await;
    let adapter = adapter.expect("spawn");
    let session = adapter.new_session(Path::new("/tmp")).await.expect("session/new");
    adapter.prompt(&session, "hello").await.expect("session/prompt");

    let chunks = support::drain_notifications(&mut rx).await;
    assert_eq!(chunks.len(), 3);
    adapter.cancel(&session).expect("session/cancel is a notification");
}

/// A permission sink nothing reads: these suites drive the protocol legs, not
/// R8's answer path (that lives in the daemon's pool tests). Dropping the
/// receiver would answer every ask `Cancelled`, so the sender is leaked
/// deliberately to keep the fixture's behaviour unchanged.
fn permission_sink() -> tokio::sync::mpsc::UnboundedSender<ainb_acp::client::PermissionRequest> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(rx);
    tx
}

/// The gate re-run's unknown-session taxonomy, both shapes, decided in ONE
/// place. A client that only understood claude's typed `-32002` would treat
/// codex's opaque `-32603` as a spawn failure and never rebuild the context.
#[tokio::test]
async fn both_adapters_unknown_session_shapes_mean_rebuild() {
    for shape in ["claude", "codex"] {
        let (adapter, _rx) =
            spawn("default", env(&[("FAKE_ACP_LOAD_UNKNOWN_SESSION", shape)])).await;
        let adapter = adapter.expect("spawn");
        let error = adapter
            .load_session("fake-session-1", Path::new("/tmp"))
            .await
            .expect_err("an unknown session must fail the load");
        assert!(
            error.load_means_rebuild(),
            "{shape} shape must be classified as rebuild: {error:?}"
        );
    }
}

/// ...and the SAFE direction for anything else. A generic `-32603` is a spawn
/// failure, because rebuilding on it would throw away the adapter-side history
/// the load existed to recover.
#[tokio::test]
async fn an_unclassified_internal_error_is_not_a_rebuild() {
    let (adapter, _rx) = spawn(
        "default",
        env(&[("FAKE_ACP_LOAD_UNKNOWN_SESSION", "opaque")]),
    )
    .await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .load_session("fake-session-1", Path::new("/tmp"))
        .await
        .expect_err("the load failed");
    assert!(!error.load_means_rebuild(), "{error:?}");
}

/// A missing `loadSession` capability is the OTHER rebuild case: the adapter
/// cannot resume at all, so re-prime is the only path (R5: the design must not
/// DEPEND on session/load).
#[tokio::test]
async fn a_missing_load_capability_means_rebuild() {
    let (adapter, _rx) = spawn("default", env(&[("FAKE_ACP_NO_LOAD", "1")])).await;
    let adapter = adapter.expect("spawn");
    let error = adapter
        .load_session("fake-session-1", Path::new("/tmp"))
        .await
        .expect_err("no capability, no load");
    assert!(error.load_means_rebuild(), "{error:?}");
}

/// Static adapter config is re-applied after EVERY load, in the same step as
/// the mode. The spike proved config does not survive a load; without this the
/// resumed session runs on a model nobody chose and nothing says so.
#[tokio::test]
async fn static_config_options_are_applied_at_new_and_re_applied_after_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("rpc.log");
    let (tx, _rx) = mpsc::unbounded_channel();
    let config = fake_config(
        "default",
        env(&[("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8 path"))]),
    )
    .config_options(vec![
        ("model".to_string(), "pinned-model".to_string()),
        ("reasoning".to_string(), "high".to_string()),
    ]);
    let adapter = AdapterProcess::spawn(&config, tx, permission_sink()).await.expect("spawn");

    let session = adapter.new_session(Path::new("/tmp")).await.expect("session/new");
    adapter.load_session(&session, Path::new("/tmp")).await.expect("session/load");

    let lines: Vec<String> = std::fs::read_to_string(&log)
        .expect("rpc log")
        .lines()
        .map(str::to_string)
        .collect();
    let applied: Vec<&String> = lines.iter().filter(|line| line.starts_with("config:")).collect();
    assert_eq!(
        applied.len(),
        4,
        "two options at session/new and the SAME two after session/load: {lines:?}"
    );
    assert!(
        applied.iter().filter(|line| line.ends_with("model=pinned-model")).count() == 2,
        "{lines:?}"
    );
    assert!(
        applied.iter().filter(|line| line.ends_with("reasoning=high")).count() == 2,
        "{lines:?}"
    );
}
