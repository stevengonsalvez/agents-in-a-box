//! Issue #961: a pane that changes hands stops receiving the old session's
//! keys.
//!
//! A correlated binding was decided once and never re-confirmed, so a managed
//! row kept `tmux_target` pointing at a pane that a different agent had since
//! taken over. Every send-keys path resolves its target from that column
//! (`rpc/mod.rs` reads `session.tmux_target`, and the Codex path additionally
//! compares `process_start_fingerprint` against the live scan), so the answer
//! to one agent's question was typed into another agent's prompt.
//!
//! This drives the real reducer rather than the binding function: the refusal
//! has to be visible in the COLUMN the router reads, because that is what makes
//! it true for every caller rather than for the one that remembered to ask.

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};

const CWD: &str = "/w/reuse";
const PANE: &str = "dev:1.0";
const SESSION: &str = "sid-961";
const SESSION_KEY: &str = "claude:sid-961";
const BASE_MS: i64 = 1_700_000_000_000;

/// One hook observation, through the daemon's own reducer.
///
/// `fingerprint` of `None` is the #916 shape and the one that matters here: a
/// hook forked from a shared provider daemon never saw `$TMUX_PANE`, so it
/// cannot name its own pane and the daemon's correlation is the only binding
/// there is. A hook that DOES name a pane is reporting the process running in
/// that pane, which is why it is trusted when present.
async fn hook(store: &Store, event_id: &str, fingerprint: Option<&str>, observed_at: i64) {
    let mut inner = serde_json::json!({
        "session_id": SESSION,
        "cwd": CWD,
        "hook_event_name": "PreToolUse",
    });
    let mut payload = serde_json::json!({
        "session_id": SESSION,
        "cwd": CWD,
    });
    if let Some(fingerprint) = fingerprint {
        payload["tmux_target"] = serde_json::Value::String(PANE.to_string());
        payload["process_start_fingerprint"] = serde_json::Value::String(fingerprint.to_string());
        inner["tmux_target"] = serde_json::Value::String(PANE.to_string());
        inner["process_start_fingerprint"] = serde_json::Value::String(fingerprint.to_string());
    }
    payload["payload"] = inner;
    ainb_hangar_daemon::fleet::apply_hook(
        store.pool(),
        &EventBroker::new().sink(),
        ainb_hangar_daemon::fleet::HookObservation {
            event_id: event_id.to_string(),
            provider: "claude",
            provider_session_id: SESSION,
            cwd: CWD,
            event_type: "PreToolUse",
            payload: &payload,
            observed_at,
            transcript_model: None,
        },
    )
    .await
    .expect("the hook applies");
}

/// Seed the tier-5 row for `PANE`, which is what "the pane is now running X"
/// looks like to the daemon.
async fn scan(store: &Store, event_id: &str, fingerprint: &str, observed_at: i64) {
    FleetRepo::apply_event(
        store.pool(),
        &NewFleetEvent {
            event_id: event_id.to_string(),
            session_key: format!("tmux:{PANE}:{fingerprint}"),
            observed_at,
            authority: ObservationAuthority::Inferred,
            event_type: "tmux_discovered".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                provider: Some("claude".to_string()),
                cwd: Some(CWD.to_string()),
                tmux_target: Some(PANE.to_string()),
                process_start_fingerprint: Some(fingerprint.to_string()),
                tier: Some("pane_text".to_string()),
                ..FleetSessionPatch::default()
            },
        },
    )
    .await
    .expect("the scan row applies");
}

/// The whole point: after the pane changes hands, nothing can route to it.
#[tokio::test]
async fn a_pane_taken_over_by_another_agent_stops_receiving_this_session_s_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    // The agent is in `dev:1.0`, and its hook says so.
    hook(&store, "e-bind", Some("pane=%1;pid=100"), BASE_MS).await;
    let bound = FleetRepo::get_session(store.pool(), SESSION_KEY)
        .await
        .expect("query")
        .expect("the hook made a row");
    assert_eq!(
        bound.tmux_target.as_deref(),
        Some(PANE),
        "the hook named its own pane, so the row routes there"
    );
    assert_eq!(
        bound.bound_target.as_deref(),
        Some(PANE),
        "and the decision is recorded, which is what a later pass re-confirms"
    );
    assert_eq!(bound.bound_fingerprint.as_deref(), Some("pane=%1;pid=100"));

    // The agent dies. Something else takes the pane.
    scan(&store, "e-reuse", "pane=%1;pid=999", BASE_MS + 60_000).await;

    // A later event for the ORIGINAL session, in the #916 shape: the hook ran
    // from a shared provider daemon and cannot name its own pane. The daemon
    // therefore has only the scan to go on, and the scan says the pane it bound
    // belongs to someone else now.
    hook(&store, "e-after", None, BASE_MS + 120_000).await;

    let after = FleetRepo::get_session(store.pool(), SESSION_KEY)
        .await
        .expect("query")
        .expect("the row survives");
    assert_eq!(
        after.tmux_target, None,
        "THE refusal: every send-keys path resolves its target from this \
         column, so clearing it is what stops the answer reaching the agent \
         that did not ask"
    );
    assert_eq!(
        after.bound_target, None,
        "and the stale decision goes with it, so nothing re-adopts the pane"
    );
}

/// The same pane, still held by the same process, keeps working. A refusal that
/// fired on every scan would be worse than the bug.
#[tokio::test]
async fn a_pane_still_held_by_its_own_agent_keeps_routing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    hook(&store, "e-bind", Some("pane=%1;pid=100"), BASE_MS).await;
    scan(&store, "e-scan", "pane=%1;pid=100", BASE_MS + 60_000).await;
    hook(&store, "e-after", None, BASE_MS + 120_000).await;

    let after = FleetRepo::get_session(store.pool(), SESSION_KEY)
        .await
        .expect("query")
        .expect("the row survives");
    assert_eq!(
        after.tmux_target.as_deref(),
        Some(PANE),
        "an unchanged pane must keep its route"
    );
    assert_eq!(after.bound_target.as_deref(), Some(PANE));
}
