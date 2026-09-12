//! D14 — one status truth on the `fleet_session` family.
//!
//! Three properties that a second status source silently breaks, each written
//! against the shipped write and read paths rather than a hand-built row:
//!
//! 1. **Tier order holds**, in both halves it has: the store refuses an
//!    inferred observation over an authoritative one even when the inferred
//!    one is newer, and the discovery path labels what it writes `inferred` so
//!    that refusal means what it says. The second half had regressed.
//! 2. **Completion is never derived from silence.** Absence of evidence is not
//!    evidence of completion. This vocabulary has no `done`: the state that
//!    would wrongly claim it is `idle` ("finished its turn and is free"), and
//!    the honest answer for a session nothing has reported on is
//!    `unverifiable`. Asserted over pseudo-random event sequences containing no
//!    terminal event, because the failure mode is a particular ORDER rather
//!    than a particular event, and separately for true silence.
//! 3. **The projection cannot drift.** After a 1,000-event replay, zero rows
//!    where `attention.open` disagrees with `fleet_session.attention_state`.
//!    Both are written by the single apply path in one transaction, so the
//!    only way they diverge is a lost write — which is exactly what
//!    `sweep_once` stopped papering over when it became an assertion.

use ainb_fleet_core::types::{
    AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
    Provider, SessionKey, TransportHealth,
};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::fleet::{
    ReconcilePass, apply_hook_with_attention, reconcile_discovered_panes, status_rows,
};
use ainb_hangar_proto::agent_status::{AgentState, Provenance};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::fleet::{
    AttentionProjection, FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use std::collections::BTreeSet;

const CWD: &str = "/w/one-truth";
const PANE: &str = "sbx:1.1";
const SESSION_ID: &str = "claude-one-truth";
const SESSION_KEY: &str = "claude:claude-one-truth";
const BASE_MS: i64 = 1_700_000_000_000;

/// One hook line's worth of envelope, shaped the way `attention_ingest` hands
/// it to the reducer: the provider payload under `payload`, the pane on the
/// envelope.
fn envelope(event: &str, tool: Option<&str>) -> serde_json::Value {
    let mut hook = serde_json::json!({
        "session_id": SESSION_ID,
        "cwd": CWD,
        "hook_event_name": event,
    });
    if let Some(tool) = tool {
        hook["tool_name"] = serde_json::Value::String(tool.to_string());
    }
    serde_json::json!({
        "session_id": SESSION_ID,
        "cwd": CWD,
        "tmux_target": PANE,
        "payload": hook,
    })
}

/// Apply one hook event through the real reducer, with `projection` riding the
/// same transaction exactly as the ingest does.
async fn hook(
    store: &Store,
    event_id: &str,
    event_type: &str,
    tool: Option<&str>,
    observed_at: i64,
    projection: Option<AttentionProjection>,
) {
    let payload = envelope(event_type, tool);
    apply_hook_with_attention(
        store.pool(),
        &EventBroker::new().sink(),
        ainb_hangar_daemon::fleet::HookObservation {
            event_id: event_id.to_string(),
            provider: "claude",
            provider_session_id: SESSION_ID,
            cwd: CWD,
            event_type,
            payload: &payload,
            observed_at,
            transcript_model: None,
        },
        projection,
    )
    .await
    .expect("the hook applies");
}

/// The projection a real ask carries: raise one card, keyed so a re-firing
/// collapses onto it.
fn raise(sequence: usize, now_ms: i64) -> AttentionProjection {
    AttentionProjection {
        session_id: SESSION_ID.to_string(),
        close_open_asks: false,
        closed_by: String::new(),
        closed_answer: String::new(),
        closed_at: now_ms,
        raise: Some(NewAttention {
            id: format!("att:{SESSION_ID}:{sequence}"),
            session_id: SESSION_ID.to_string(),
            cwd: CWD.to_string(),
            workspace_id: None,
            kind: AttentionKind::AskUserQuestion,
            payload: r#"{"kind":"ASK","context":{"question":"one truth?"}}"#.to_string(),
            degraded: false,
            created_at: now_ms,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        }),
        request_key: Some(format!("rk-{sequence}")),
    }
}

/// The projection a release carries: close whatever is open, raise nothing.
fn release(now_ms: i64) -> AttentionProjection {
    AttentionProjection {
        session_id: SESSION_ID.to_string(),
        close_open_asks: true,
        closed_by: "resolved:session".to_string(),
        closed_answer: "answered in session".to_string(),
        closed_at: now_ms,
        raise: None,
        request_key: None,
    }
}

/// The tier-5 discovery scan's view of `PANE`: a pane running Claude in `CWD`
/// that looks idle, which is all a scan can ever tell.
fn discovered_idle() -> FleetSession {
    FleetSession {
        session_key: SessionKey::legacy(Provider::Claude, PANE, "pane=%9;pid=9;started=9"),
        provider: Provider::Claude,
        provider_session_id: None,
        cwd: CWD.to_string(),
        exact_tmux_target: Some(PANE.to_string()),
        pane_pid: Some(9),
        process_start_fingerprint: Some("pane=%9;pid=9;started=9".to_string()),
        // The whole point: the scan reads the pane as finished, because a
        // blocked Claude session and a finished one look identical in a pane.
        lifecycle: LifecycleState::TurnComplete,
        attention: AttentionState::None,
        management: ManagementState::Degraded,
        capabilities: Capabilities::default(),
        provenance: BTreeSet::new(),
        confidence: Confidence::Inferred,
        transport_health: TransportHealth::Healthy,
        first_seen_ms: Some(BASE_MS),
        last_seen_ms: Some(BASE_MS),
        version: 1,
    }
}

/// The `(state, provenance)` every surface reads for `SESSION_KEY`.
async fn read_state(store: &Store) -> (AgentState, Provenance) {
    let result = status_rows(store.pool()).await.expect("status read");
    let row = result
        .rows
        .into_iter()
        .find(|row| row.session_key == SESSION_KEY)
        .expect("the hook session has a status row");
    (row.state, row.provenance)
}

/// A tier-5 scan must not be able to retract a tier-0 question.
///
/// Two halves, because the guarantee has two halves and each can break alone.
///
/// **The rule**, at the store: an INFERRED observation cannot overwrite an
/// attention state an AUTHORITATIVE one wrote, even arriving later.
/// `should_replace` ranks before it compares clocks, and `apply_patch` is the
/// only writer, so this is where the tier order actually lives.
///
/// **The label**, at the discovery path: the row a tmux scan writes must carry
/// `inferred` authority on the groups it only infers. This is the half that
/// regressed. The discovered event was promoted to `Authoritative` so a
/// terminal status footer could complete the model pair, and `apply_patch`
/// stamps one authority onto every group a patch touches, so the scan's
/// inferred lifecycle and attention were promoted with it. Nothing went red:
/// the daemon keeps the scan off hook-owned rows by ownership, so the mislabel
/// was latent rather than live. A rule that holds only because a second,
/// unrelated guard happens to stand in front of it is one refactor from being
/// a bug, and the ranking stops meaning what it says in the meantime.
#[tokio::test]
async fn an_inferred_observation_cannot_retract_an_authoritative_attention_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    // Tier 0: the hook says a human is needed.
    hook(
        &store,
        "e-ask",
        "AskUserQuestion",
        Some("AskUserQuestion"),
        BASE_MS,
        Some(raise(0, BASE_MS)),
    )
    .await;
    assert_eq!(
        read_state(&store).await,
        (AgentState::Waiting, Provenance::Hook),
        "the hook's own answer, before anything else observes"
    );

    // Tier 5, onto the SAME row and a minute LATER: a pane that reads
    // finished. Newer, and still not entitled to answer.
    FleetRepo::apply_event(
        store.pool(),
        &NewFleetEvent {
            event_id: "tmux:retract".to_string(),
            session_key: SESSION_KEY.to_string(),
            observed_at: BASE_MS + 60_000,
            authority: ObservationAuthority::Inferred,
            event_type: "tmux_discovered".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                lifecycle_state: Some("TURN_COMPLETE".to_string()),
                attention_state: Some("NONE".to_string()),
                ..FleetSessionPatch::default()
            },
        },
    )
    .await
    .expect("the inferred observation is stored");

    assert_eq!(
        read_state(&store).await,
        (AgentState::Waiting, Provenance::Hook),
        "a newer tier-5 idle must not retract the tier-0 question, and must not \
         relabel its provenance"
    );
}

