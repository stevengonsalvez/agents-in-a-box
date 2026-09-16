// ABOUTME: A headless second renderer for the D15 contract. It subscribes to a
// section subset over a channel, applies each drain as one transaction with
// effects after the commit, and reads root selectors that return only scalars.
//
//   host thread: AppState ──Mirror::batch──▶ mpsc ──▶ renderer: drain ──▶ MirrorStore
//
// The desktop host and the web client replace this renderer; the invariants it
// asserts are the ones they must keep.

use ainb_app::wire::frame::{Frame, FrameBatch, HostId, Mirror, Subscription};
use ainb_app::wire::section_json;
use ainb_app::wire::store::{MirrorStore, ROOT_SELECTORS, Scalar};
use ainb_app::{AppState, SectionId};
use ainb_hangar_proto::agent_status as status;
use ainb_hangar_proto::fleet as proto;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// One scratch `HOME` for the binary, set before any `AppState` reads config.
fn isolated_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
}

/// The renderer: a store plus the receiving end of the channel, and the host
/// the transport says is at the other end of it.
struct Renderer {
    store: MirrorStore,
    peer: HostId,
    rx: mpsc::Receiver<FrameBatch>,
}

impl Renderer {
    /// Drain whatever the channel holds right now, as one transaction.
    fn drain(&mut self) -> ainb_app::wire::store::Commit {
        let pending: Vec<FrameBatch> = self.rx.try_iter().collect();
        self.store.apply_drain(&self.peer, pending)
    }
}

fn connect(subscription: Subscription) -> (Mirror, mpsc::Sender<FrameBatch>, Renderer) {
    let (tx, rx) = mpsc::channel();
    (
        Mirror::new(HostId::local(), subscription),
        tx,
        Renderer {
            store: MirrorStore::new(subscription),
            peer: HostId::local(),
            rx,
        },
    )
}

fn send(mirror: &mut Mirror, tx: &mpsc::Sender<FrameBatch>, state: &AppState) -> Vec<String> {
    let batch = mirror.batch(state);
    let names = batch.frames.iter().map(|frame| frame.section.clone()).collect();
    if !batch.is_empty() {
        tx.send(batch).expect("renderer is listening");
    }
    names
}

const SUBSET: [SectionId; 3] = [SectionId::Sessions, SectionId::Shell, SectionId::Config];

/// `sections` as held from this host, the shape `Commit::changed` reports.
fn local(sections: &[SectionId]) -> Vec<(HostId, SectionId)> {
    sections.iter().map(|id| (HostId::local(), *id)).collect()
}

#[test]
fn the_first_batch_frames_every_subscribed_section_and_nothing_else() {
    isolated_home();
    let state = AppState::new();
    let (mut mirror, tx, mut renderer) = connect(Subscription::only(&SUBSET));

    let names = send(&mut mirror, &tx, &state);
    assert_eq!(names, ["sessions", "config", "shell"]);
    assert_eq!(
        send(&mut mirror, &tx, &state),
        Vec::<String>::new(),
        "nothing changed"
    );

    let commit = renderer.drain();
    assert_eq!(
        commit.changed,
        local(&[SectionId::Sessions, SectionId::Config, SectionId::Shell])
    );
    for id in SUBSET {
        let held = renderer.store.section(&HostId::local(), id).expect("subscribed section held");
        assert_eq!(held.version, state.versions()[id.index()]);
        assert_eq!(held.host_id, HostId::local());
        assert_eq!(
            held.body,
            section_json(&state, id),
            "the body is the redacted section frame"
        );
    }
    assert!(renderer.store.section(&HostId::local(), SectionId::Fleet).is_none());
}

