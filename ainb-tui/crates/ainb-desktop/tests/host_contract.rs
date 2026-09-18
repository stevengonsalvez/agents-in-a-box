//! The desktop host's contract with the state machine, headless: what it frames,
//! when effects run, and that its config is the one it was given.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::app::Effect;
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, CommandId, Intent, Keymap, SectionId};
use ainb_desktop::host::{DesktopHost, Executor};

mod support;

use support::isolated_home as scratch_home;

type Log = Rc<RefCell<Vec<String>>>;

fn host(sections: &[SectionId], log: &Log) -> DesktopHost<impl FnMut(FrameBatch)> {
    host_on(AppConfig::default(), sections, log)
}

fn host_on(
    config: AppConfig,
    sections: &[SectionId],
    log: &Log,
) -> DesktopHost<impl FnMut(FrameBatch)> {
    scratch_home();
    let log = Rc::clone(log);
    DesktopHost::new(
        config,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(sections),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                log.borrow_mut().push(format!("frame {}", frame.section));
            }
        },
    )
}

fn key(spelling: &str) -> Intent {
    Intent::Key(Chord::parse(spelling).expect("valid chord"))
}

/// Records each effect it is handed, in the shared log, and runs none.
struct Recorder(Log);

impl Executor for Recorder {
    fn execute(&mut self, effect: Effect) -> Vec<Intent> {
        let name = match effect {
            Effect::Persist(store) => format!("effect persist {}", store.store_id()),
            other => format!("effect {other:?}"),
        };
        self.0.borrow_mut().push(name);
        Vec::new()
    }
}

#[test]
fn a_host_that_never_draws_still_lands_an_answer_outcome() {
    // The send worker reports into the state. This shell has none of the
    // terminal's draw loop, so unless its tick folds the report, an answer sent
    // from this window leaves the row reading SENT for as long as it is open.
    use ainb_app::fleet::answer::{AnswerPhase, request_id};
    use ainb_app::fleet::attention::{AttentionKind, SessionAttention};

    let log = Log::default();
    let mut host = host(&[SectionId::Shell], &log);
    let _ = host.tick();
    let chip = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-1".into());

    // Exactly what a worker does when the daemon has answered.
    host.state().fleet.ask_state.reports().lock().expect("inbox").push((
        request_id(&chip),
        AnswerPhase::Delivered {
            via: "tmux (feat-login)".to_string(),
        },
    ));
    let _ = host.tick();

    assert!(
        matches!(
            host.state().fleet.ask_state.phase_for(&chip),
            Some(AnswerPhase::Delivered { .. })
        ),
        "the desktop's own tick folded the outcome"
    );
}

#[test]
fn an_answer_from_this_window_is_recorded_as_the_desktops() {
    // The daemon stamps `answered_by` from the kind the connection declares,
    // and the answer path dials with the kind the state carries. A shell that
    // left it at the default would record every answer as the TUI's, and the
    // concurrency gate would read a surface nobody sat at.
    let log = Log::default();
    let host = host(&[SectionId::Shell], &log);

    assert_eq!(
        host.state().host.surface,
        ainb_hangar_proto::connections::SurfaceKind::Desktop
    );
}

#[test]
fn the_first_batch_frames_every_subscribed_section_and_nothing_else() {
    let log = Log::default();
    let mut host = host(&[SectionId::Sessions, SectionId::Shell], &log);

    assert!(host.tick().is_empty());

    let mut framed = log.borrow().clone();
    framed.sort();
    assert_eq!(framed, vec!["frame sessions", "frame shell"]);
}