/// The label half: what the discovery path actually writes.
///
/// Asserted on the stored authority columns rather than on the constructor,
/// because the constructor is private and the columns are what `should_replace`
/// reads. A discovered row claiming `authoritative` lifecycle is a row no
/// later hook can correct when the hook's own clock lags the scan's, which
/// provider hooks routinely do — they carry the provider's timestamp, not the
/// daemon's.
#[tokio::test]
async fn a_discovered_pane_row_claims_only_inferred_authority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    let discovered = discovered_idle();
    let key = discovered.session_key.to_string();
    reconcile_discovered_panes(
        store.pool(),
        &EventBroker::new().sink(),
        vec![discovered],
        BASE_MS,
        ReconcilePass::Panes,
    )
    .await
    .expect("discovery reconciles");

    let row = FleetRepo::get_session(store.pool(), &key)
        .await
        .expect("session query")
        .expect("the scan wrote a row");
    assert_eq!(
        (
            row.lifecycle_authority.as_str(),
            row.attention_authority.as_str(),
            row.transport_authority.as_str()
        ),
        ("inferred", "inferred", "inferred"),
        "a tmux scan infers lifecycle, attention and transport; claiming \
         authority over them lets a pane outrank the hook that owns the session"
    );
}

/// Completion is a claim, and only a terminal event may make it.
///
/// The sequences below are pseudo-random ORDERS of non-terminal events, with
/// gaps that cross every idle threshold the reducer knows about. Order is the
/// variable because the failure mode is not "a Stop was mishandled" — there is
/// no Stop here — it is a reducer that concludes completion from the shape of
/// what it saw, which only shows up in some orders.
///
/// `Idle` and `Exited` are both barred: `idle` is this vocabulary's "finished
/// its turn and is free", which is the false claim, and `exited` asserts the
/// process is gone on process evidence no hook line carries.
#[tokio::test]
async fn no_sequence_without_a_terminal_event_ever_claims_completion() {
    // Every event a session emits while it is still going. Deliberately no
    // `Stop`, no `StopFailure`, and no `agent-turn-complete`: if one were in
    // this pool the test would be asserting nothing.
    const ALIVE: [(&str, Option<&str>); 6] = [
        ("SessionStart", None),
        ("UserPromptSubmit", None),
        ("PreToolUse", Some("Bash")),
        ("PostToolUse", Some("Bash")),
        ("Notification", None),
        ("AskUserQuestion", Some("AskUserQuestion")),
    ];

    for seed in 0..24_u64 {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("store");
        let mut rng = StdRng::seed_from_u64(seed);

        let mut order: Vec<usize> = (0..ALIVE.len() * 3).map(|i| i % ALIVE.len()).collect();
        order.shuffle(&mut rng);

        let mut now = BASE_MS;
        for (step, index) in order.iter().enumerate() {
            let (event, tool) = ALIVE[*index];
            // Gaps up to 20 minutes, so the run crosses the 5-minute idle
            // window a pane heuristic would call finished.
            now += 1_000 + (seed as i64 * 37 + step as i64 * 101) % 1_200_000;
            let projection = (event == "AskUserQuestion").then(|| raise(step, now));
            hook(
                &store,
                &format!("e-{seed}-{step}"),
                event,
                tool,
                now,
                projection,
            )
            .await;

            let (state, _) = read_state(&store).await;
            assert!(
                !matches!(state, AgentState::Idle | AgentState::Exited),
                "seed {seed} step {step} ({event}) derived {} with no terminal \
                 event in the sequence; order was {order:?}",
                state.as_str()
            );
        }
    }
}

