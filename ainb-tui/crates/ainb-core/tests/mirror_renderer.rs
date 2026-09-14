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

/// The renderer: a store plus the receiving end of the channel.
struct Renderer {
    store: MirrorStore,
    rx: mpsc::Receiver<FrameBatch>,
}

impl Renderer {
    /// Drain whatever the channel holds right now, as one transaction.
    fn drain(&mut self) -> ainb_app::wire::store::Commit {
        let pending: Vec<FrameBatch> = self.rx.try_iter().collect();
        self.store.apply_drain(pending)
    }
}

fn connect(subscription: Subscription) -> (Mirror, mpsc::Sender<FrameBatch>, Renderer) {
    let (tx, rx) = mpsc::channel();
    (
        Mirror::new(HostId::local(), subscription),
        tx,
        Renderer {
            store: MirrorStore::new(subscription),
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
        vec![SectionId::Sessions, SectionId::Config, SectionId::Shell]
    );
    for id in SUBSET {
        let held = renderer.store.section(id).expect("subscribed section held");
        assert_eq!(held.version, state.versions()[id.index()]);
        assert_eq!(held.host_id, HostId::local());
        assert_eq!(
            held.body,
            section_json(&state, id),
            "the body is the redacted section frame"
        );
    }
    assert!(renderer.store.section(SectionId::Fleet).is_none());
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
    assert_eq!(frame.body["expand_all_workspaces"], false);

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
        let version = |id: SectionId| store.section(id).map_or(0, |section| section.version);
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
    assert_eq!(commit.changed, vec![SectionId::Sessions, SectionId::Shell]);
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
        renderer.store.section(SectionId::Sessions).unwrap().body["expand_all_workspaces"],
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
    let applied = renderer.store.frames_applied();

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
    assert_eq!(renderer.store.frames_applied(), applied);
    assert!(renderer.store.section(SectionId::Sessions).is_none());
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
