//! The desktop host's contract with the state machine, headless: what it frames,
//! when effects run, and that its config is the one it was given.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::app::Effect;
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, Intent, Keymap, SectionId};
use ainb_desktop::host::{DesktopHost, Executor};

/// One scratch HOME for this binary, set before any host is built. Nothing in
/// these tests may write under it.
fn scratch_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        std::env::set_var("AINB_HOME", home.path());
        std::env::set_var("AINB_HANGAR_HOME", home.path().join(".agents-in-a-box"));
        home
    })
    .path()
}

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
