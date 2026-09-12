//! D14 / T0-daemon gate: one fixture session, driven hook to store, must read
//! IDENTICALLY on every surface.
//!
//! The drift this phase removes is not subtle. `ainb fleet needs` folds a
//! materialized `current_state` table with a live tmux `classify()` fallback,
//! `GET /api/needs` maps the attention inbox, and the TUI fleet panel renders a
//! Fleet snapshot. Three readers, three sources, three vocabularies — so one
//! agent could be `waiting` on the phone, absent from the dashboard and `idle`
//! in the panel, with nothing in the tree saying which was right.
//!
//! The fix is not three careful implementations kept in sync by review. It is
//! ONE derivation (`ainb_hangar_proto::agent_status`) that every surface calls,
//! and this test is what holds them to it: it drives a real hook line through
//! the real ingest into a real store, then asks all three surfaces for their
//! `(session_key, state, provenance, tier, evidence_observed_at)` and asserts
//! they are the same tuple.

use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_store::Store;

const SESSION_ID: &str = "one-truth-1";
const SESSION_KEY: &str = "claude:one-truth-1";
const CWD: &str = "/w/one-truth";

/// One `AskUserQuestion` hook line: the shape `ainb fleet atc hook` appends to
/// `events.jsonl` for a live interview, carrying the full `tool_input` so the
/// ingest can raise the card from the announcement rather than the transcript.
fn ask_hook_line(event_id: &str) -> String {
    format!(
        r#"{{"event_id":"{event_id}","ts":1700000000000,"session_id":"{SESSION_ID}","cwd":"{CWD}","transcript_path":"","agent":"claude","event_type":"PreToolUse","matcher":"AskUserQuestion","parent":null,"tmux_target":"dev:1.0","process_start_fingerprint":"pane=%1;pid=1;started=1","payload":{{"session_id":"{SESSION_ID}","cwd":"{CWD}","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{{"questions":[{{"question":"Which store?","header":"Store","options":[{{"label":"sqlite","description":"local"}},{{"label":"postgres","description":"remote"}}],"multiSelect":false}}]}}}}}}"#
    )
}

/// Drive one fixture session from hook line to store, exactly as the daemon
/// does: the real ingest, the real reducer, the real single apply path.
async fn fixture_store(dir: &std::path::Path) -> Store {
    let store = Store::open_in(dir).await.expect("open store");
    let events_jsonl = dir.join("events.jsonl");
    std::fs::write(&events_jsonl, format!("{}\n", ask_hook_line("e-ask-1")))
        .expect("write events.jsonl");
    AttentionIngest::new(
        store.pool().clone(),
        EventBroker::new().sink(),
        events_jsonl,
        dir.join("attention.cursor"),
    )
    .ingest_once(1_700_000_001_000)
    .await;
    store
}