/// Daemon news makes the tick merge attention: a blocking daemon row no session
/// claims is counted elsewhere, and the merge frames neither Sessions nor Shell.
/// A tick right after, with no news, does not merge again inside the throttle.
#[test]
fn daemon_news_merges_attention_on_the_tick_and_frames_only_what_moved() {
    use ainb_app::fleet::attention::{AttentionKind, DaemonAttention, SessionAttention};
    use std::sync::atomic::Ordering;

    let log = Log::default();
    let mut host = host(&[SectionId::Sessions, SectionId::Shell], &log);
    // Held off, so no poller thread overwrites the rows this test installs.
    host.state().host.attention_poll_running.store(true, Ordering::Release);
    let _ = host.tick();
    log.borrow_mut().clear();

    let row = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-unclaimed".into());
    *host.state().fleet.daemon_attention.lock().unwrap() = DaemonAttention::up(
        std::collections::HashMap::from([("/nowhere".to_string(), vec![row])]),
    );
    host.state().host.daemon_attention_generation.fetch_add(1, Ordering::Release);
    let _ = host.tick();

    assert_eq!(
        host.state().fleet.attention_elsewhere,
        1,
        "the tick merged the daemon row"
    );
    let framed = log.borrow().clone();
    assert!(
        !framed.iter().any(|frame| frame == "frame sessions"),
        "no session row moved: {framed:?}"
    );
    // Shell moves once and once only: a merge that changed something sets the
    // refresh latch the terminal host reads, and nothing here clears it.
    assert_eq!(
        framed.iter().filter(|frame| *frame == "frame shell").count(),
        1,
        "{framed:?}"
    );

    // No news: the row goes, but the next merge is not due yet.
    *host.state().fleet.daemon_attention.lock().unwrap() =
        DaemonAttention::up(std::collections::HashMap::new());
    let _ = host.tick();
    assert_eq!(
        host.state().fleet.attention_elsewhere,
        1,
        "no merge inside the throttle"
    );
}

#[test]
fn a_section_that_did_not_move_frames_nothing() {
    let log = Log::default();
    let mut host = host(&[SectionId::Sessions, SectionId::Shell], &log);
    let _ = host.tick();
    log.borrow_mut().clear();

    let _ = host.tick();
    assert!(log.borrow().is_empty(), "nothing moved: {:?}", log.borrow());

    // Opening the sessions screen moves the shell section only.
    let _ = host.dispatch(key("s"));
    assert_eq!(*log.borrow(), vec!["frame shell"]);
}

#[test]
fn effects_come_back_from_dispatch_and_run_after_the_state_write() {
    let log = Log::default();
    let mut host = host(&[SectionId::Config], &log);
    let _ = host.tick();
    let shown = host.state().config.app_config.ui_preferences.show_session_menu_bar;
    let mut recorder = Recorder(Rc::clone(&log));
    host.run(key("s"), &mut recorder);
    log.borrow_mut().clear();

    host.run(key("M"), &mut recorder);

    assert_ne!(
        host.state().config.app_config.ui_preferences.show_session_menu_bar,
        shown,
        "the toggle was applied"
    );
    assert_eq!(
        *log.borrow(),
        vec!["frame config", "effect persist config"],
        "the config write was framed before its persistence effect ran"
    );
}

/// A renderer that attaches late gets every subscribed section again.
#[test]
fn reframe_sends_every_subscribed_section_again() {
    let log = Log::default();
    let mut host = host(&[SectionId::Sessions, SectionId::Shell], &log);
    let _ = host.tick();
    log.borrow_mut().clear();

    host.reframe();

    let mut framed = log.borrow().clone();
    framed.sort();
    assert_eq!(framed, vec!["frame sessions", "frame shell"]);
}

/// A renderer that attaches names its sections and gets exactly those, once.
#[test]
fn subscribe_frames_exactly_the_named_sections_in_one_batch() {
    let log = Log::default();
    let mut host = host(&[], &log);
    let _ = host.tick();
    assert!(
        log.borrow().is_empty(),
        "nothing is framed before a renderer subscribes"
    );

    host.subscribe(Subscription::only(&[SectionId::Sessions, SectionId::Fleet]));

    let mut framed = log.borrow().clone();
    framed.sort();
    assert_eq!(framed, vec!["frame fleet", "frame sessions"]);
}

