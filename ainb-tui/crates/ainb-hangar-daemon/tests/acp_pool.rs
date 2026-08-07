//! The multiplexed ACP agent pool, driven against the SCRIPTED FIXTURE adapter.
//!
//! DISCLOSURE: every adapter here is `ainb-acp`'s `fake_acp_adapter` fixture,
//! never a real `claude-agent-acp` / `codex-acp`. The real-adapter probes live
//! in `ainb-acp/tests/real_adapter.rs` behind `#[ignore]`.
//!
//! Covers, per the plan's Phase 5 automated list:
//!
//! * **I4** one prompt puts EXACTLY the final agent message on the timeline
//!   while every chunk lands in the transcript.
//! * **Session demux** two sessions on ONE process keep their transcripts
//!   separate: zero cross-attribution, and a chunk for an id NOBODY owns is
//!   dropped (its permission twin answered `Cancelled`) rather than handed to a
//!   neighbour.
//! * **I16** SIGKILL the shared process while TWO scopes have open turns: both
//!   converge (`acp.turn_interrupted`, terminal delivery, `open_turn_id`
//!   cleared) and both accept a fresh prompt with no daemon restart.
//! * **Bounded queue** a full per-scope FIFO answers REJECTED with `queue_full`,
//!   never unbounded growth.
//! * **I16 queued outcomes** a prompt queued behind a killed turn is resolved
//!   terminal AND never executed afterwards.
//! * **I16 deadline** a per-session deadline cancels only the overdue session,
//!   and a cancel naming a turn that already ended is a no-op.
//! * **R8 round trip** an ANSWERED permission unblocks the adapter AND closes
//!   its attention row and the session's approval state. TWO parked at once are
//!   both answerable in either order, an `Approve` with no allow option to take
//!   refuses rather than picking one, and a dead adapter closes every one of
//!   them.
//! * **Exit is not data loss** an adapter that writes its last chunks and exits
//!   in the same breath still has every one of them in the transcript.
//! * **I6** a prompt that provably never reached the adapter is requeued and
//!   then fails; one that did is UNKNOWN with no second turn.
//! * **I11** broadcast replies land in each recipient's OWN scope, threaded to
//!   the broadcast message.
//! * **I12** transcript wakeups arrive DURING the turn, not at its end.
//! * **LRU** the oldest idle session is evicted while the process stays warm,
//!   and a process stops only after its idle window has actually elapsed.
//! * **I16 boot** a session a daemon killed with SIGKILL left mid-turn is
//!   converged at startup, with no pool and no operator.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::acp_pool::{AcpPool, ConvergeCause, PoolConfig, SubmitOutcome};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::{FleetSessionPatch, NewFleetEvent, ObservationAuthority};
use ainb_hangar_store::repo::fleet_acp_session::{
    FleetAcpSessionRepo, FleetAcpSessionRow, NewFleetAcpSession,
};
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};
use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventRepo;

// ------------------------------------------------------------------ harness

/// The fixture adapter binary, rebuilt on demand.
///
/// `CARGO_BIN_EXE_*` only exists inside the crate that DECLARES the binary, so
/// this crate resolves it by target-directory convention and builds it. A
/// silent skip would turn a broken pool into a green suite.
///
/// The build runs UNCONDITIONALLY (it is a no-op when the fixture is already
/// fresh). Building only when the file is ABSENT is the trap this once hit:
/// `cargo test -p ainb-hangar-daemon` never rebuilds another crate's binary, so
/// an edit to the fixture left a stale executable on disk and every new
/// scripting knob read as "the pool ignored it".
///
/// At most ONCE per test binary: tests share this process, and two threads
/// racing two `cargo build` invocations would serialise on the package lock for
/// no benefit.
fn fake_adapter() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let mut dir = std::env::current_exe().expect("test binary path");
            dir.pop(); // deps/
            if dir.ends_with("deps") {
                dir.pop();
            }
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "-p", "ainb-acp", "--bin", "fake_acp_adapter"])
                .status()
                .expect("build the fixture adapter");
            assert!(status.success(), "fixture adapter build failed");
            let binary = dir.join("fake_acp_adapter");
            assert!(
                binary.exists(),
                "fixture adapter missing at {}",
                binary.display()
            );
            binary
        })
        .clone()
}

fn config(script: &[(&str, &str)]) -> PoolConfig {
    let adapter = ainb_acp::config::AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "default")
        .command(fake_adapter())
        .extra_env(
            script
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        );
    let mut config = PoolConfig::default();
    config.adapters.insert(ainb_acp::config::CLAUDE_ADAPTER.to_string(), adapter);
    config
}

async fn harness(script: &[(&str, &str)]) -> (tempfile::TempDir, Store, Arc<AcpPool>) {
    let (dir, store, pool, _broker) = harness_with_broker(script, |_| {}).await;
    (dir, store, pool)
}

/// The same harness, keeping the broker so a test can watch the live streams,
/// and with a hook to tune the pool config before it is built.
async fn harness_with_broker(
    script: &[(&str, &str)],
    tune: impl FnOnce(&mut PoolConfig),
) -> (tempfile::TempDir, Store, Arc<AcpPool>, EventBroker) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let broker = EventBroker::new();
    let mut cfg = config(script);
    tune(&mut cfg);
    let pool = AcpPool::new(store.clone(), broker.sink(), cfg);
    (dir, store, pool, broker)
}

