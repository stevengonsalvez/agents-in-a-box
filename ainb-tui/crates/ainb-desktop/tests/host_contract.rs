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

/// A key-only row writes outside ainb, so a chord that lands on one is named
/// for the shell to refuse; any other chord is not.
#[test]
fn a_chord_on_a_key_only_row_is_named() {
    let log = Log::default();
    let host = host(&[SectionId::Shell], &log);

    assert_eq!(
        host.key_only_command(&Chord::parse("W").expect("valid chord"))
            .as_ref()
            .map(ainb_app::CommandId::as_str),
        Some("global.wire_statusline")
    );
    assert_eq!(
        host.key_only_command(&Chord::parse("s").expect("valid chord")),
        None
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