/// #1066: re-pinning the host frames every subscribed section again, static
/// ones included, under the new id; re-pinning to the same id sends nothing.
#[test]
fn set_host_reframes_every_subscribed_section_under_the_new_id() {
    scratch_home();
    let hosts = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let seen = Rc::clone(&hosts);
    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::none(),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                seen.borrow_mut().push((frame.section, frame.host_id.as_str().to_string()));
            }
        },
    );
    host.subscribe(Subscription::only(&[
        SectionId::Config,
        SectionId::Sessions,
    ]));
    assert!(hosts.borrow().iter().all(|(_, id)| id == "local"));
    hosts.borrow_mut().clear();

    assert!(!host.set_host(HostId::local()));
    assert!(hosts.borrow().is_empty(), "the same id frames nothing");

    let ulid = HostId::new("01K5A0000000000000000AAAAA");
    assert!(host.set_host(ulid.clone()));
    assert_eq!(host.host_id(), &ulid);
    let mut framed = hosts.borrow().clone();
    framed.sort();
    assert_eq!(
        framed,
        vec![
            ("config".to_string(), ulid.as_str().to_string()),
            ("sessions".to_string(), ulid.as_str().to_string()),
        ]
    );
}

/// The desktop's sidebar is the session list, so the host moves the reducer
/// there through the home sidebar's own rows, and the list's row click is then
/// in context.
#[test]
fn open_sessions_moves_the_reducer_to_the_session_list_through_its_rows() {
    let log = Log::default();
    let mut host = host(&[SectionId::Shell], &log);
    assert_eq!(host.state().shell.current_screen, "home");
    assert!(
        !ainb_app::app::keymap::command_contexts(host.state())
            .iter()
            .any(|context| context.name() == "session_list")
    );

    host.open_sessions(&mut Recorder(Rc::clone(&log)));

    assert_eq!(host.state().shell.current_screen, "session_list");
    assert!(
        ainb_app::app::keymap::command_contexts(host.state())
            .iter()
            .any(|context| context.name() == "session_list")
    );
}

/// The palette and the dispatch seam share one refusal set, so the palette
/// cannot offer a row the seam would refuse.
#[test]
fn every_palette_entry_passes_the_seam_and_no_refused_row_is_offered() {
    use ainb_desktop::intent::{RendererIntent, refused_from_webview};

    let log = Log::default();
    let host = host(&[SectionId::Shell], &log);
    let keymap = Keymap::defaults();
    let palette = host.palette();
    assert!(!palette.is_empty(), "the keymap has commands to offer");

    for entry in &palette {
        assert!(
            !refused_from_webview(&keymap, &entry.id),
            "the palette offers a refused row: {}",
            entry.id.as_str()
        );
        let intent = RendererIntent::Command(entry.id.clone(), serde_json::Value::Null);
        assert!(
            Intent::try_from(intent).is_ok(),
            "the seam refuses a palette row: {}",
            entry.id.as_str()
        );
    }

    let offered: Vec<&str> = palette.iter().map(|entry| entry.id.as_str()).collect();
    let key_only: Vec<CommandId> =
        keymap.commands().filter(|(_, row)| row.key_only()).map(|(id, _)| id).collect();
    assert!(!key_only.is_empty(), "the keymap has key-only rows");
    for refused in ainb_app::app::reports::ids::ALL
        .iter()
        .chain(ainb_app::app::plugin_action::ids::ALL)
        .copied()
        .chain(key_only.iter().map(CommandId::as_str))
    {
        assert!(
            !offered.contains(&refused),
            "the palette offers `{refused}`"
        );
    }

    // A palette names a row with no payload, so a pointer row that refuses
    // `Args::Null` has nothing to run with and is not offered; one that runs
    // without a payload is an ordinary row and is.
    for pointer in ainb_app::app::pointer::ids::ALL {
        let Some(row) = keymap.command(&CommandId::new(*pointer)) else {
            continue;
        };
        if row.action.with_args(&serde_json::Value::Null).is_none() {
            assert!(
                !offered.contains(pointer),
                "the palette offers `{pointer}`, which needs a payload"
            );
        }
    }

    // A row that is not active is still offered, so the list does not shift
    // under the user; `global.go_home` is bound and always active.
    assert!(palette.iter().any(|entry| entry.active), "{offered:?}");
    assert!(
        palette.iter().any(|entry| entry.chord.is_some()),
        "{offered:?}"
    );
}