/// Create one ACP session pair exactly as `fleet/acp_session_create` does.
async fn seed_session(store: &Store, session_key: &str) -> FleetAcpSessionRow {
    let scope_key = format!("session:{session_key}");
    let event = NewFleetEvent {
        event_id: format!("acp-session-create:{session_key}"),
        session_key: session_key.to_string(),
        observed_at: 1,
        authority: ObservationAuthority::Authoritative,
        event_type: "acp_session_created".to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            provider: Some("acp".to_string()),
            cwd: Some("/tmp/acp".to_string()),
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(r#"{"send_prompt":true,"interrupt":true}"#.to_string()),
            lifecycle_state: Some("IDLE".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    let (row, _) = FleetAcpSessionRepo::insert_with_fleet_session(
        store.pool(),
        &NewFleetAcpSession {
            session_key: session_key.to_string(),
            scope_key,
            provider: ainb_acp::config::CLAUDE_ADAPTER.to_string(),
            cwd: "/tmp/acp".to_string(),
            permission_mode: "default".to_string(),
            state: "IDLE".to_string(),
            created_at: 1,
            last_active_at: 1,
        },
        &event,
    )
    .await
    .expect("seed acp session");
    row
}

/// Persist one operator message addressed to `session_key` and return its id.
async fn seed_message(store: &Store, session_key: &str, body: &str) -> String {
    let id = format!("msg-{}-{}", session_key, body.len());
    let row = FleetMessageRepo::insert_message_with_deliveries(
        store.pool(),
        &NewFleetMessage {
            id: id.clone(),
            request_id: None,
            request_fingerprint: None,
            scope_key: format!("session:{session_key}"),
            origin_message_id: None,
            sender: "operator".to_string(),
            kind: "user".to_string(),
            body: body.to_string(),
            created_at: 1,
        },
        &[session_key.to_string()],
    )
    .await
    .expect("seed message");
    row.id
}

async fn delivery_state(
    store: &Store,
    message_id: &str,
    session_key: &str,
) -> Option<(String, Option<String>)> {
    FleetMessageRepo::deliveries_for_message(store.pool(), message_id)
        .await
        .expect("deliveries")
        .into_iter()
        .find(|leg| leg.session_key == session_key)
        .map(|leg| (leg.state, leg.detail))
}

/// Poll until the leg leaves PENDING, or fail loudly.
async fn await_terminal(
    store: &Store,
    message_id: &str,
    session_key: &str,
) -> (String, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((state, detail)) = delivery_state(store, message_id, session_key).await {
            if state != "PENDING" {
                return (state, detail);
            }
        }
        assert!(
            Instant::now() < deadline,
            "delivery {message_id} to {session_key} never resolved"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until `session_key` has an open turn (its adapter session exists and the
/// prompt is genuinely in flight), or fail loudly.
async fn await_open_turn(store: &Store, session_key: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Some(row)) = FleetAcpSessionRepo::get(store.pool(), session_key).await {
            if let Some(turn) = row.open_turn_id {
                return turn;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{session_key} never opened a turn"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Every line the fixture adapter recorded (`FAKE_ACP_RPC_LOG`).
///
/// The store says what the POOL decided; this says what the ADAPTER actually
/// received, which is the only honest evidence for "the prompt was never
/// resent" and for "the cancel named A's session id, not B's".
fn rpc_log(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Poll the fixture's RPC log until `needle` appears, or fail loudly.
async fn await_rpc_line(path: &std::path::Path, needle: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let lines = rpc_log(path);
        if lines.iter().any(|line| line == needle) {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "the adapter never recorded {needle:?}; log: {lines:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Poll until `session_key` has exactly `count` OPEN approval rows, and return
/// them as `(attention_id, requestFingerprint)` in RAISE order.
///
/// The attention id is a ULID, so id order IS raise order, which is what lets a
/// test say "answer the OLDER ask" without guessing.
async fn await_open_permissions(
    store: &Store,
    session_key: &str,
    count: usize,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, payload FROM attention \
             WHERE session_id = ? AND kind = 'approval' AND state = 'open' ORDER BY id ASC",
        )
        .bind(session_key)
        .fetch_all(store.pool())
        .await
        .expect("attention query");
        if rows.len() == count {
            return rows
                .into_iter()
                .map(|(id, payload)| {
                    let payload: serde_json::Value =
                        serde_json::from_str(&payload).expect("payload json");
                    let fingerprint =
                        payload["requestFingerprint"].as_str().expect("fingerprint").to_string();
                    (id, fingerprint)
                })
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "{session_key} never had {count} open permissions (it has {})",
            rows.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One attention row's `(state, answered_by)`.
async fn attention_row(store: &Store, attention_id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, answered_by FROM attention WHERE id = ?")
        .bind(attention_id)
        .fetch_one(store.pool())
        .await
        .expect("attention row")
}

async fn transcript(store: &Store, session_key: &str) -> Vec<(String, String)> {
    FleetProviderEventRepo::list_by_session_after(store.pool(), session_key, 0, 500)
        .await
        .expect("transcript")
        .into_iter()
        .map(|row| (row.event_type, row.raw_payload))
        .collect()
}

// -------------------------------------------------------------------- tests

/// I4: the TIMELINE gets exactly the final agent message; the TRANSCRIPT gets
/// every chunk plus the turn markers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_puts_one_message_on_the_timeline_and_every_chunk_in_the_transcript() {
    let (_dir, store, pool) = harness(&[("FAKE_ACP_CHUNKS", "6")]).await;
    let session = seed_session(&store, "acp:i4").await;
    let message_id = seed_message(&store, &session.session_key, "hello").await;

    assert_eq!(
        pool.submit_prompt(&session.session_key, &message_id, "hello").await,
        SubmitOutcome::Queued
    );
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "DELIVERED", "detail: {detail:?}");

    // Timeline: exactly ONE agent row, threaded to the prompt.
    let replies = FleetMessageRepo::list_by_origin(store.pool(), &message_id, 0, 50)
        .await
        .expect("replies");
    assert_eq!(replies.len(), 1, "one final message, not one per chunk");
    assert_eq!(replies[0].kind, "agent");
    assert_eq!(replies[0].sender, session.session_key);
    assert_eq!(replies[0].scope_key, session.scope_key);
    assert!(
        replies[0].body.contains("chunk-0") && replies[0].body.contains("chunk-5"),
        "the final message is the whole agent text: {:?}",
        replies[0].body
    );

    // Transcript: the chunk stream AND both turn markers.
    let rows = transcript(&store, &session.session_key).await;
    let kinds: Vec<&str> = rows.iter().map(|(kind, _)| kind.as_str()).collect();
    assert!(kinds.contains(&"acp.turn_started"), "{kinds:?}");
    assert!(kinds.contains(&"acp.turn_completed"), "{kinds:?}");
    assert!(
        kinds.contains(&"acp.message"),
        "the agent chunks reached the transcript: {kinds:?}"
    );
    let transcript_text: String = rows.iter().map(|(_, payload)| payload.as_str()).collect();
    for index in 0..6 {
        assert!(
            transcript_text.contains(&format!("chunk-{index}")),
            "chunk-{index} is missing from the transcript"
        );
    }
}

/// Two sessions multiplexed on ONE adapter process keep their transcripts
/// apart. Cross-attribution would put one tenant's output in another's log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_sessions_on_one_process_never_cross_attribute() {
    let (_dir, store, pool) =
        harness(&[("FAKE_ACP_CHUNKS", "2"), ("FAKE_ACP_ECHO_PROMPT", "1")]).await;
    let one = seed_session(&store, "acp:demux-one").await;
    let two = seed_session(&store, "acp:demux-two").await;
    let message_one = seed_message(&store, &one.session_key, "alpha").await;
    let message_two = seed_message(&store, &two.session_key, "bravo-x").await;

    pool.submit_prompt(&one.session_key, &message_one, "alpha").await;
    pool.submit_prompt(&two.session_key, &message_two, "bravo").await;
    await_terminal(&store, &message_one, &one.session_key).await;
    await_terminal(&store, &message_two, &two.session_key).await;

    let text_one: String = transcript(&store, &one.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    let text_two: String = transcript(&store, &two.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    assert!(text_one.contains("echo:alpha"), "{text_one}");
    assert!(
        !text_one.contains("echo:bravo"),
        "cross-attributed: {text_one}"
    );
    assert!(text_two.contains("echo:bravo"), "{text_two}");
    assert!(
        !text_two.contains("echo:alpha"),
        "cross-attributed: {text_two}"
    );

    // One process serves both sessions (the multiplex, graft 6).
    let health = pool.health().await;
    assert_eq!(health.processes.len(), 1, "one process per PROVIDER");
    assert_eq!(health.processes[0].sessions, 2);
    assert_eq!(health.sessions.len(), 2);
}

/// I16: SIGKILL the shared process while TWO scopes have open turns. Both
/// converge, a prompt QUEUED behind one of them gets a defined outcome, and
/// BOTH scopes then accept a FRESH prompt end to end with NO daemon restart.
///
/// The last clause is the one that matters operationally: convergence that
/// leaves the store tidy but the scope unusable until someone restarts the
/// daemon is not convergence, it is a nicer-looking wedge. The hang is keyed on
/// the PROMPT TEXT rather than the adapter session id precisely so the
/// respawned process (whose session ids start over at 1) answers the fresh
/// prompts normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_process_crash_converges_every_session_it_hosted() {
    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_HANG_PROMPTS", "one,two"),
        ("FAKE_ACP_CHUNKS", "1"),
        ("FAKE_ACP_ECHO_PROMPT", "1"),
    ])
    .await;
    let one = seed_session(&store, "acp:crash-one").await;
    let two = seed_session(&store, "acp:crash-two").await;
    let message_one = seed_message(&store, &one.session_key, "one").await;
    let message_two = seed_message(&store, &two.session_key, "two").await;

    pool.submit_prompt(&one.session_key, &message_one, "one").await;
    pool.submit_prompt(&two.session_key, &message_two, "two").await;

    // Wait until BOTH turns are genuinely open before the kill, so the test
    // proves convergence rather than a race with the spawn.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let opened = FleetAcpSessionRepo::list_dirty(store.pool())
            .await
            .expect("dirty")
            .iter()
            .filter(|row| row.open_turn_id.is_some())
            .count();
        if opened == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "both turns never opened");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // One prompt sitting in the FIFO behind a turn that is about to die.
    let queued = seed_message(&store, &one.session_key, "queued-behind").await;
    assert_eq!(
        pool.submit_prompt(&one.session_key, &queued, "queued-behind").await,
        SubmitOutcome::Queued
    );

    assert!(pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await);

    for (session, message) in [(&one, &message_one), (&two, &message_two)] {
        let (state, detail) = await_terminal(&store, message, &session.session_key).await;
        assert!(
            state == "UNKNOWN" || state == "FAILED",
            "{} resolved {state} ({detail:?})",
            session.session_key
        );
        let row = FleetAcpSessionRepo::get(store.pool(), &session.session_key)
            .await
            .expect("row")
            .expect("session row");
        assert!(
            row.open_turn_id.is_none(),
            "{} still carries an open turn",
            session.session_key
        );
        let kinds: Vec<String> = transcript(&store, &session.session_key)
            .await
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        assert!(
            kinds.iter().any(|kind| kind == "acp.turn_interrupted"),
            "{} has no turn_interrupted marker: {kinds:?}",
            session.session_key
        );
    }

    // The queued prompt has a DEFINED outcome carrying its enumerated cause,
    // rather than sitting in a channel nobody will read.
    let (queued_state, queued_detail) = await_terminal(&store, &queued, &one.session_key).await;
    assert_eq!(
        queued_state, "FAILED",
        "a prompt that never reached the adapter is FAILED: {queued_detail:?}"
    );
    assert_eq!(queued_detail.as_deref(), Some("adapter_exit"));

    // THE clause: both scopes take a fresh prompt to completion with no daemon
    // restart, no new session_key, and no operator intervention.
    for session in [&one, &two] {
        let fresh = seed_message(
            &store,
            &session.session_key,
            &format!("fresh-{}", session.session_key),
        )
        .await;
        assert_eq!(
            pool.submit_prompt(&session.session_key, &fresh, "fresh").await,
            SubmitOutcome::Queued,
            "{} refused a fresh prompt after convergence",
            session.session_key
        );
        let (state, detail) = await_terminal(&store, &fresh, &session.session_key).await;
        assert_eq!(
            state, "DELIVERED",
            "{} never completed a fresh turn: {detail:?}",
            session.session_key
        );
        let replies = FleetMessageRepo::list_by_origin(store.pool(), &fresh, 0, 10)
            .await
            .expect("replies");
        assert_eq!(replies.len(), 1, "{} : {replies:?}", session.session_key);
        assert!(
            replies[0].body.contains("echo:fresh"),
            "the fresh turn produced this session's own reply: {:?}",
            replies[0].body
        );
    }
}

/// The per-scope FIFO is BOUNDED: a full queue is an answered REJECTED
/// delivery carrying `queue_full`, never unbounded growth and never a silent
/// drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_per_scope_queue_rejects_rather_than_growing() {
    let (_dir, store, pool) = {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("store");
        let broker = EventBroker::new();
        let mut cfg = config(&[("FAKE_ACP_HANG_SESSIONS", "*")]);
        cfg.queue_depth = 2;
        (dir, store.clone(), AcpPool::new(store, broker.sink(), cfg))
    };
    let session = seed_session(&store, "acp:queue").await;

    let mut outcomes = Vec::new();
    for index in 0..6 {
        let body = "x".repeat(index + 1);
        let message_id = seed_message(&store, &session.session_key, &body).await;
        outcomes.push(pool.submit_prompt(&session.session_key, &message_id, &body).await);
    }
    assert!(
        outcomes.contains(&SubmitOutcome::Rejected("queue_full")),
        "a full queue must answer queue_full: {outcomes:?}"
    );
    let queued = outcomes.iter().filter(|outcome| **outcome == SubmitOutcome::Queued).count();
    assert!(
        queued <= 3,
        "the bounded queue accepted {queued} prompts past its depth of 2"
    );
}

/// I16: a prompt QUEUED behind a killed turn gets a defined outcome AND is
/// never executed afterwards.
///
/// The regression this pins: convergence used to resolve the queued leg
/// terminal and leave the job in the channel, so the actor then picked it up,
/// respawned the adapter and opened a real turn for a delivery whose receipt was
/// already UNKNOWN and could never be corrected (the claim was taken).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_queued_behind_a_killed_turn_is_resolved_and_never_executed() {
    let (_dir, store, pool) = harness(&[("FAKE_ACP_HANG_SESSIONS", "*")]).await;
    let session = seed_session(&store, "acp:queued").await;
    let in_flight = seed_message(&store, &session.session_key, "a").await;
    let queued = seed_message(&store, &session.session_key, "bb").await;

    assert_eq!(
        pool.submit_prompt(&session.session_key, &in_flight, "a").await,
        SubmitOutcome::Queued
    );
    let open = await_open_turn(&store, &session.session_key).await;
    assert_eq!(open, in_flight, "the FIRST prompt owns the turn");
    // Queued behind the in-flight turn: accepted by the bounded FIFO, not yet
    // sent anywhere.
    assert_eq!(
        pool.submit_prompt(&session.session_key, &queued, "bb").await,
        SubmitOutcome::Queued
    );

    assert!(pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await);

    let (in_flight_state, in_flight_detail) =
        await_terminal(&store, &in_flight, &session.session_key).await;
    assert!(
        in_flight_state == "UNKNOWN" || in_flight_state == "FAILED",
        "{in_flight_state} ({in_flight_detail:?})"
    );
    let (queued_state, queued_detail) = await_terminal(&store, &queued, &session.session_key).await;
    assert_eq!(
        queued_state, "FAILED",
        "a prompt that provably never reached the adapter is FAILED, not UNKNOWN: {queued_detail:?}"
    );
    assert_eq!(
        queued_detail.as_deref(),
        Some("adapter_exit"),
        "the drained prompt carries the enumerated cause"
    );

    // The decisive assertion: give the actor room to misbehave, then prove it
    // never opened a turn for the resolved prompt.
    tokio::time::sleep(Duration::from_millis(750)).await;
    let started: Vec<String> = transcript(&store, &session.session_key)
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "acp.turn_started")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(
        started.len(),
        1,
        "exactly ONE turn ever started for this session: {started:?}"
    );
    assert!(
        started[0].contains(&in_flight),
        "and it was the in-flight prompt, not the resolved one: {started:?}"
    );
    let row = FleetAcpSessionRepo::get(store.pool(), &session.session_key)
        .await
        .expect("row")
        .expect("session row");
    assert!(row.open_turn_id.is_none(), "no turn survives convergence");
}

/// I16 deadline: the sweep cancels ONLY the overdue session; a healthy session
/// on the same process is untouched, and the `session/cancel` the adapter
/// receives names A's OWN `sessionId`.
///
/// That last clause is the multiplex's load-bearing detail. Both sessions live
/// on ONE process, so a cancel that omitted the session id (or carried the
/// wrong one) would kill the healthy tenant's turn while the overdue one ran
/// on, and nothing in the store would show it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_deadline_sweep_cancels_only_the_overdue_session() {
    let evidence = tempfile::tempdir().expect("evidence dir");
    let log = evidence.path().join("rpc.log");
    // `fake-session-1` is the FIRST adapter session this process mints, so
    // prompting A before B makes which session hangs deterministic.
    let (_dir, store, pool, _broker) = harness_with_broker(
        &[
            ("FAKE_ACP_HANG_SESSIONS", "fake-session-1"),
            ("FAKE_ACP_CHUNKS", "2"),
            ("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8")),
        ],
        |config| config.turn_deadline = Duration::from_millis(1),
    )
    .await;
    let hung = seed_session(&store, "acp:deadline-hung").await;
    let healthy = seed_session(&store, "acp:deadline-ok").await;
    let hung_message = seed_message(&store, &hung.session_key, "a").await;
    let healthy_message = seed_message(&store, &healthy.session_key, "bb").await;

    pool.submit_prompt(&hung.session_key, &hung_message, "a").await;
    await_open_turn(&store, &hung.session_key).await;
    pool.submit_prompt(&healthy.session_key, &healthy_message, "bb").await;
    let (healthy_state, healthy_detail) =
        await_terminal(&store, &healthy_message, &healthy.session_key).await;
    assert_eq!(healthy_state, "DELIVERED", "{healthy_detail:?}");

    // The healthy session has no open turn left, so only the hung one can be
    // overdue. Sleep past the (1 ms) deadline first.
    tokio::time::sleep(Duration::from_millis(50)).await;
    pool.sweep_once().await;

    let (state, detail) = await_terminal(&store, &hung_message, &hung.session_key).await;
    assert_eq!(state, "UNKNOWN", "{detail:?}");
    assert_eq!(detail.as_deref(), Some("turn_deadline"));
    assert_eq!(
        delivery_state(&store, &healthy_message, &healthy.session_key).await,
        Some(("DELIVERED".to_string(), None)),
        "the other tenant of the same process is untouched"
    );
    assert!(
        FleetAcpSessionRepo::get(store.pool(), &hung.session_key)
            .await
            .expect("row")
            .expect("session row")
            .open_turn_id
            .is_none()
    );

    // The wire evidence: exactly ONE `session/cancel`, naming the overdue
    // session's own adapter id.
    let lines = await_rpc_line(&log, "cancel:fake-session-1").await;
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("cancel:")).count(),
        1,
        "only the overdue session was cancelled on the shared process: {lines:?}"
    );
    assert!(
        !lines.contains(&"cancel:fake-session-2".to_string()),
        "the healthy tenant's session id was never cancelled: {lines:?}"
    );
}