#[test]
fn frames_name_only_the_sections_that_changed() {
    isolated_home();
    let mut state = AppState::new();
    let (mut mirror, tx, mut renderer) = connect(Subscription::only(&SUBSET));
    send(&mut mirror, &tx, &state);
    renderer.drain();

    state.sessions.get_mut().expand_all_workspaces = false;
    let batch = mirror.batch(&state);
    let frame: &Frame = match batch.frames.as_slice() {
        [frame] => frame,
        other => panic!("one changed section, got {other:?}"),
    };
    assert_eq!(frame.section_id(), Some(SectionId::Sessions));
    assert_eq!(frame.version, state.versions()[SectionId::Sessions.index()]);
    assert_eq!(frame.body()["expand_all_workspaces"], false);

    // A change in a section nobody subscribed to frames nothing.
    state.fleet.get_mut().attention_elsewhere = 3;
    assert!(mirror.batch(&state).is_empty());
}

#[test]
fn one_drain_is_one_transaction_and_effects_run_after_the_commit() {
    isolated_home();
    let mut state = AppState::new();
    let (mut mirror, tx, mut renderer) = connect(Subscription::only(&SUBSET));
    send(&mut mirror, &tx, &state);
    renderer.drain();
    let before = renderer.store.transactions();

    // What each effect saw, captured when it ran.
    let seen: Arc<Mutex<Vec<(u64, u64, u64)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    renderer.store.on_commit(move |store, commit| {
        let version = |id: SectionId| {
            store.section(&HostId::local(), id).map_or(0, |section| section.version)
        };
        sink.lock().unwrap().push((
            commit.transaction,
            version(SectionId::Sessions),
            version(SectionId::Shell),
        ));
    });

    // Three host ticks land between two renderer drains.
    state.sessions.get_mut().expand_all_workspaces = false;
    send(&mut mirror, &tx, &state);
    state.add_info_notification("first".to_string());
    send(&mut mirror, &tx, &state);
    state.sessions.get_mut().expand_all_workspaces = true;
    send(&mut mirror, &tx, &state);

    let commit = renderer.drain();
    assert_eq!(
        renderer.store.transactions(),
        before + 1,
        "three batches, one transaction"
    );
    assert_eq!(
        commit.changed,
        local(&[SectionId::Sessions, SectionId::Shell])
    );
    let effects_seen = seen.lock().unwrap().clone();
    assert_eq!(
        effects_seen.as_slice(),
        [(
            before + 1,
            state.versions()[SectionId::Sessions.index()],
            state.versions()[SectionId::Shell.index()],
        )],
        "the effect ran once, after both sections were committed at their final versions"
    );
    assert_eq!(
        renderer.store.section(&HostId::local(), SectionId::Sessions).unwrap().body["expand_all_workspaces"],
        true,
        "the last frame of a section in a drain wins"
    );

    // An empty drain commits nothing and runs no effect.
    assert!(renderer.drain().changed.is_empty());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn an_unsubscribed_hot_section_applies_no_frame() {
    isolated_home();
    let mut state = AppState::new();
    let cold = [SectionId::Shell, SectionId::Config];
    let (mut mirror, tx, mut renderer) = connect(Subscription::only(&cold));
    let (mut hot_mirror, _hot_tx, _) = connect(Subscription::only(&[SectionId::Sessions]));
    send(&mut mirror, &tx, &state);
    renderer.drain();
    let applied = renderer.store.sections_committed();

    for round in 0..50 {
        state.sessions.get_mut().selected_session_index = Some(round);
        assert!(
            send(&mut mirror, &tx, &state).is_empty(),
            "the hot section is never framed"
        );
    }
    // Even a frame for it that reaches the renderer (another host, a stale
    // subscription) is dropped, not applied.
    tx.send(hot_mirror.batch(&state)).unwrap();
    let commit = renderer.drain();
    assert!(commit.changed.is_empty());
    assert_eq!(renderer.store.sections_committed(), applied);
    assert!(renderer.store.section(&HostId::local(), SectionId::Sessions).is_none());
    assert_eq!(renderer.store.frames_ignored(), 1);

    // Subscribing later frames it in full on the next batch.
    mirror.resubscribe(Subscription::only(&[
        SectionId::Sessions,
        SectionId::Shell,
        SectionId::Config,
    ]));
    assert_eq!(send(&mut mirror, &tx, &state), ["sessions"]);
}

#[test]
fn every_root_selector_returns_a_scalar() {
    isolated_home();
    let mut state = AppState::new();
    let (mut mirror, tx, mut renderer) = connect(Subscription::all());
    send(&mut mirror, &tx, &state);
    renderer.drain();

    let before = renderer.store.read_selectors();
    assert_eq!(before.len(), ROOT_SELECTORS.len());
    for (name, value) in &before {
        let json = serde_json::to_value(value).expect("a scalar serialises");
        assert!(
            !json.is_array() && !json.is_object(),
            "root selector {name} returned a list or an object: {json}"
        );
        assert_ne!(
            *value,
            Scalar::Absent,
            "{name} reads a section the renderer holds"
        );
    }

    state.add_info_notification("one".to_string());
    send(&mut mirror, &tx, &state);
    renderer.drain();
    let count = |values: &[(&str, Scalar)]| {
        values
            .iter()
            .find(|(name, _)| *name == "notification_count")
            .map(|(_, v)| v.clone())
    };
    assert_eq!(count(&before), Some(Scalar::Count(0)));
    assert_eq!(
        count(&renderer.store.read_selectors()),
        Some(Scalar::Count(1))
    );
}

/// One roster row as a daemon on `host` reports it.
fn roster_read(
    host: &str,
    read_at_ms: i64,
    evidence_observed_at: i64,
) -> status::RosterStatusResult {
    let session = proto::FleetSession {
        session_key: "claude:shared-key".to_string(),
        provider: proto::FleetProvider::Claude,
        provider_session_id: Some("s-1".to_string()),
        tmux_target: Some("ainb-a".to_string()),
        pane_binding: proto::PaneBinding::default(),
        process_start_fingerprint: None,
        cwd: format!("/home/{host}/repo"),
        display_name: Some(format!("{host} session")),
        lifecycle: proto::LifecycleState::Running,
        active_work_count: 1,
        attention: proto::AttentionState::default(),
        current_request_fingerprint: None,
        current_request: None,
        management: proto::ManagementState::Managed,
        transport_health: proto::TransportHealth::Healthy,
        capabilities: proto::FleetCapabilities::default(),
        provenance: proto::FleetProvenance::Authoritative,
        confidence: proto::FleetConfidence::High,
        discovered_at: 1,
        last_observed_at: 2,
        lifecycle_updated_at: 2,
        attention_updated_at: 1,
        model: None,
        reasoning_effort: None,
        model_updated_at: 0,
        version: 1,
        updated_revision: 3,
    };
    let row = status::AgentStatusRow {
        host_id: host.to_string(),
        evidence_observed_at,
        ..status::status_row(&session, false)
    };
    status::RosterStatusResult {
        rows: vec![status::RosterStatusRow {
            session,
            status: row,
            read_revision: 7,
        }],
        read_revision: 7,
        unknown_events: Vec::new(),
        read_at_ms,
    }
}

/// Two machines mirror the same session key into one renderer over their own
/// channels: both are held, each with its own host id and version, and the
/// folded count is two.
#[test]
fn two_hosts_fold_into_one_renderer_without_collision() {
    isolated_home();
    let subscription = Subscription::only(&[SectionId::Fleet, SectionId::AgentStatus]);
    let mut store = MirrorStore::new(subscription);
    let (h1, h2) = (HostId::new("h1"), HostId::new("h2"));
    for host in [&h1, &h2] {
        let mut state = AppState::new();
        let read = roster_read(host.as_str(), 10_000, 9_000);
        *state.fleet.get_mut().fleet_snapshot.lock().unwrap() = vec![read.rows[0].session.clone()];
        state.apply_agent_status_read(read, 10_000);
        let batch = Mirror::new(host.clone(), subscription).batch(&state);
        let commit = store.apply_drain(host, [batch]);
        assert_eq!(
            commit.changed,
            vec![
                (host.clone(), SectionId::Fleet),
                (host.clone(), SectionId::AgentStatus)
            ]
        );
    }
    let renderer = Renderer {
        store,
        peer: h1.clone(),
        rx: mpsc::channel().1,
    };
    for host in [&h1, &h2] {
        let card =
            &renderer.store.section(host, SectionId::AgentStatus).unwrap().body["view"]["cards"][0];
        assert_eq!(
            card["host_id"],
            host.as_str(),
            "a row names the host it came from"
        );
        assert_eq!(card["session_key"], "claude:shared-key");
        // The fleet row and the card name the same host: the frame's, not a
        // process-wide one (#1066).
        let row =
            &renderer.store.section(host, SectionId::Fleet).unwrap().body["fleet_snapshot"][0];
        assert_eq!(row["host_id"], card["host_id"], "row and card agree");
    }
    let selectors = renderer.store.read_selectors();
    let read = |name: &str| selectors.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
    assert_eq!(read("agent_count"), Some(Scalar::Count(2)));
    assert_eq!(read("host_count"), Some(Scalar::Count(2)));
}

/// A daemon 90 s ahead: the section 20 frame carries the daemon's clock at the
/// read, and a card's age is that clock minus its evidence stamp, 5 s. This
/// surface's own clock minus the stamp is negative, the bug the frame avoids.
#[test]
fn a_card_ages_on_the_daemon_clock_across_a_90_second_skew() {
    const SKEW_MS: i64 = 90_000;
    isolated_home();
    let local_now = 1_000_000;
    let daemon_read_at = local_now + SKEW_MS;
    let evidence = daemon_read_at - 5_000;
    let mut state = AppState::new();
    state.apply_agent_status_read(roster_read("h1", daemon_read_at, evidence), local_now);
    let subscription = Subscription::only(&[SectionId::AgentStatus]);
    let batch = Mirror::new(HostId::new("h1"), subscription).batch(&state);
    let frame = &batch.frames[0];
    let read = frame.daemon_read.expect("section 20 names its daemon read");
    assert_eq!(read.revision, 7);
    assert_eq!(read.clock_ms, daemon_read_at);
    let observed = frame.body()["view"]["cards"][0]["evidence_observed_at"].as_i64().unwrap();
    assert_eq!(read.clock_ms - observed, 5_000, "the age a renderer draws");
    assert!(
        local_now - observed < 0,
        "local now minus a remote stamp goes negative"
    );
}

/// The Fleet section's rows are stamped on the daemon's clock too. A daemon
/// 90 s ahead: the Fleet frame names that clock, so a row observed 5 s before
/// the read ages 5 s, where this surface's own now would make it negative.
#[test]
fn a_fleet_row_ages_on_the_daemon_clock_across_a_90_second_skew() {
    const SKEW_MS: i64 = 90_000;
    isolated_home();
    let local_now = 1_000_000;
    let daemon_read_at = local_now + SKEW_MS;
    let observed = daemon_read_at - 5_000;
    let mut state = AppState::new();
    state.apply_agent_status_read(roster_read("h1", daemon_read_at, observed), local_now);
    let read = roster_read("h1", daemon_read_at, observed);
    let session = proto::FleetSession {
        attention_updated_at: observed,
        ..read.rows[0].session.clone()
    };
    *state.fleet.get_mut().fleet_snapshot.lock().unwrap() = vec![session];

    let subscription = Subscription::only(&[SectionId::Fleet]);
    let batch = Mirror::new(HostId::new("h1"), subscription).batch(&state);
    let frame = &batch.frames[0];
    let clock = frame.daemon_read.expect("the Fleet frame names the daemon clock").clock_ms;
    let stamp = frame.body()["fleet_snapshot"][0]["attention_updated_at"].as_i64().unwrap();
    assert_eq!(clock - stamp, 5_000, "the age a renderer draws");
    assert!(
        local_now - stamp < 0,
        "local now minus a remote stamp goes negative"
    );
}