/// A key-only row writes outside ainb, so a chord or a name that lands on one
/// is refused for the webview, with the row and the reason; any other is not.
#[test]
fn a_chord_or_a_name_on_a_key_only_row_is_refused() {
    let log = Log::default();
    let host = host(&[SectionId::Shell], &log);

    let refusal = host.refused_from_renderer(&key("W")).expect("W is key-only");
    assert_eq!(refusal.command.as_str(), "global.wire_statusline");
    assert!(refusal.reason.contains("only from its key"), "{refusal:?}");
    let named = Intent::Command(refusal.command.clone(), serde_json::Value::Null);
    assert_eq!(host.refused_from_renderer(&named), Some(refusal));
    assert_eq!(host.refused_from_renderer(&key("s")), None);
}

/// Enter confirms whatever the open dialog holds, so it is judged by that
/// action: on the abtop setup offer, whose selected "Enable" edits Claude
/// Code's settings, the webview's Enter is refused.
#[test]
fn enter_on_a_dialog_holding_a_key_only_action_is_refused() {
    let log = Log::default();
    let mut host = host(&[SectionId::Shell], &log);
    assert_eq!(host.refused_from_renderer(&key("enter")), None);

    let _ = host.dispatch(key("t"));
    let dialog = host
        .state()
        .shell
        .confirmation_dialog
        .as_ref()
        .expect("the first abtop open offers the setup");
    assert!(matches!(
        dialog.selected_action(),
        Some(ainb_app::app::state::ConfirmAction::SetupAbtopRateLimits)
    ));

    let refusal = host
        .refused_from_renderer(&key("enter"))
        .expect("Enter would run the abtop setup");
    assert!(
        refusal.command.as_str().ends_with(".confirm"),
        "{refusal:?}"
    );
    assert!(
        refusal.reason.contains("runs only from its key"),
        "{refusal:?}"
    );
}