/// The deadline sweep is genuinely WIRED, not just callable: the task
/// `spawn_sweeper` starts expires an overdue turn on its own cadence with no
/// explicit `sweep_once` in the test.
///
/// The production default is 30 minutes (`PoolConfig::turn_deadline`); both it
/// and the tick are plain config fields so this test can compress them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_spawned_sweeper_expires_an_overdue_turn_on_its_own() {
    assert_eq!(
        PoolConfig::default().turn_deadline,
        Duration::from_mins(30),
        "the shipped deadline default"
    );
    let (_dir, store, pool, _broker) =
        harness_with_broker(&[("FAKE_ACP_HANG_SESSIONS", "*")], |config| {
            config.turn_deadline = Duration::from_millis(1);
            config.sweep_interval = Duration::from_millis(50);
        })
        .await;
    let sweeper = pool.spawn_sweeper();
    let session = seed_session(&store, "acp:sweeper").await;
    let message_id = seed_message(&store, &session.session_key, "hang").await;
    pool.submit_prompt(&session.session_key, &message_id, "hang").await;

    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "UNKNOWN", "{detail:?}");
    assert_eq!(
        detail.as_deref(),
        Some("turn_deadline"),
        "the running sweeper, not a manual sweep, ended this turn"
    );
    sweeper.abort();
}