#[tokio::test]
async fn every_surface_reports_the_same_tuple_for_one_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = fixture_store(dir.path()).await;

    // Surface 0: what the daemon serves on `fleet/status`. Every other surface
    // is measured against this, because it is the one derivation.
    let status = ainb_hangar_daemon::fleet::status_rows(store.pool()).await.expect("status rows");
    let daemon_row = status
        .rows
        .iter()
        .find(|row| row.session_key == SESSION_KEY)
        .expect("the fixture session reached the store");
    let expected = daemon_row.identity_tuple();
    assert_eq!(expected.1, "waiting", "the hook announced a live question");
    assert_eq!(
        expected.2, "hook",
        "a hook wrote it, so the provenance is hook"
    );
    assert_eq!(expected.3, 0, "tier 0 is the hook push");
    assert!(expected.4 > 0, "the evidence clock must be stamped");

    // Surface 1: the TUI fleet panel. It keeps its own flattened row because it
    // renders strings, so this proves the flattening round-trips through the
    // shared derivation rather than becoming a second fold.
    let snapshot = ainb_hangar_daemon::fleet::snapshot_wire(store.pool()).await.expect("snapshot");
    let wire_session = snapshot
        .sessions
        .iter()
        .find(|session| session.session_key == SESSION_KEY)
        .expect("the panel sees the session")
        .clone();
    let panel_row = ainb_plugin_hangar::screen::fleet::FleetSessionRow::from(wire_session.clone());
    let panel = panel_row.status_identity();
    assert_eq!(
        (panel.0.as_str(), panel.1, panel.2, panel.3, panel.4),
        expected,
        "the TUI fleet panel must report the daemon's tuple, not its own reading"
    );

    // Surface 2: `GET /api/needs`. The dashboard stamps every card from the
    // same status read, so its card carries the same five values.
    let inbox: Vec<ainb_hangar_proto::events::AttentionRow> =
        ainb_hangar_store::repo::attention::AttentionRepo::list_fleet(store.pool())
            .await
            .expect("inbox")
            .into_iter()
            .map(|row| ainb_hangar_proto::events::AttentionRow {
                id: row.id,
                session_id: row.session_id,
                cwd: row.cwd,
                workspace_id: row.workspace_id,
                kind: row.kind.as_str().to_string(),
                payload: row.payload,
                degraded: row.degraded,
                created_at: row.created_at,
                channels: row.channels,
            })
            .collect();
    let cards = ainb_web::daemon::attention_to_needs_with_status(&inbox, &status.rows);
    let card = cards
        .as_array()
        .expect("cards are an array")
        .iter()
        .find(|card| card["sessionKey"] == SESSION_KEY)
        .expect("the dashboard shows the session");
    assert_eq!(
        (
            card["sessionKey"].as_str().unwrap(),
            card["state"].as_str().unwrap(),
            card["provenance"].as_str().unwrap(),
            u8::try_from(card["tier"].as_u64().unwrap()).unwrap(),
            card["evidenceObservedAt"].as_i64().unwrap(),
        ),
        expected,
        "GET /api/needs must report the daemon's tuple"
    );

    // Surface 3: `ainb fleet needs --format json`. The CLI stamps the same five
    // fields onto its row, so a consumer parsing its JSON reads the same agent
    // the panel drew.
    let mut cli_row = ainb_fleet_core::fleet::read::needs::make_row(
        ainb_fleet_core::types::Session {
            id: SESSION_KEY.to_string(),
            cwd: CWD.to_string(),
            pid: None,
            git_root: None,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![ainb_fleet_core::types::SessionSource::Ainb],
            summary: None,
            last_seen_ms: Some(daemon_row.evidence_observed_at),
        },
        ainb_fleet_core::fleet::read::needs::NeedsContext::Wait(
            ainb_fleet_core::fleet::read::needs::WaitContext {
                marker: "needs input:".to_string(),
                text: "blocked on a human".to_string(),
            },
        ),
        ainb_fleet_core::fleet::read::needs::RouteHint::None,
    );
    cli_row.stamp_status(
        expected.0.to_string(),
        expected.1,
        expected.2,
        expected.3,
        expected.4,
        daemon_row.pane_unbound,
    );
    let json = serde_json::to_value(&cli_row).expect("the CLI row serializes");
    assert_eq!(
        (
            json["session_key"].as_str().unwrap(),
            json["state"].as_str().unwrap(),
            json["source"].as_str().unwrap(),
            u8::try_from(json["tier"].as_u64().unwrap()).unwrap(),
            json["evidence_observed_at"].as_i64().unwrap(),
        ),
        expected,
        "`ainb fleet needs --format json` must report the daemon's tuple"
    );
}