#[test]
fn a_host_built_on_an_injected_config_touches_no_file_under_home() {
    let home = scratch_home();
    let log = Log::default();
    let mut config = AppConfig::default();
    config.ui_preferences.show_session_menu_bar = !config.ui_preferences.show_session_menu_bar;
    let expected = config.ui_preferences.show_session_menu_bar;

    let mut host = host_on(config, &[SectionId::Config], &log);
    let _ = host.tick();

    assert_eq!(
        host.state().config.app_config.ui_preferences.show_session_menu_bar,
        expected,
        "the host runs on the config it was given"
    );
    let written: Vec<_> = walk(home);
    assert!(written.is_empty(), "files appeared under HOME: {written:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// The answer's full round trip with no window: the ask commands a person's
/// clicks send, against a session waiting on a daemon question, move the
/// frame's phase to in flight and then settle it. There is no daemon in this
/// test, so it settles as a failure that names the call and keeps the answer;
/// the delivered leg runs against a real daemon in the journey.
#[test]
fn the_ask_commands_send_an_answer_and_the_frame_follows_it() {
    use ainb_app::AppState;
    use ainb_app::app::pointer::select_session_tab;
    use ainb_app::app::screens::ids as screen_ids;
    use ainb_app::components::session_tabs::SessionTab;
    use ainb_app::fleet::answer::AnswerPhase;
    use ainb_app::fleet::attention::{AttentionKind, AttentionOption, SessionAttention};
    use ainb_app::models::{Session, Workspace};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    scratch_home();
    let chip = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-7".into()).with_options(
        ["staging", "production"]
            .iter()
            .map(|label| AttentionOption {
                label: (*label).to_string(),
                description: String::new(),
            })
            .collect(),
    );
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut workspace = Workspace::new("api".to_string(), "/work/api".into());
    let mut session = Session::new("feat".to_string(), "/work/api/wt".to_string());
    session.live_attention = vec![chip.clone()];
    let session_id = session.id;
    workspace.add_session(session);
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    // The chip is the seed; a merge would recompute it from stores this test
    // does not have, so none is due while the test runs.
    state.host.last_attention_refresh = Some(Instant::now());
    let log = Log::default();
    let sink_log = Rc::clone(&log);
    let mut host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Fleet]),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                sink_log.borrow_mut().push(format!("frame {}", frame.section));
            }
        },
    );
    host.state().host.attention_poll_running.store(true, Ordering::Release);

    // What the banner sends to pick the second option, in its order: the row
    // selected without attaching it, the ask pane shown, the cursor moved,
    // then Enter.
    let _ = host.dispatch(ainb_app::app::pointer::select_session_row(
        &ainb_app::app::state::SessionListRowId::Session(session_id),
        false,
    ));
    let _ = host.dispatch(select_session_tab(SessionTab::Ask));
    let _ = host.tick();
    let _ = host.dispatch(Intent::Command(
        CommandId::new("session_list.ask.next"),
        serde_json::Value::Null,
    ));
    assert_eq!(
        host.state().fleet.ask_state.cursor(),
        1,
        "the cursor is on option two"
    );
    log.borrow_mut().clear();
    let _ = host.dispatch(Intent::Command(
        CommandId::new("session_list.ask.enter"),
        serde_json::Value::Null,
    ));
    assert!(
        matches!(
            host.state().fleet.ask_state.phase_for(&chip),
            Some(AnswerPhase::InFlight { .. })
        ),
        "the send is out"
    );
    assert!(
        log.borrow().iter().any(|frame| frame == "frame fleet"),
        "and the frame says so"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state().fleet.ask_state.in_flight() {
        assert!(Instant::now() < deadline, "the send never settled");
        std::thread::sleep(Duration::from_millis(20));
        let _ = host.tick();
    }
    match host.state().fleet.ask_state.phase_for(&chip) {
        Some(AnswerPhase::Failed { reason, .. }) => {
            assert!(
                reason.contains("attention/answer"),
                "names the call: {reason}"
            );
        }
        other => panic!("with no daemon the send fails, and says so: {other:?}"),
    }
}

/// Typed text reaches the reducer's composer once its cursor is on the
/// composer row, which is what the banner's free-text send relies on.
#[test]
fn text_typed_at_the_composer_row_lands_in_the_reducers_composer() {
    use ainb_app::AppState;
    use ainb_app::app::pointer::select_session_tab;
    use ainb_app::app::screens::ids as screen_ids;
    use ainb_app::components::session_tabs::SessionTab;
    use ainb_app::fleet::attention::{AttentionKind, SessionAttention};
    use ainb_app::models::{Session, Workspace};
    use std::time::Instant;

    scratch_home();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut workspace = Workspace::new("api".to_string(), "/work/api".into());
    let mut session = Session::new("feat".to_string(), "/work/api/wt".to_string());
    session.live_attention = vec![SessionAttention::daemon(
        AttentionKind::Ask,
        1,
        "att-9".into(),
    )];
    workspace.add_session(session);
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state.host.last_attention_refresh = Some(Instant::now());
    let mut host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Fleet]),
        |_batch: FrameBatch| {},
    );

    let _ = host.dispatch(select_session_tab(SessionTab::Ask));
    let _ = host.tick();
    // No options, so the retarget has already put the cursor on the composer.
    let _ = host.dispatch(Intent::Text("qa".to_string()));

    assert_eq!(host.state().fleet.ask_state.free_text(), "qa");
}