/// A cancel naming a turn that is no longer open is a NO-OP.
///
/// The deadline sweep reads an overdue `open_turn_id` from the store and only
/// then sends the cancel; in the gap that turn can end and the next queued
/// prompt can start. Untargeted, the cancel would converge the FRESH delivery
/// UNKNOWN with detail `turn_deadline` for a turn seconds old.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_for_a_turn_that_already_ended_is_ignored() {
    let (_dir, store, pool) = harness(&[("FAKE_ACP_HANG_SESSIONS", "*")]).await;
    let session = seed_session(&store, "acp:stale-cancel").await;
    let message_id = seed_message(&store, &session.session_key, "live").await;
    pool.submit_prompt(&session.session_key, &message_id, "live").await;
    await_open_turn(&store, &session.session_key).await;

    assert!(
        pool.cancel_turn(
            &session.session_key,
            ConvergeCause::TurnDeadline,
            Some("msg-that-already-ended".to_string()),
        )
        .await,
        "the message reaches the actor"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        delivery_state(&store, &message_id, &session.session_key).await.map(|leg| leg.0),
        Some("PENDING".to_string()),
        "a stale cancel must not resolve the turn that succeeded it"
    );

    // An untargeted cancel still converges the live turn, so the guard has not
    // simply broken cancellation.
    assert!(pool.cancel(&session.session_key, ConvergeCause::OperatorStop).await);
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "UNKNOWN");
    // The taxonomy is what makes "did the adapter exit?" countable: this
    // adapter is alive and warm, and calling an operator stop `adapter_exit`
    // would inflate every crash figure by every Interrupt.
    assert_eq!(
        detail.as_deref(),
        Some("operator_stop"),
        "an operator stop is NOT an adapter exit"
    );
    assert!(
        transcript(&store, &session.session_key)
            .await
            .iter()
            .any(|(kind, payload)| kind == "acp.turn_interrupted"
                && payload.contains("operator_stop")),
        "the interrupt marker carries the same enumerated cause"
    );
}