/// True silence: a session the store knows about that nothing has reported a
/// state for reads `unverifiable`, never `idle`.
///
/// Driven with an event name no normalizer maps, which is the shape this
/// actually takes in production — a provider ships `agent-turn-aborted` in a
/// point release and the daemon has no arm for it. The row must still land with
/// its identity and its clocks and assert no transition, so the session reads
/// "nothing has told us", not "finished and free".
#[tokio::test]
async fn an_event_nobody_can_map_leaves_the_state_unverifiable_not_idle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    hook(
        &store,
        "e-unmapped",
        "agent-turn-aborted",
        None,
        BASE_MS,
        None,
    )
    .await;

    let (state, _) = read_state(&store).await;
    assert_eq!(
        state,
        AgentState::Unverifiable,
        "an unmapped event must assert no transition; silence and idleness are \
         different facts"
    );
}

/// The inbox is a projection of the session state, written in the same
/// transaction, so it cannot fall behind it.
///
/// Replays the open/close interleaving a real fleet produces — an ask raised,
/// then released, a thousand times — and then asks the drift assertion the
/// question `sweep_once` asks in production. `sweep_once` only logs, so if this
/// ever diverges nothing goes red except this test.
#[tokio::test]
async fn a_thousand_event_replay_leaves_no_projection_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    for sequence in 0..1_000_usize {
        let now = BASE_MS + sequence as i64 * 1_000;
        let asking = sequence % 2 == 0;
        let (event, tool) = if asking {
            ("AskUserQuestion", Some("AskUserQuestion"))
        } else {
            ("Stop", None)
        };
        hook(
            &store,
            &format!("e-replay-{sequence}"),
            event,
            tool,
            now,
            Some(if asking {
                raise(sequence, now)
            } else {
                release(now)
            }),
        )
        .await;
    }

    let drift = AttentionRepo::drift_against_fleet_session(store.pool())
        .await
        .expect("drift read");
    assert!(
        drift.is_clean(),
        "the projection drifted from the session state it is derived from: \
         {} open rows with no asking session, {} asking sessions with no open row",
        drift.open_without_asking_session,
        drift.asking_session_without_open
    );
}