/// Seam 4 reaches the desktop: the host's own tick opens the conversation the
/// open tab names and frames it. The terminal used to open and tick chat hosts
/// only while drawing, so on this shell `fleet.conversation` stayed the default
/// forever.
#[test]
fn a_desktop_tick_frames_the_open_conversation() {
    use ainb_app::app::pointer::select_session_tab;
    use ainb_app::components::session_tabs::SessionTab;
    use ainb_app::fleet::conversation::{Conversation, ConversationTopic};

    let log = Log::default();
    let mut host = host(&[SectionId::Fleet], &log);
    let mut recorder = Recorder(Rc::clone(&log));
    host.open_sessions(&mut recorder);
    let _ = host.dispatch(select_session_tab(SessionTab::Pal));
    log.borrow_mut().clear();

    let _ = host.tick();

    let conversation = &host.state().fleet.conversation;
    assert_ne!(
        *conversation,
        Conversation::default(),
        "the tick projected it"
    );
    assert_eq!(conversation.topic, ConversationTopic::Pal);
    assert!(
        log.borrow().iter().any(|frame| frame == "frame fleet"),
        "and framed it: {:?}",
        log.borrow()
    );
}

/// A status read holding one ACP card, `acp:s-1`.
fn acp_roster() -> ainb_hangar_proto::agent_status::RosterStatusResult {
    use ainb_hangar_proto::agent_status as status;
    use ainb_hangar_proto::fleet;
    let session = fleet::FleetSession {
        session_key: "acp:s-1".to_string(),
        provider: fleet::FleetProvider::Acp,
        provider_session_id: None,
        tmux_target: None,
        pane_binding: fleet::PaneBinding::PaneUnbound,
        process_start_fingerprint: None,
        cwd: "/w".to_string(),
        display_name: None,
        lifecycle: fleet::LifecycleState::Running,
        active_work_count: 0,
        attention: fleet::AttentionState::None,
        current_request_fingerprint: None,
        current_request: None,
        management: fleet::ManagementState::Managed,
        transport_health: fleet::TransportHealth::Healthy,
        capabilities: fleet::FleetCapabilities::default(),
        provenance: fleet::FleetProvenance::Authoritative,
        confidence: fleet::FleetConfidence::High,
        discovered_at: 1,
        last_observed_at: 1,
        lifecycle_updated_at: 1,
        attention_updated_at: 1,
        model: None,
        reasoning_effort: None,
        model_updated_at: 0,
        version: 1,
        updated_revision: 1,
    };
    let row = status::status_row_with_tier(&session, false, None);
    status::RosterStatusResult {
        rows: vec![status::RosterStatusRow {
            session,
            status: row,
            read_revision: 1,
        }],
        read_revision: 1,
        unknown_events: Vec::new(),
        read_at_ms: 0,
    }
}

/// An ACP session's transcript: opened from the board by its Fleet session
/// key, paged by the host's own tick, framed on Fleet.
mod transcript {
    use super::*;
    use ainb_app::app::pointer::open_transcript;
    use ainb_app::fleet::transcript::{ChunkKind, MAX_CHUNKS, Transcript, TranscriptOutcome};
    use ainb_hangar_proto::fleet::{FleetTranscriptChunk, FleetTranscriptListResult};

    /// A state whose status read holds the ACP card `acp:s-1`: the host opens
    /// a transcript only for a card it holds.
    fn holding_acp_card() -> ainb_app::AppState {
        let mut state = ainb_app::AppState::with_config(AppConfig::default());
        state.apply_agent_status_read(acp_roster(), 1);
        state
    }