/// The demux DROPS a `session/update` whose `sessionId` no session owns, and
/// ANSWERS the permission twin `Cancelled` rather than leaving the adapter
/// blocked.
///
/// The other half of the cross-attribution guarantee, and the half a
/// `routes.values().next()` fallback would silently pass: the neighbour test
/// only ever emits chunks for ids that ARE routed, so it cannot observe a ghost
/// id being handed to whoever happens to be first in the map.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_for_an_unowned_session_id_is_dropped_never_cross_attributed() {
    let evidence = tempfile::tempdir().expect("evidence dir");
    let log = evidence.path().join("rpc.log");
    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_CHUNKS", "1"),
        ("FAKE_ACP_ECHO_PROMPT", "1"),
        ("FAKE_ACP_GHOST_SESSION", "fake-session-ghost"),
        ("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8")),
    ])
    .await;
    let one = seed_session(&store, "acp:ghost-one").await;
    let two = seed_session(&store, "acp:ghost-two").await;
    let message_one = seed_message(&store, &one.session_key, "alpha").await;
    let message_two = seed_message(&store, &two.session_key, "bravo-x").await;

    // Sequential, so the number of ghost emissions is exactly the number of
    // turns and the evidence count is deterministic.
    pool.submit_prompt(&one.session_key, &message_one, "alpha").await;
    await_terminal(&store, &message_one, &one.session_key).await;
    pool.submit_prompt(&two.session_key, &message_two, "bravo").await;
    await_terminal(&store, &message_two, &two.session_key).await;

    for (session, own) in [(&one, "echo:alpha"), (&two, "echo:bravo")] {
        let text: String = transcript(&store, &session.session_key)
            .await
            .iter()
            .map(|(_, payload)| payload.as_str())
            .collect();
        assert!(
            text.contains(own),
            "{} lost its own text: {text}",
            session.session_key
        );
        assert!(
            !text.contains("ghost-text"),
            "{} was handed a chunk for a session id nobody owns: {text}",
            session.session_key
        );
    }
    // A ghost permission must not raise an ask against a neighbour either: a
    // row here is an operator asked to approve a tool call for a session that
    // does not exist.
    let asks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attention")
        .fetch_one(store.pool())
        .await
        .expect("attention count");
    assert_eq!(
        asks, 0,
        "no attention row belongs to an unrouted permission"
    );

    // The decisive wire evidence: the adapter was BLOCKED on that permission
    // and got a real `Cancelled` answer, once per turn. Dropping it instead
    // would hang the turn, not just lose a row.
    let lines = await_rpc_line(&log, "permission:fake-session-ghost:cancelled").await;
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("permission:")).count(),
        2,
        "one answered ghost permission per turn: {lines:?}"
    );
}

/// The BOOT scan (I16): a session a daemon killed with SIGKILL left mid-turn
/// is converged at startup, with no pool, no actor and no operator involved.
///
/// Without it a daemon that died in a turn leaves `open_turn_id` set and the
/// leg PENDING forever: the process-exit and deadline paths only ever see
/// sessions THIS process is hosting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_boot_scan_converges_a_session_a_dead_daemon_left_dirty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let session = seed_session(&store, "acp:boot").await;
    let message_id = seed_message(&store, &session.session_key, "mid-turn").await;

    // Exactly the shape a SIGKILL leaves behind.
    FleetAcpSessionRepo::set_open_turn(store.pool(), &session.session_key, &message_id, 10)
        .await
        .expect("open turn");
    FleetAcpSessionRepo::set_state(store.pool(), &session.session_key, "ACTIVE", 10)
        .await
        .expect("active");

    ainb_hangar_daemon::acp_pool::converge_dirty_sessions_at_boot(
        store.pool(),
        &EventBroker::new().sink(),
    )
    .await;

    assert_eq!(
        delivery_state(&store, &message_id, &session.session_key).await,
        Some(("UNKNOWN".to_string(), Some("daemon_restart".to_string()))),
        "the stuck leg gets its enumerated terminal outcome"
    );
    let row = FleetAcpSessionRepo::get(store.pool(), &session.session_key)
        .await
        .expect("row")
        .expect("session row");
    assert!(row.open_turn_id.is_none(), "the open turn is closed out");
    assert_eq!(row.state, "IDLE", "and the scope is reusable");
    assert!(
        transcript(&store, &session.session_key)
            .await
            .iter()
            .any(|(kind, _)| kind == "acp.turn_interrupted"),
        "a reader can tell this turn was cut short"
    );
    assert!(
        FleetAcpSessionRepo::list_dirty(store.pool()).await.expect("dirty").is_empty(),
        "nothing is left for the next boot to find"
    );
}

/// R8 round trip: answering a permission unblocks the adapter AND retires the
/// attention row plus the session's approval state.
///
/// Before this, an answered permission left `attention.state = 'open'` and
/// `fleet_session.attention_state = 'APPROVAL'` with a stale fingerprint
/// forever: the operator's list carried a ghost row for a decision they had
/// already made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answered_permission_closes_its_attention_row() {
    use ainb_hangar_daemon::acp_pool::{PermissionAnswer, PermissionDecision};
    use ainb_hangar_store::repo::fleet::FleetRepo;

    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
        ("FAKE_ACP_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:permission").await;
    let message_id = seed_message(&store, &session.session_key, "rm").await;
    pool.submit_prompt(&session.session_key, &message_id, "rm").await;

    // Wait for the raised attention row and read the fingerprint the answer
    // must carry (the staleness machinery `fleet/action` validates against).
    let deadline = Instant::now() + Duration::from_secs(20);
    let (attention_id, fingerprint) = loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT id, payload FROM attention WHERE session_id = ? AND state = 'open'",
        )
        .bind(&session.session_key)
        .fetch_optional(store.pool())
        .await
        .expect("attention query");
        if let Some((id, payload)) = row {
            let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
            break (
                id,
                payload["requestFingerprint"].as_str().expect("fingerprint").to_string(),
            );
        }
        assert!(Instant::now() < deadline, "no permission was ever raised");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert_eq!(
        pool.answer_permission(
            &session.session_key,
            &fingerprint,
            PermissionDecision::Approve
        )
        .await,
        PermissionAnswer::Delivered("allow-once".to_string())
    );
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "DELIVERED", "{detail:?}");

    // The adapter genuinely observed the answer.
    let text: String = transcript(&store, &session.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    assert!(
        text.contains("permission:selected:allow-once"),
        "the adapter was unblocked with the chosen option: {text}"
    );

    // ... and NOTHING is left waiting.
    let (attention_state, answered_by): (String, Option<String>) =
        sqlx::query_as("SELECT state, answered_by FROM attention WHERE id = ?")
            .bind(&attention_id)
            .fetch_one(store.pool())
            .await
            .expect("attention row");
    assert_eq!(attention_state, "answered", "the ask must close");
    assert_eq!(answered_by.as_deref(), Some("operator"));
    let fleet = FleetRepo::get_session(store.pool(), &session.session_key)
        .await
        .expect("fleet session")
        .expect("row");
    assert_eq!(
        fleet.attention_state, "NONE",
        "the snapshot must stop showing this session as awaiting approval"
    );
    assert_eq!(fleet.current_request_fingerprint, None);
}

