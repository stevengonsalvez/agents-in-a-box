//! Terminal tabs against a real tmux on a private socket directory: pane output
//! reaches the tab's sink, typed input reaches the pane, the cap evicts the tab
//! idle longest, and a session that ends closes its tab with a report.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ainb_app::{CommandId, Intent};
use ainb_desktop::terminal::{
    MAX_ATTACHED_TABS, TabEvents, TabState, TabTarget, TabsView, Terminals,
};

/// One private tmux socket directory for the test binary, so no test touches
/// the tmux server anyone else is using.
fn private_tmux() -> PathBuf {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        // Under /tmp: a socket path must stay short.
        let dir = tempfile::Builder::new().prefix("d1c").tempdir_in("/tmp").expect("tmux dir");
        std::env::set_var("TMUX_TMPDIR", dir.path());
        std::env::remove_var("TMUX");
        dir
    });
    assert!(dir.path().exists());
    PathBuf::from("tmux")
}

/// A detached tmux session running `command`, killed by exact name on drop.
struct Session(String);

impl Session {
    fn start(name: &str, command: &str) -> Self {
        let tmux = private_tmux();
        let status = Command::new(&tmux)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-x",
                "80",
                "-y",
                "24",
            ])
            .args(["-s", name, command])
            .status()
            .expect("tmux runs");
        assert!(status.success(), "tmux session {name} started");
        Self(name.to_string())
    }

    fn capture(&self) -> String {
        let output = Command::new(private_tmux())
            // A pane target: the session's active pane, `=` for an exact name.
            .args(["capture-pane", "-p", "-t", &format!("={}:", self.0)])
            .output()
            .expect("capture-pane runs");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn clients(&self) -> usize {
        let output = Command::new(private_tmux())
            .args(["list-clients", "-t", &format!("={}", self.0)])
            .output()
            .expect("list-clients runs");
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    fn kill(&self) {
        let _ = Command::new(private_tmux())
            .args(["kill-session", "-t", &format!("={}", self.0)])
            .status();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Default)]
struct Recorder {
    tabs: Mutex<Vec<TabsView>>,
    toasts: Mutex<Vec<String>>,
}

/// The recorder as the tabs' event sink: a local type, since `TabEvents` and
/// `Arc` both live in other crates.
struct Events(Arc<Recorder>);

impl TabEvents for Events {
    fn tabs(&self, view: TabsView) {
        self.0.tabs.lock().unwrap().push(view);
    }

    fn toast(&self, message: String) {
        self.0.toasts.lock().unwrap().push(message);
    }
}

fn terminals() -> (Terminals, Arc<Recorder>, mpsc::Receiver<Intent>) {
    let recorder = Arc::new(Recorder::default());
    let (reports_tx, reports) = mpsc::channel();
    let terminals = Terminals::new(private_tmux(), Events(Arc::clone(&recorder)), reports_tx);
    (terminals, recorder, reports)
}

fn tmux_tab(name: &str) -> TabTarget {
    TabTarget::Tmux {
        tmux: name.to_string(),
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn report_named(report: &Intent) -> (String, serde_json::Value) {
    match report {
        Intent::Command(id, args) => (id.as_str().to_string(), args.clone()),
        other => panic!("not a report: {other:?}"),
    }
}

fn state_of(terminals: &Terminals, key: &str) -> Option<TabState> {
    terminals
        .view()
        .tabs
        .into_iter()
        .find(|tab| tab.key == key)
        .map(|tab| tab.state)
}

#[test]
fn pane_output_reaches_the_sink_and_typed_input_reaches_the_pane() {
    let session = Session::start("d1c-io", "sh -c 'echo seeded-pane-output; exec sh'");
    let (terminals, recorder, _reports) = terminals();

    assert_eq!(terminals.open(tmux_tab("d1c-io")), None, "the tab opened");
    assert_eq!(
        recorder.tabs.lock().unwrap().last().and_then(|view| view.focus.clone()),
        Some("d1c-io".to_string()),
        "an open focuses its tab"
    );

    let painted = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&painted);
    assert!(terminals.attach_output(
        "d1c-io",
        Box::new(move |bytes| {
            sink.lock().unwrap().extend(bytes);
            true
        })
    ));
    wait_for("the seeded output in the tab", || {
        String::from_utf8_lossy(&painted.lock().unwrap()).contains("seeded-pane-output")
    });

    terminals.input("d1c-io", b"echo typed-$((40+2))\r".to_vec());
    wait_for("the typed line in the pane", || {
        session.capture().contains("typed-42")
    });
}

#[test]
fn the_ninth_tab_detaches_the_tab_idle_longest() {
    let names: Vec<String> = (0..=MAX_ATTACHED_TABS).map(|i| format!("d1c-cap{i}")).collect();
    let _sessions: Vec<Session> =
        names.iter().map(|name| Session::start(name, "sleep 600")).collect();
    let (terminals, recorder, _reports) = terminals();

    for name in &names {
        assert_eq!(terminals.open(tmux_tab(name)), None, "{name} opened");
        // Distinct open times, so "idle longest" has one answer.
        std::thread::sleep(Duration::from_millis(20));
    }

    let view = terminals.view();
    assert_eq!(
        view.tabs.len(),
        MAX_ATTACHED_TABS + 1,
        "the evicted tab stays listed"
    );
    assert_eq!(state_of(&terminals, &names[0]), Some(TabState::Detached));
    assert_eq!(
        view.tabs.iter().filter(|tab| tab.state == TabState::Attached).count(),
        MAX_ATTACHED_TABS
    );
    assert!(
        recorder.toasts.lock().unwrap().iter().any(|toast| toast.contains(&names[0])),
        "a toast names the detached tab"
    );

    // A click re-attaches it, and the tab idle longest now makes room.
    terminals.reattach(&names[0]);
    assert_eq!(state_of(&terminals, &names[0]), Some(TabState::Attached));
    assert_eq!(state_of(&terminals, &names[1]), Some(TabState::Detached));
}

#[test]
fn a_session_that_ends_closes_its_tab_with_a_report() {
    let session = Session::start("d1c-end", "sleep 600");
    let (terminals, recorder, reports) = terminals();
    assert_eq!(terminals.open(tmux_tab("d1c-end")), None);

    session.kill();
    wait_for("the tab to close", || {
        state_of(&terminals, "d1c-end").is_none()
    });

    let (id, args) = report_named(&reports.recv_timeout(Duration::from_secs(5)).expect("a report"));
    assert_eq!(id, ainb_app::app::reports::ids::ATTACH_FINISHED);
    assert_eq!(args["target"], serde_json::json!({ "tmux": "d1c-end" }));
    assert!(args["outcome"].get("target_missing").is_some(), "{args}");
    assert!(recorder.toasts.lock().unwrap().iter().any(|toast| toast.contains("d1c-end")));
}

#[test]
fn a_dropped_client_on_a_live_session_redials() {
    let session = Session::start("d1c-redial", "sleep 600");
    let (terminals, _recorder, _reports) = terminals();
    assert_eq!(terminals.open(tmux_tab("d1c-redial")), None);
    // The tab's client registers with the server a moment after it starts.
    wait_for("the tab's client to attach", || session.clients() > 0);

    let status = Command::new(private_tmux())
        .args(["detach-client", "-s", "=d1c-redial"])
        .status()
        .expect("detach-client runs");
    assert!(status.success());

    wait_for("the tab to start reconnecting", || {
        matches!(
            state_of(&terminals, "d1c-redial"),
            Some(TabState::Reconnecting { .. })
        )
    });
    wait_for("the tab to be attached again", || {
        state_of(&terminals, "d1c-redial") == Some(TabState::Attached)
    });
    drop(session);
}

#[test]
fn opening_a_missing_session_reports_it_and_lists_nothing() {
    private_tmux();
    let (terminals, _recorder, _reports) = terminals();
    let target = TabTarget::Session {
        id: uuid::Uuid::nil(),
        tmux: "d1c-missing".to_string(),
    };

    let report = terminals.open(target).expect("a failure report");
    let (id, args) = report_named(&report);
    assert_eq!(id, ainb_app::app::reports::ids::ATTACH_FINISHED);
    assert!(args["outcome"].get("target_missing").is_some(), "{args}");
    assert!(terminals.view().tabs.is_empty());
}

#[test]
fn closing_a_tab_reports_the_user_left_it() {
    let _session = Session::start("d1c-close", "sleep 600");
    let (terminals, _recorder, reports) = terminals();
    assert_eq!(terminals.open(tmux_tab("d1c-close")), None);

    terminals.close("d1c-close");

    assert!(terminals.view().tabs.is_empty());
    let (id, args) = report_named(&reports.recv_timeout(Duration::from_secs(5)).expect("a report"));
    assert_eq!(
        CommandId::new(id).as_str(),
        ainb_app::app::reports::ids::ATTACH_FINISHED
    );
    assert_eq!(args["outcome"], serde_json::json!("detached"));
}