    /// A host on the sessions screen whose sink records each Fleet frame's
    /// encoded size.
    fn sized_host(sizes: &Rc<RefCell<Vec<usize>>>) -> DesktopHost<impl FnMut(FrameBatch)> {
        scratch_home();
        let sizes = Rc::clone(sizes);
        let mut host = DesktopHost::hosting(
            holding_acp_card(),
            Keymap::defaults(),
            HostId::local(),
            Subscription::only(&[SectionId::Fleet]),
            move |batch: FrameBatch| {
                for frame in batch.frames {
                    sizes.borrow_mut().push(serde_json::to_vec(&frame).expect("encodes").len());
                }
            },
        )
        // Folding a large page takes a while in a debug build; no rescan may
        // come due inside it, since a scan needs the runtime this test lacks.
        .rescanning_every(std::time::Duration::from_secs(600));
        host.open_sessions(&mut Recorder(Log::default()));
        host
    }

    fn page(count: i64, text: &str) -> TranscriptOutcome {
        let chunks: Vec<FleetTranscriptChunk> = (1..=count)
            .map(|order| FleetTranscriptChunk {
                ingest_order: order,
                event_id: format!("e-{order}"),
                session_key: "acp:s-1".to_string(),
                event_type: if order % 2 == 0 {
                    "acp.thought"
                } else {
                    "acp.message"
                }
                .to_string(),
                payload: serde_json::json!({ "text": text }),
                observed_at: order,
            })
            .collect();
        TranscriptOutcome::Page(FleetTranscriptListResult {
            next_after_order: chunks.last().map(|chunk| chunk.ingest_order),
            chunks,
            truncated: false,
        })
    }

    /// Stand in for the page worker, as a real one reports.
    fn deliver(host: &DesktopHost<impl FnMut(FrameBatch)>, outcome: TranscriptOutcome) {
        let open = host.state().host.transcript.as_ref().expect("a transcript is open");
        open.reports().lock().expect("inbox").insert(0, outcome);
    }

    #[test]
    fn a_tick_frames_the_open_transcript() {
        let sizes = Rc::new(RefCell::new(Vec::new()));
        let mut host = sized_host(&sizes);
        let _ = host.dispatch(open_transcript(Some("acp:s-1")));
        deliver(&host, page(2, "hello"));

        let _ = host.tick();

        let framed = &host.state().fleet.transcript;
        assert_eq!(framed.session_key.as_deref(), Some("acp:s-1"));
        assert_eq!(
            framed.chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
            vec![ChunkKind::Message, ChunkKind::Thought]
        );
        assert!(!sizes.borrow().is_empty(), "and the Fleet frame went out");
    }

    #[test]
    fn a_transcript_past_the_bound_still_frames_inside_one_frame() {
        let sizes = Rc::new(RefCell::new(Vec::new()));
        let mut host = sized_host(&sizes);
        let _ = host.dispatch(open_transcript(Some("acp:s-1")));
        // Far more than the host keeps, each far longer than a row survives.
        deliver(&host, page(600, &"x".repeat(20_000)));
        sizes.borrow_mut().clear();

        let _ = host.tick();

        let framed = &host.state().fleet.transcript;
        assert_eq!(framed.chunks.len(), MAX_CHUNKS);
        assert!(framed.starts_part_way);
        let largest = sizes.borrow().iter().copied().max().expect("a frame went out");
        assert!(
            largest < ainb_app::wire::frame::MAX_FRAME_BYTES,
            "a Fleet frame of {largest} bytes"
        );
    }

    #[test]
    fn a_closed_transcript_frames_nothing() {
        let sizes = Rc::new(RefCell::new(Vec::new()));
        let mut host = sized_host(&sizes);
        let _ = host.dispatch(open_transcript(Some("acp:s-1")));
        deliver(&host, page(2, "hello"));
        let _ = host.tick();

        let _ = host.dispatch(open_transcript(None));
        let _ = host.tick();
        assert_eq!(
            host.state().fleet.transcript,
            Transcript::default(),
            "closing clears the field on the next tick"
        );
        sizes.borrow_mut().clear();
        let _ = host.tick();
        assert!(
            sizes.borrow().is_empty(),
            "and a closed transcript frames nothing after"
        );
    }
}