/// TWO permissions parked on ONE session are BOTH answerable, in either order,
/// and answering one leaves the other open.
///
/// The regression this pins: `raise_permission` overwrites the session row's
/// single `current_request_fingerprint` per ask, and the answer path refused
/// any fingerprint that was not the current one. With two asks outstanding only
/// the NEWEST could ever be answered: the older adapter request stayed blocked
/// until the 30 minute turn deadline, with an attention row an operator could
/// click forever. The parked map is the authority for liveness now, and the row
/// is re-pointed at the oldest ask that is still waiting.
///
/// DISCLOSURE: the two concurrent asks come from the fixture's
/// `FAKE_ACP_PERMISSION_COUNT`; a real adapter raises them by running parallel
/// tool calls in one turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_parked_permissions_are_both_answerable_newest_first() {
    use ainb_hangar_daemon::acp_pool::{PermissionAnswer, PermissionDecision};
    use ainb_hangar_store::repo::fleet::FleetRepo;

    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
        ("FAKE_ACP_PERMISSION_COUNT", "2"),
        ("FAKE_ACP_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:two-permissions").await;
    let message_id = seed_message(&store, &session.session_key, "rm").await;
    pool.submit_prompt(&session.session_key, &message_id, "rm").await;

    let asks = await_open_permissions(&store, &session.session_key, 2).await;
    let (older_id, older_fingerprint) = asks[0].clone();
    let (newer_id, newer_fingerprint) = asks[1].clone();
    assert_ne!(
        older_fingerprint, newer_fingerprint,
        "two asks must have two identities"
    );

    // NEWEST first: the only order the old fingerprint gate survived.
    assert_eq!(
        pool.answer_permission(
            &session.session_key,
            &newer_fingerprint,
            PermissionDecision::Approve
        )
        .await,
        PermissionAnswer::Delivered("allow-once".to_string())
    );
    assert_eq!(attention_row(&store, &newer_id).await.0, "answered");
    assert_eq!(
        attention_row(&store, &older_id).await.0,
        "open",
        "answering one ask must not close the other"
    );
    let waiting = FleetRepo::get_session(store.pool(), &session.session_key)
        .await
        .expect("fleet session")
        .expect("row");
    assert_eq!(
        waiting.attention_state, "APPROVAL",
        "one ask is still waiting, so the session is still awaiting approval"
    );
    assert_eq!(
        waiting.current_request_fingerprint.as_deref(),
        Some(older_fingerprint.as_str()),
        "the row must point at the ask that is genuinely open, not the answered one"
    );

    // ... and the OLDER one is answerable too, which is the whole finding.
    assert_eq!(
        pool.answer_permission(
            &session.session_key,
            &older_fingerprint,
            PermissionDecision::Approve
        )
        .await,
        PermissionAnswer::Delivered("allow-once".to_string())
    );
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(
        state, "DELIVERED",
        "the turn only ends once BOTH adapter requests are unblocked: {detail:?}"
    );

    // The adapter genuinely observed two answers, not one.
    let text: String = transcript(&store, &session.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    assert_eq!(
        text.matches("permission:selected:allow-once").count(),
        2,
        "both blocked adapter requests were answered: {text}"
    );
    assert_eq!(attention_row(&store, &older_id).await.0, "answered");
    let settled = FleetRepo::get_session(store.pool(), &session.session_key)
        .await
        .expect("fleet session")
        .expect("row");
    assert_eq!(settled.attention_state, "NONE");
    assert_eq!(settled.current_request_fingerprint, None);
}

/// An adapter that dies with TWO permissions parked closes both rows, with no
/// ghost left behind for either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_adapter_death_closes_every_parked_permission() {
    use ainb_hangar_store::repo::fleet::FleetRepo;

    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
        ("FAKE_ACP_PERMISSION_COUNT", "2"),
        ("FAKE_ACP_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:permission-crash").await;
    let message_id = seed_message(&store, &session.session_key, "rm").await;
    pool.submit_prompt(&session.session_key, &message_id, "rm").await;

    let asks = await_open_permissions(&store, &session.session_key, 2).await;
    assert!(pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await);

    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert!(
        state == "UNKNOWN" || state == "FAILED",
        "the killed turn resolved {state} ({detail:?})"
    );
    for (attention_id, _) in &asks {
        let (row_state, answered_by) = attention_row(&store, attention_id).await;
        assert_eq!(
            row_state, "answered",
            "{attention_id} survived its dead adapter as a ghost row"
        );
        assert_eq!(answered_by.as_deref(), Some("hangar-converge"));
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM attention WHERE session_id = ? AND state = 'open'",
    )
    .bind(&session.session_key)
    .fetch_one(store.pool())
    .await
    .expect("open count");
    assert_eq!(open, 0, "nothing is left waiting on a dead adapter");
    let fleet = FleetRepo::get_session(store.pool(), &session.session_key)
        .await
        .expect("fleet session")
        .expect("row");
    assert_eq!(fleet.attention_state, "NONE");
    assert_eq!(fleet.current_request_fingerprint, None);
}

/// `Approve` never falls back to "whatever option came first".
///
/// The regression: the option was picked by substring-matching a `Debug`
/// rendering, with `options.first()` as a fallback. Against an adapter that
/// offers no allow-flavoured option at all, that made Approve SELECT THE
/// REJECT and report it as an approval. It refuses now, and refuses without
/// spending the adapter's one reply slot, so the same ask is still answerable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_refuses_when_the_adapter_offered_no_allow_option() {
    use ainb_hangar_daemon::acp_pool::{PermissionAnswer, PermissionDecision};

    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_PERMISSION_SESSIONS", "*"),
        ("FAKE_ACP_PERMISSION_NO_ALLOW", "1"),
        ("FAKE_ACP_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:no-allow").await;
    let message_id = seed_message(&store, &session.session_key, "rm").await;
    pool.submit_prompt(&session.session_key, &message_id, "rm").await;

    let asks = await_open_permissions(&store, &session.session_key, 1).await;
    let (attention_id, fingerprint) = asks[0].clone();
    assert_eq!(
        pool.answer_permission(
            &session.session_key,
            &fingerprint,
            PermissionDecision::Approve
        )
        .await,
        PermissionAnswer::UnknownOption,
        "there is nothing to approve WITH; selecting the reject would be a lie"
    );
    assert_eq!(
        attention_row(&store, &attention_id).await.0,
        "open",
        "a refused answer must leave the ask answerable, not orphan its responder"
    );

    // Still answerable, and the adapter sees the decision the operator made.
    assert_eq!(
        pool.answer_permission(&session.session_key, &fingerprint, PermissionDecision::Deny)
            .await,
        PermissionAnswer::Delivered("reject-once".to_string())
    );
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "DELIVERED", "{detail:?}");
    let text: String = transcript(&store, &session.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    assert!(
        text.contains("permission:selected:reject-once"),
        "the adapter was unblocked with the REJECT the operator chose: {text}"
    );
}

/// An adapter that writes its last chunks and exits in the same breath still
/// has every one of them in the transcript.
///
/// Two ways they used to be dropped: the supervisor's `select!` was unbiased,
/// so `wait_closed()` could win over a `session/update` already sitting in the
/// channel; and the actor's process-exit arm dropped the receiver (and the
/// reducer's pending text) without draining either. Both are data the adapter
/// genuinely produced, and the transcript is the only place it exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_adapter_that_exits_after_its_chunks_still_commits_them() {
    const CHUNKS: usize = 40;

    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_CHUNKS", "40"),
        ("FAKE_ACP_DIE_AFTER_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:burst-exit").await;
    let message_id = seed_message(&store, &session.session_key, "burst").await;
    pool.submit_prompt(&session.session_key, &message_id, "burst").await;

    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert!(
        state == "UNKNOWN" || state == "FAILED",
        "a prompt whose adapter died has no honest terminal state but this: {state} ({detail:?})"
    );
    let text: String = transcript(&store, &session.session_key)
        .await
        .iter()
        .map(|(_, payload)| payload.as_str())
        .collect();
    for index in 0..CHUNKS {
        assert!(
            text.contains(&format!("chunk-{index} ")),
            "chunk {index} was written by the adapter and never reached the transcript"
        );
    }
}

/// I6, the pre-write half: a prompt that never reached the adapter is retried
/// ONCE and then fails, with no turn ever opened.
///
/// DISCLOSURE: the injection is a spawn that cannot succeed (a command that does
/// not exist), which is the honest fixture for "the failure happened before
/// `session/prompt` was issued". The post-write half is the crash test above.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_that_never_reached_the_adapter_fails_without_a_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let broker = EventBroker::new();
    let mut cfg = PoolConfig::default();
    cfg.adapters.insert(
        ainb_acp::config::CLAUDE_ADAPTER.to_string(),
        ainb_acp::config::AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "default")
            .command(dir.path().join("no-such-adapter-binary")),
    );
    let pool = AcpPool::new(store.clone(), broker.sink(), cfg);

    let session = seed_session(&store, "acp:never-sent").await;
    let message_id = seed_message(&store, &session.session_key, "hi").await;
    pool.submit_prompt(&session.session_key, &message_id, "hi").await;

    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(state, "FAILED", "{detail:?}");
    assert!(
        transcript(&store, &session.session_key)
            .await
            .iter()
            .all(|(kind, _)| kind != "acp.turn_started"),
        "nothing reached the adapter, so no turn may be recorded"
    );
}

