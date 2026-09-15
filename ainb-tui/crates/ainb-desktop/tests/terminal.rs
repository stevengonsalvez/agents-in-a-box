//! Terminal tabs against a real tmux on a private server: pane output reaches
//! the tab's sink, typed input reaches the pane, the cap evicts the tab idle
//! longest, and a session that ends closes its tab with a report.
//!
//! Each test runs its own tmux server on a socket in its own temporary
//! directory, named with `-S` on every command, so no test touches the server
//! anyone else is using and nothing mutates the process environment.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ainb_app::{CommandId, Intent};
use ainb_desktop::terminal::{
    MAX_ATTACHED_TABS, TabEvents, TabState, TabTarget, TabsView, Terminals, Tmux,
};

/// One test's private tmux server.
struct Server {
    // Under /tmp: a socket path must stay short.
    dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new().prefix("d1c").tempdir_in("/tmp").expect("tmux dir"),
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("tmux.sock")
    }

    fn tmux(&self) -> Tmux {
        Tmux::new(PathBuf::from("tmux")).on_socket(self.socket())
    }

    /// A tmux command against this server.
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.arg("-S").arg(self.socket()).env_remove("TMUX");
        command
    }

    /// A detached session running `command`, killed by exact name on drop.
    fn start(&self, name: &str, command: &str) -> Session<'_> {
        let status = self
            .command()
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
        Session {
            server: self,
            name: name.to_string(),
        }
    }
}

impl Drop for Server {
    /// Declared before its sessions and tabs, a server drops after them: every
    /// session has been killed by its exact name and every tab client with it,
    /// so tmux exits on its own (`exit-empty`). Waiting for that before the
    /// directory goes leaves no server and no directory behind; nothing here
    /// kills a server.
    fn drop(&mut self) {
        let running = || {
            self.command()
                .arg("list-sessions")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if running() {
            eprintln!(
                "tmux server on {} still up: a session leaked",
                self.socket().display()
            );
        }
    }
}

struct Session<'a> {
    server: &'a Server,
    name: String,
}

impl Session<'_> {
    fn capture(&self) -> String {
        let output = self
            .server
            .command()
            // A pane target: the session's active pane, `=` for an exact name.
            .args(["capture-pane", "-p", "-t", &format!("={}:", self.name)])
            .output()
            .expect("capture-pane runs");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn clients(&self) -> usize {
        let output = self
            .server
            .command()
            .args(["list-clients", "-t", &format!("={}", self.name)])
            .output()
            .expect("list-clients runs");
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    fn detach_clients(&self) {
        let status = self
            .server
            .command()
            .args(["detach-client", "-s", &format!("={}", self.name)])
            .status()
            .expect("detach-client runs");
        assert!(status.success());
    }

    fn kill(&self) {
        let _ = self
            .server
            .command()
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .status();
    }
}

impl Drop for Session<'_> {
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

fn terminals(server: &Server) -> (Terminals, Arc<Recorder>, mpsc::Receiver<Intent>) {
    let recorder = Arc::new(Recorder::default());
    let (reports_tx, reports) = mpsc::channel();
    let terminals = Terminals::new(server.tmux(), Events(Arc::clone(&recorder)), reports_tx);
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
    let server = Server::new();
    let session = server.start("d1c-io", "sh -c 'echo seeded-pane-output; exec sh'");
    let (terminals, recorder, _reports) = terminals(&server);

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
    let server = Server::new();
    let _sessions: Vec<Session> =
        names.iter().map(|name| server.start(name, "sleep 600")).collect();
    let (terminals, recorder, _reports) = terminals(&server);

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

    // Opening it again (a click reaches here through the reducer) re-attaches
    // it, and the tab idle longest now makes room.
    assert_eq!(terminals.open(tmux_tab(&names[0])), None);
    assert_eq!(state_of(&terminals, &names[0]), Some(TabState::Attached));
    assert_eq!(state_of(&terminals, &names[1]), Some(TabState::Detached));
}

#[test]
fn a_session_that_ends_closes_its_tab_with_a_report() {
    let server = Server::new();
    let session = server.start("d1c-end", "sleep 600");
    let (terminals, recorder, reports) = terminals(&server);
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
    let server = Server::new();
    let session = server.start("d1c-redial", "sleep 600");
    let (terminals, _recorder, _reports) = terminals(&server);
    assert_eq!(terminals.open(tmux_tab("d1c-redial")), None);
    // The tab's client registers with the server a moment after it starts.
    wait_for("the tab's client to attach", || session.clients() > 0);

    session.detach_clients();

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
    let server = Server::new();
    let (terminals, _recorder, _reports) = terminals(&server);
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
    let server = Server::new();
    let _session = server.start("d1c-close", "sleep 600");
    let (terminals, _recorder, reports) = terminals(&server);
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

/// The executor opens a tab for a session attach and keeps the documented
/// failure for a target a tab cannot hold.
#[test]
fn the_executor_opens_tabs_for_session_attaches_only() {
    use ainb_app::app::{Effect, TerminalTarget, TmuxSessionName};
    use ainb_desktop::executor::DesktopExecutor;
    use ainb_desktop::host::Executor;

    let server = Server::new();
    let _session = server.start("d1c-exec", "sleep 600");
    let recorder = Arc::new(Recorder::default());
    let executor = DesktopExecutor::new(None);
    let terminals = Terminals::new(
        server.tmux(),
        Events(Arc::clone(&recorder)),
        executor.report_sender(),
    );
    let mut executor = executor.with_terminals(terminals.clone());
    let name = TmuxSessionName::new("d1c-exec").expect("a valid tmux name");

    let reports = executor.execute(Effect::AttachTerminal(TerminalTarget::Session {
        id: uuid::Uuid::nil(),
        tmux_session: name.clone(),
    }));
    assert!(
        reports.is_empty(),
        "an opened tab reports nothing yet: {reports:?}"
    );
    assert_eq!(state_of(&terminals, "d1c-exec"), Some(TabState::Attached));

    let reports = executor.execute(Effect::AttachTerminal(TerminalTarget::InPlace {
        tmux_session: name,
        show_menu_bar: false,
    }));
    let (id, _) = report_named(&reports[0]);
    assert_eq!(id, ainb_app::app::reports::ids::IN_PLACE_FAILED);

    terminals.close("d1c-exec");
    let (id, _) = report_named(&executor.take_deferred()[0]);
    assert_eq!(
        id,
        ainb_app::app::reports::ids::ATTACH_FINISHED,
        "a closed tab reports on the tick"
    );
}