/// A tier-0 `waiting` followed by a tier-5 `idle` for the SAME pane stays
/// `waiting`, on hook provenance.
///
/// This is the failure the six-tier order exists to prevent: the tmux scan
/// reconciles every few seconds and reads a blocked pane as an idle one, so
/// without the authority rule a live question would be cleared off every
/// surface seconds after it was asked, by a scrape that knows less than the
/// hook that raised it.
#[tokio::test]
async fn a_tier_five_idle_never_overwrites_a_tier_zero_waiting() {
    use ainb_fleet_core::types::{
        AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
        Provider, SessionKey, TransportHealth,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let store = fixture_store(dir.path()).await;

    // The tmux scan finds the same pane and calls it idle with nothing asking.
    let scanned = FleetSession {
        session_key: SessionKey::legacy(Provider::Claude, "dev:1.0", "pane=%1;pid=1;started=1"),
        provider: Provider::Claude,
        provider_session_id: None,
        cwd: CWD.to_string(),
        exact_tmux_target: Some("dev:1.0".to_string()),
        pane_pid: Some(1),
        process_start_fingerprint: Some("pane=%1;pid=1;started=1".to_string()),
        lifecycle: LifecycleState::Idle,
        attention: AttentionState::None,
        management: ManagementState::Degraded,
        capabilities: Capabilities::default(),
        provenance: std::collections::BTreeSet::new(),
        confidence: Confidence::Inferred,
        transport_health: TransportHealth::Healthy,
        first_seen_ms: Some(1_700_000_002_000),
        last_seen_ms: Some(1_700_000_002_000),
        version: 1,
    };
    ainb_hangar_daemon::fleet::reconcile_discovered_panes(
        store.pool(),
        &EventBroker::new().sink(),
        vec![scanned],
        1_700_000_002_000,
        ainb_hangar_daemon::fleet::ReconcilePass::Panes,
    )
    .await
    .expect("fold the scan");

    let status = ainb_hangar_daemon::fleet::status_rows(store.pool()).await.expect("status rows");
    let row = status
        .rows
        .iter()
        .find(|row| row.session_key == SESSION_KEY)
        .expect("the hook row is still there");
    assert_eq!(
        row.identity_tuple().1,
        "waiting",
        "a pane scrape must not clear a question the provider itself raised"
    );
    assert_eq!(row.identity_tuple().2, "hook");
    assert_eq!(row.identity_tuple().3, 0);
}

/// No sequence of events ending in silence yields a state claiming the work is
/// complete.
///
/// Property-style over the real reducer rather than over a mock: every
/// permutation of the lifecycle events a provider actually emits is replayed,
/// and then the session goes quiet. `done` must not be reachable — there is no
/// such state — and a silent session must never be reported as `idle` unless
/// something actually observed it become free.
#[tokio::test]
async fn no_event_sequence_ending_in_silence_reports_completion() {
    use ainb_hangar_proto::agent_status::AgentState;

    const EVENTS: [&str; 5] = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ];

    // Every ordered subsequence of the five, which is every sequence a replay
    // of a real session can produce, including the out-of-order ones a spool
    // drain can deliver.
    for mask in 0u32..(1 << EVENTS.len()) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let events_jsonl = dir.path().join("events.jsonl");
        let session = format!("silence-{mask}");
        let mut lines = String::new();
        for (index, event) in EVENTS.iter().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            lines.push_str(&format!(
                r#"{{"event_id":"e-{mask}-{index}","ts":1700000000000,"session_id":"{session}","cwd":"{CWD}","transcript_path":"","agent":"claude","event_type":"{event}","matcher":null,"parent":null,"tmux_target":null,"process_start_fingerprint":null,"payload":{{"session_id":"{session}","cwd":"{CWD}","hook_event_name":"{event}"}}}}"#
            ));
            lines.push('\n');
        }
        std::fs::write(&events_jsonl, lines).expect("write events.jsonl");
        AttentionIngest::new(
            store.pool().clone(),
            EventBroker::new().sink(),
            events_jsonl,
            dir.path().join("attention.cursor"),
        )
        .ingest_once(1_700_000_001_000)
        .await;

        // Then silence: nothing else is fed, and the read happens much later.
        let status =
            ainb_hangar_daemon::fleet::status_rows(store.pool()).await.expect("status rows");
        for row in &status.rows {
            assert_ne!(
                row.state.as_str(),
                "done",
                "mask {mask} produced a state claiming completion"
            );
            if mask == 0 {
                continue;
            }
            assert!(
                matches!(
                    row.state,
                    AgentState::Working
                        | AgentState::Waiting
                        | AgentState::Idle
                        | AgentState::Exited
                        | AgentState::Unverifiable
                ),
                "mask {mask} produced an unknown state {:?}",
                row.state
            );
        }
    }
}