/// I6, the fault injection: the adapter dies with the request in hand and NO
/// reply on the wire, in the window between the pool claiming the turn and the
/// `session/prompt` write. The prompt provably never reached it, so exactly ONE
/// requeue is legal, and the adapter must see exactly ONE prompt in total.
///
/// The marker file makes the death fire for the FIRST process only, so the
/// legal requeue can succeed: that is what separates "requeued once" from
/// "failed twice", which the pre-existing spawn-refused test could not tell
/// apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_lost_before_the_stdin_write_is_requeued_exactly_once() {
    let evidence = tempfile::tempdir().expect("evidence dir");
    let log = evidence.path().join("rpc.log");
    let marker = evidence.path().join("died-once");
    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8")),
        (
            "FAKE_ACP_DIE_ON_SESSION_NEW",
            marker.to_str().expect("utf8"),
        ),
        ("FAKE_ACP_CHUNKS", "1"),
    ])
    .await;
    let session = seed_session(&store, "acp:i6-pre-write").await;
    let message_id = seed_message(&store, &session.session_key, "hi").await;
    pool.submit_prompt(&session.session_key, &message_id, "hi").await;

    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(
        state, "DELIVERED",
        "the ONE legal requeue must carry the prompt through: {detail:?}"
    );

    let lines = rpc_log(&log);
    assert_eq!(
        lines.iter().filter(|line| *line == "spawn").count(),
        2,
        "exactly one requeue, so exactly two spawns: {lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("prompt:")).count(),
        1,
        "the prompt reached the adapter EXACTLY once: {lines:?}"
    );
    assert!(
        lines.contains(&"die:session_new".to_string()),
        "the injection actually fired: {lines:?}"
    );
    assert_eq!(
        transcript(&store, &session.session_key)
            .await
            .iter()
            .filter(|(kind, _)| kind == "acp.turn_started")
            .count(),
        1,
        "the requeue reused the same turn, it did not open a second one"
    );
}

/// I6, the post-write half: the adapter is `SIGKILL`ed AFTER `session/prompt` was
/// issued. The honest answer is a terminal UNKNOWN, and the prompt is never
/// resent, because a resend is exactly the double delivery I6 forbids.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_killed_after_it_was_issued_is_unknown_and_never_resent() {
    let evidence = tempfile::tempdir().expect("evidence dir");
    let log = evidence.path().join("rpc.log");
    let (_dir, store, pool) = harness(&[
        ("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8")),
        ("FAKE_ACP_HANG_SESSIONS", "*"),
    ])
    .await;
    let session = seed_session(&store, "acp:i6-post-write").await;
    let message_id = seed_message(&store, &session.session_key, "rm -rf").await;
    pool.submit_prompt(&session.session_key, &message_id, "rm -rf").await;

    // The prompt is genuinely ON the wire before the kill: without this the
    // test would prove the pre-write case again.
    let open_turn = await_open_turn(&store, &session.session_key).await;
    assert_eq!(open_turn, message_id, "the open turn is this message");
    let lines = await_rpc_line(&log, "prompt:fake-session-1:rm -rf").await;
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("prompt:")).count(),
        1,
        "{lines:?}"
    );

    assert!(pool.kill_provider(ainb_acp::config::CLAUDE_ADAPTER).await);
    let (state, detail) = await_terminal(&store, &message_id, &session.session_key).await;
    assert_eq!(
        state, "UNKNOWN",
        "a prompt that WAS issued cannot be called failed: {detail:?}"
    );
    assert!(
        detail.as_deref().is_some_and(|detail| detail.starts_with("adapter_exit")),
        "{detail:?}"
    );

    // Give the pool room to misbehave, then prove no second prompt was written
    // and no second process was spawned to carry one.
    tokio::time::sleep(Duration::from_millis(750)).await;
    let lines = rpc_log(&log);
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("prompt:")).count(),
        1,
        "an issued prompt is NEVER resent: {lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|line| *line == "spawn").count(),
        1,
        "and nothing respawned to carry one: {lines:?}"
    );
}

/// I11: a BROADCAST prompt's replies land in each recipient's OWN scope,
/// threaded to the broadcast message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broadcast_replies_thread_into_each_recipients_own_scope() {
    let (_dir, store, pool) =
        harness(&[("FAKE_ACP_CHUNKS", "1"), ("FAKE_ACP_ECHO_PROMPT", "1")]).await;
    let one = seed_session(&store, "acp:bcast-one").await;
    let two = seed_session(&store, "acp:bcast-two").await;

    // One message, one broadcast scope, two delivery legs: exactly what
    // `message_send` writes for a two-target send.
    let broadcast = FleetMessageRepo::insert_message_with_deliveries(
        store.pool(),
        &NewFleetMessage {
            id: "msg-broadcast".to_string(),
            request_id: None,
            request_fingerprint: None,
            scope_key: "broadcast:01J0TEST".to_string(),
            origin_message_id: None,
            sender: "operator".to_string(),
            kind: "user".to_string(),
            body: "standup".to_string(),
            created_at: 1,
        },
        &[one.session_key.clone(), two.session_key.clone()],
    )
    .await
    .expect("broadcast row");

    pool.submit_prompt(&one.session_key, &broadcast.id, "standup").await;
    pool.submit_prompt(&two.session_key, &broadcast.id, "standup").await;
    await_terminal(&store, &broadcast.id, &one.session_key).await;
    await_terminal(&store, &broadcast.id, &two.session_key).await;

    let replies = FleetMessageRepo::list_by_origin(store.pool(), &broadcast.id, 0, 50)
        .await
        .expect("thread view");
    assert_eq!(replies.len(), 2, "one reply per recipient: {replies:?}");
    for session in [&one, &two] {
        let reply = replies
            .iter()
            .find(|row| row.sender == session.session_key)
            .unwrap_or_else(|| panic!("no reply from {}", session.session_key));
        assert_eq!(
            reply.scope_key, session.scope_key,
            "a broadcast reply lands in the RECIPIENT'S scope, never the broadcast scope"
        );
        assert_eq!(
            reply.origin_message_id.as_deref(),
            Some(broadcast.id.as_str())
        );
        assert_eq!(reply.kind, "agent");
    }
}