/// Section 20, which the board draws: the host reads it, folds each read
/// through the section's reducer, and reads it unreachable while the sidecar
/// has no daemon.
mod agent_status {
    use super::*;
    use ainb_desktop::agent_status::StatusOutcome;
    use ainb_hangar_proto::status_view::ViewHealth;

    /// A host framing section 20 only, whose reads the test stands in for.
    fn status_host(frames: &Rc<RefCell<usize>>) -> DesktopHost<impl FnMut(FrameBatch)> {
        scratch_home();
        let frames = Rc::clone(frames);
        DesktopHost::new(
            AppConfig::default(),
            Keymap::defaults(),
            HostId::local(),
            Subscription::only(&[SectionId::AgentStatus]),
            move |batch: FrameBatch| *frames.borrow_mut() += batch.frames.len(),
        )
        .rescanning_every(std::time::Duration::from_secs(600))
        .reading_agent_status_with(|_| {})
    }

    fn deliver(host: &DesktopHost<impl FnMut(FrameBatch)>, outcome: StatusOutcome) {
        host.agent_status_reports().lock().expect("inbox").push(outcome);
    }

    fn health(host: &DesktopHost<impl FnMut(FrameBatch)>) -> Option<ViewHealth> {
        host.state().agent_status.view.as_ref().map(|view| view.health.clone())
    }

    #[test]
    fn a_read_reaches_the_board_on_the_tick() {
        let frames = Rc::new(RefCell::new(0));
        let mut host = status_host(&frames);
        let _ = host.tick();
        *frames.borrow_mut() = 0;

        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();

        let view = host.state().agent_status.view.as_ref().expect("a view");
        assert!(view.cards.contains_key("acp:s-1"));
        assert_eq!(health(&host), Some(ViewHealth::Live));
        assert!(*frames.borrow() > 0, "and the section framed");
    }

    #[test]
    fn a_failed_read_keeps_the_cards_and_reads_unreachable() {
        let frames = Rc::new(RefCell::new(0));
        let mut host = status_host(&frames);
        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();

        deliver(&host, StatusOutcome::Failed("socket gone".to_string()));
        let _ = host.tick();

        assert!(matches!(
            health(&host),
            Some(ViewHealth::Unreachable { .. })
        ));
        assert!(
            host.state()
                .agent_status
                .view
                .as_ref()
                .expect("a view")
                .cards
                .contains_key("acp:s-1")
        );
    }

    #[test]
    fn a_lost_daemon_reads_unreachable_until_it_is_back() {
        let frames = Rc::new(RefCell::new(0));
        let mut host = status_host(&frames);
        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();

        host.daemon_lost("reconnecting");
        assert!(matches!(
            health(&host),
            Some(ViewHealth::Unreachable { .. })
        ));
        // A read that lands while the daemon is gone is not shown as current.
        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();
        assert!(matches!(
            health(&host),
            Some(ViewHealth::Unreachable { .. })
        ));

        host.daemon_connected();
        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();
        assert_eq!(health(&host), Some(ViewHealth::Live));
    }

    #[test]
    fn a_read_from_before_the_outage_never_folds_after_it() {
        let frames = Rc::new(RefCell::new(0));
        let mut host = status_host(&frames);
        deliver(&host, StatusOutcome::Read(acp_roster()));
        let _ = host.tick();
        // Stands in for a worker started before the outage: it keeps the inbox
        // it was handed, whatever the host does with its own after.
        let before = host.agent_status_reports();
        host.daemon_lost("reconnecting");
        host.daemon_connected();

        // It lands after the reconnect, into the inbox it was started with.
        before.lock().expect("inbox").push(StatusOutcome::Read(acp_roster()));
        let _ = host.tick();

        assert!(
            matches!(health(&host), Some(ViewHealth::Unreachable { .. })),
            "the pre-outage read is not shown as current"
        );
    }
}