/// I12: transcript rows are committed and broadcast DURING the turn, not at its
/// end.
///
/// The script deliberately crosses a KIND boundary early (message, then
/// thought, then more message) and paces itself: coalescing merges contiguous
/// same-kind text until 4 KiB or a kind change, so a boundary is what makes the
/// first row commit mid-turn. A single unbroken text turn legitimately commits
/// once, which is coalescing working, not the live leg failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transcript_chunks_stream_before_the_turn_ends() {
    let script_dir = tempfile::tempdir().expect("script dir");
    let script_path = script_dir.path().join("paced.ndjson");
    let mut script = String::new();
    for line in [
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"early "}}"#,
        r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking "}}"#,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"late "}}"#,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"later "}}"#,
    ] {
        script.push_str(line);
        script.push('\n');
    }
    std::fs::write(&script_path, script).expect("write script");

    let (_dir, store, pool, broker) = harness_with_broker(
        &[
            ("FAKE_ACP_SCRIPT", script_path.to_str().expect("utf8 path")),
            ("FAKE_ACP_CHUNK_DELAY_MS", "150"),
        ],
        |config| config.writer.flush_interval = Duration::from_millis(25),
    )
    .await;
    let mut chunks = broker.subscribe_transcript();
    let session = seed_session(&store, "acp:live").await;
    let message_id = seed_message(&store, &session.session_key, "stream").await;
    pool.submit_prompt(&session.session_key, &message_id, "stream").await;

    // Wakeups for THIS session until an AGENT CHUNK (not just the turn_started
    // marker) is committed: that is the live leg R4 promises. The delivery must
    // still be PENDING at that moment, which is what makes it "during the turn"
    // rather than "at its end".
    let deadline = Instant::now() + Duration::from_secs(20);
    let rows = loop {
        let (woken, _order) = tokio::time::timeout(Duration::from_secs(20), chunks.recv())
            .await
            .expect("a transcript wakeup within the turn")
            .expect("broadcast alive");
        if woken == session.session_key {
            let rows = transcript(&store, &session.session_key).await;
            if rows.iter().any(|(kind, _)| kind == "acp.message") {
                break rows;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no agent chunk was ever committed for this session"
        );
    };
    let mid_turn = delivery_state(&store, &message_id, &session.session_key).await;
    assert_eq!(
        mid_turn.map(|leg| leg.0),
        Some("PENDING".to_string()),
        "the first transcript chunk must arrive BEFORE the turn resolves"
    );
    assert!(
        rows.iter().all(|(kind, _)| kind != "acp.turn_completed"),
        "the turn has not ended yet: {rows:?}"
    );
    await_terminal(&store, &message_id, &session.session_key).await;
}

/// LRU: with N+1 sessions on one provider the least recently used IDLE session
/// is closed VIA `session/close` when the new tenant arrives at the cap, its
/// state becomes EVICTED, its stable key survives, and the process stays warm.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lru_evicts_a_session_and_keeps_the_process_warm() {
    let evidence = tempfile::tempdir().expect("evidence dir");
    let log = evidence.path().join("rpc.log");
    let (_dir, store, pool, _broker) = harness_with_broker(
        &[
            ("FAKE_ACP_CHUNKS", "1"),
            ("FAKE_ACP_RPC_LOG", log.to_str().expect("utf8")),
        ],
        |config| {
            config.max_sessions_per_provider = 1;
        },
    )
    .await;
    let first = seed_session(&store, "acp:lru-first").await;
    let second = seed_session(&store, "acp:lru-second").await;

    let first_message = seed_message(&store, &first.session_key, "a").await;
    pool.submit_prompt(&first.session_key, &first_message, "a").await;
    await_terminal(&store, &first_message, &first.session_key).await;

    let second_message = seed_message(&store, &second.session_key, "bb").await;
    pool.submit_prompt(&second.session_key, &second_message, "bb").await;
    await_terminal(&store, &second_message, &second.session_key).await;

    let evicted = FleetAcpSessionRepo::get(store.pool(), &first.session_key)
        .await
        .expect("row")
        .expect("session row");
    assert_eq!(evicted.state, "EVICTED", "the idle tenant made room");
    assert_eq!(
        evicted.session_key, first.session_key,
        "the stable key survives eviction (I5)"
    );
    let health = pool.health().await;
    assert_eq!(health.processes.len(), 1, "the process stays warm");
    assert_eq!(health.processes[0].state, "running");
    assert_eq!(health.evicted_total, 1);
    // The health pane must not keep rendering the victim as a live IDLE tenant
    // while the store says EVICTED: an operator reading the two disagrees about
    // which sessions this process is actually hosting.
    let victim = health
        .sessions
        .iter()
        .find(|row| row.session_key == first.session_key)
        .expect("the evicted session is still a known session");
    assert_eq!(victim.state, "EVICTED");

    // The wire evidence: the victim left through `session/close`, not through a
    // dead process, and only the victim did.
    let lines = await_rpc_line(&log, "close:fake-session-1").await;
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("close:")).count(),
        1,
        "exactly the LRU victim was closed: {lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|line| *line == "spawn").count(),
        1,
        "the process was never restarted, so it really did stay warm: {lines:?}"
    );
}

/// A process is stopped only after its idle window has ACTUALLY elapsed.
///
/// The regression: `stop_idle_processes` used to kill any process whose route
/// table was momentarily empty, on the next 15 s tick, so the plan's "the
/// provider process stays warm" was false and a sweep landing during a slow
/// `session/new` could SIGKILL a healthy adapter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_process_survives_a_sweep_inside_its_idle_window() {
    let (_dir, store, pool) = harness(&[("FAKE_ACP_CHUNKS", "1")]).await;
    let session = seed_session(&store, "acp:warm").await;
    let message_id = seed_message(&store, &session.session_key, "a").await;
    pool.submit_prompt(&session.session_key, &message_id, "a").await;
    await_terminal(&store, &message_id, &session.session_key).await;

    // Tear the SESSION down: the route table is now empty, which is exactly the
    // state that used to kill the process on the next tick.
    assert!(pool.teardown(&session.session_key, ConvergeCause::OperatorStop).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    pool.sweep_once().await;
    pool.sweep_once().await;
    assert_eq!(
        pool.health().await.processes.len(),
        1,
        "a process inside its 10 minute idle window stays warm"
    );
}

/// ... and it IS stopped once the window has passed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zero_session_process_stops_after_its_idle_window() {
    let (_dir, store, pool, _broker) = harness_with_broker(&[("FAKE_ACP_CHUNKS", "1")], |config| {
        config.process_idle_window = Duration::ZERO;
    })
    .await;
    let session = seed_session(&store, "acp:cold").await;
    let message_id = seed_message(&store, &session.session_key, "a").await;
    pool.submit_prompt(&session.session_key, &message_id, "a").await;
    await_terminal(&store, &message_id, &session.session_key).await;
    assert!(pool.teardown(&session.session_key, ConvergeCause::OperatorStop).await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Two sweeps, because the first one is also the one that STAMPS the empty
    // transition: with a non-zero window that stamp is all it does.
    pool.sweep_once().await;
    pool.sweep_once().await;
    assert!(
        pool.health().await.processes.is_empty(),
        "a tenant-free process is stopped once its idle window has elapsed"
    );
}

/// A cancel is per SESSION: convergence is idempotent and leaves the scope
/// reusable without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn convergence_is_idempotent() {
    let (_dir, store, pool) = harness(&[("FAKE_ACP_CHUNKS", "1")]).await;
    let session = seed_session(&store, "acp:converge").await;
    let message_id = seed_message(&store, &session.session_key, "hi").await;
    pool.submit_prompt(&session.session_key, &message_id, "hi").await;
    await_terminal(&store, &message_id, &session.session_key).await;

    let before = transcript(&store, &session.session_key).await.len();
    for _ in 0..2 {
        ainb_hangar_daemon::acp_pool::converge_dirty_session(
            store.pool(),
            &EventBroker::new().sink(),
            &session.session_key,
            ConvergeCause::DaemonRestart,
        )
        .await
        .expect("converge");
    }
    assert_eq!(
        transcript(&store, &session.session_key).await.len(),
        before,
        "convergence on a clean session must write nothing"
    );
}
