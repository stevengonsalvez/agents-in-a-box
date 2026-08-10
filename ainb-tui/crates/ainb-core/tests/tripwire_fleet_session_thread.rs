//! Tripwire: a per-session chat thread, driven the way an operator drives it.
//!
//! Part 2 Phase B's daemon half is `origin_message_id` linkage plus a scope
//! read, and both are invisible from the CLI's side of the socket. This drives
//! the REAL `ainb` binary in tmux against a REAL daemon on an isolated socket,
//! opens the thread the way a user does (`f`, then `5`, then `M`, each pressed
//! ONCE), and reads the pane.
//!
//! What is proven against the real daemon here:
//!
//! * `M` on a selected Fleet row opens THAT session's thread, headed by the
//!   session key and the `session:<key>` scope part 1 mints for it;
//! * a thread that already has history renders it in commit order, with the
//!   reply marked as a reply and attributed to the session rather than to the
//!   operator;
//! * the composer writes into the thread's own scope: the message the operator
//!   types comes back through `fleet/message_list` on `session:claude:demo`,
//!   which pins the CLIENT's derivation (`ChatTopic::scope_key`) to part 1's
//!   scope grammar. It is not a cross-check of the daemon's derivation — the
//!   composer supplies `scope_key` explicitly, so the daemon files the row under
//!   the string the client sent. The daemon's own derivation for a send that
//!   omits it is pinned in `rpc_message_bus.rs`
//!   (`a_threaded_send_round_trips_through_the_origin_join`, which asserts the
//!   stored `scope_key` of a send that supplied none);
//! * a reply threaded to that message by `origin_message_id` arrives live and
//!   renders UNDER it, marked.
//!
//! What is NOT proven here, and why:
//!
//! * a real agent's reply. No adapter process runs in this test, so the reply
//!   is seeded through the store with the same `origin_message_id` the daemon's
//!   turn-end writer sets. The daemon's own half (an origin must exist and must
//!   be in the same scope) is tested in `rpc_message_bus.rs`, against the RPC.
//! * a tmux delivery. The seeded session has no pane, so the delivery leg
//!   resolves terminal-not-delivered. The MESSAGE is still durable and in the
//!   thread, which is what this surface shows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{ExactTmuxSession, FleetHangar};

/// The session whose thread this test opens.
const SESSION_KEY: &str = "claude:demo";
/// Part 1's scope grammar for that session's own conversation. Written out as
/// a literal on purpose: it is the SCREEN's derivation
/// (`ChatTopic::scope_key`) that is pinned here, and a drift away from this
/// string stops the pre-seeded history rendering at all.
const SESSION_SCOPE: &str = "session:claude:demo";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|out| out.status.success())
}

/// A HOME with onboarding complete and the notify prompt dismissed.
///
/// The record is seeded under BOTH homes on purpose: `notifyd::Paths` resolves
/// `AINB_HANGAR_HOME` BEFORE `AINB_HOME`, this test sets both, and a missing
/// record fires an install modal whose first act is to eat the keypress this
/// test is about to send.
///
/// The onboarding version is read from the BINARY's own version. A literal
/// parses as major 0 and re-runs the wizard, which eats the same keypress.
fn seed_isolated_home(home: &Path, hangar_home: &Path) {
    let base = home.join(".agents-in-a-box");
    let config = base.join("config");
    fs::create_dir_all(&config).expect("create isolated config dir");
    fs::write(
        config.join("onboarding.toml"),
        format!(
            r#"completed = true
completed_at = "2026-08-08T00:00:00+00:00"
version = "{version}"
skipped_dependencies = []
git_directories = []
"#,
            version = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    seed_notify_dismissed(&base);
    seed_notify_dismissed(hangar_home);
}

fn seed_notify_dismissed(base: &Path) {
    fs::create_dir_all(base).expect("create notify record dir");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn type_text(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send-keys -l");
}

fn wait_for(session: &str, needle: &str, secs: u64) -> bool {
    wait_for_row(session, |row| row.contains(needle), secs).is_some()
}

/// Wait for a ROW matching `matches`, and return it.
///
/// Row-anchored on purpose: a bare pane-wide `contains` passes on any
/// incidental occurrence, which is how an assertion that proves nothing stays
/// green for a release.
fn wait_for_row<F>(session: &str, mut matches: F, secs: u64) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let capture = capture_pane(session);
        if let Some(row) = capture.lines().find(|row| matches(row)) {
            return Some(row.to_string());
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(400));
    }
}

/// The 0-based row index of the first line matching `matches`, so ORDER can be
/// asserted on what is painted rather than on what the state vector holds.
fn row_index<F>(pane: &str, mut matches: F) -> Option<usize>
where
    F: FnMut(&str) -> bool,
{
    pane.lines().position(|row| matches(row))
}

/// Seed one tmux-era Fleet session so the panel has a row to select.
///
/// `tmux_target` is NULL, so a send fails SAFE and its delivery leg resolves to
/// a terminal non-delivered state. The message itself is still persisted, which
/// is the half the thread renders.
fn seed_fleet_session(hangar: &FleetHangar, cwd: &Path) {
    let cwd = cwd.display().to_string();
    hangar.block_on(async {
        sqlx::query(
            "INSERT INTO fleet_session \
             (session_key, provider, cwd, display_name, lifecycle_state, management_state, \
              capabilities, discovered_at, last_observed_at, version) \
             VALUES (?, 'claude', ?, 'thread-demo', 'IDLE', 'MANAGED', \
                     '{\"send_prompt\":true}', 1, 1, 1)",
        )
        .bind(SESSION_KEY)
        .bind(&cwd)
        .execute(hangar.pool())
        .await
        .expect("seed the fleet session the thread is opened on");
    });
}

/// Insert one row straight into the thread's scope, as the daemon's own
/// turn-end writer would.
fn seed_message(hangar: &FleetHangar, id: &str, sender: &str, body: &str, origin: Option<&str>) {
    hangar.block_on(async {
        FleetMessageRepo::insert_message(
            hangar.pool(),
            &NewFleetMessage {
                id: id.to_string(),
                request_id: None,
                request_fingerprint: None,
                scope_key: SESSION_SCOPE.to_string(),
                origin_message_id: origin.map(str::to_string),
                sender: sender.to_string(),
                kind: if origin.is_some() { "agent" } else { "user" }.to_string(),
                body: body.to_string(),
                created_at: 1_700_000_000_000,
            },
        )
        .await
        .expect("seed a thread row");
    });
}

#[test]
fn a_session_thread_opens_orders_its_replies_and_takes_a_message() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-thread-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let hangar_home = home_tmp.path().join("hangar-home");
    seed_isolated_home(home_tmp.path(), &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);
    // A NAMED working directory, not the temp home. The panel titles a row with
    // its repository label, which is the cwd's basename, and the temp home's
    // basename is random per run (`ainb-thread-buics0`), so a test that waits
    // for a stable name never sees one. The assertion below is unchanged: the
    // fixture now sets up what it always claimed to.
    let cwd = home_tmp.path().join("thread-demo");
    fs::create_dir_all(&cwd).expect("create the session's working directory");
    seed_fleet_session(&hangar, &cwd);

    // History the thread already has when the operator opens it: a prompt and
    // the reply threaded to it. Inserted with DESCENDING ids so ordering can
    // only come from the commit-ordered `seq`; a renderer (or a query) that
    // sorted by the ULID would put the reply first.
    seed_message(&hangar, "01J0ZZZZPROMPT", "operator", "status?", None);
    seed_message(
        &hangar,
        "01J0AAAAREPLY",
        SESSION_KEY,
        "still running the suite",
        Some("01J0ZZZZPROMPT"),
    );

    let name = format!("ainb-thread-{}", std::process::id());
    let tmux = ExactTmuxSession::create(name, "180", "50");
    let session = tmux.name();
    let command = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_HANGAR_HOME={hangar} \
         AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 \
         CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
        hangar = hangar_home.display(),
        peers = home_tmp.path().join("peers.db").display(),
        jobs = home_tmp.path().join("jobs").display(),
        bin = ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &command, "C-m"])
        .status()
        .expect("launch the TUI");

    // Each key ONCE, after waiting for the screen that receives it. A retry
    // loop is how a modal that swallows the first press stays invisible.
    assert!(
        wait_for(session, "A I N B", 60),
        "the home screen never painted:\n{}",
        capture_pane(session)
    );
    send_key(session, "f");
    assert!(
        wait_for(session, "Fleet ·", 30),
        "the Fleet panel did not open on the first `f`:\n{}",
        capture_pane(session)
    );
    // The panel lands on the action queue, which shows only what needs the
    // operator; an IDLE session is not on it. `5` is the All view.
    send_key(session, "5");
    assert!(
        wait_for(session, "thread-demo", 30),
        "the seeded session never reached the roster:\n{}",
        capture_pane(session)
    );
    send_key(session, "M");

    // THE SCREEN FIRST. Everything below reads message rows, and reading them
    // off the wrong screen is how an assertion about a thread passes against
    // the panel underneath it.
    let header = wait_for_row(
        session,
        |row| row.contains("Fleet thread ·") && row.contains(SESSION_KEY),
        30,
    );
    assert!(
        header.is_some(),
        "`M` did not open the selected session's thread:\n{}",
        capture_pane(session)
    );
    assert!(
        header.as_deref().is_some_and(|row| row.contains(SESSION_SCOPE)),
        "the thread header does not name the scope it is reading: {header:?}"
    );

    // ORDER AND ATTRIBUTION. The reply is under the message it answers, marked
    // as a reply, and wearing the session's name rather than the operator's.
    assert!(
        wait_for(session, "still running the suite", 25),
        "the thread's history never rendered:\n{}",
        capture_pane(session)
    );
    let pane = capture_pane(session);
    let prompt = row_index(&pane, |row| {
        row.contains("YOU") && row.contains("│ status?")
    })
    .unwrap_or_else(|| panic!("the operator's prompt is not attributed to the operator:\n{pane}"));
    let reply = row_index(&pane, |row| row.contains("still running the suite"))
        .unwrap_or_else(|| panic!("no reply row:\n{pane}"));
    assert!(
        prompt < reply,
        "the reply rendered above the message it answers, so ordering is not by \
         commit seq (its ULID sorts first):\n{pane}"
    );
    let reply_row = pane.lines().nth(reply).unwrap_or_default();
    assert!(
        reply_row.contains('↳'),
        "the reply is not marked as threaded: {reply_row}"
    );
    assert!(
        reply_row.contains(SESSION_KEY),
        "the reply is not attributed to the session that wrote it: {reply_row}"
    );
    assert!(
        !reply_row.contains("YOU"),
        "an agent reply is attributed to the operator: {reply_row}"
    );

    // THE COMPOSER. What is typed here lands in the thread's own scope — the
    // one the SCREEN derived and supplied, which is what a drift in
    // `ChatTopic::scope_key` breaks.
    type_text(session, "run the tests");
    send_key(session, "Enter");
    let sent = wait_for_row(
        session,
        |row| row.contains("YOU") && row.contains("│ run the tests"),
        30,
    );
    assert!(
        sent.is_some(),
        "the composed message never came back through the thread's scope:\n{}",
        capture_pane(session)
    );
    let stored = hangar.block_on(async {
        FleetMessageRepo::list_by_scope(hangar.pool(), SESSION_SCOPE, 0, 50)
            .await
            .expect("read the thread back")
    });
    let composed = stored
        .iter()
        .find(|row| row.sender == "operator" && row.body == "run the tests")
        .unwrap_or_else(|| panic!("the daemon filed the message elsewhere: {stored:#?}"));

    // A REPLY ARRIVING LIVE, threaded to the message the daemon just minted.
    seed_message(
        &hangar,
        "01J0BBBBLIVE",
        SESSION_KEY,
        "tests are green",
        Some(&composed.id),
    );
    let live = wait_for_row(session, |row| row.contains("tests are green"), 25);
    assert!(
        live.is_some(),
        "a reply threaded to the composed message never arrived:\n{}",
        capture_pane(session)
    );
    let pane = capture_pane(session);
    let composed_row = row_index(&pane, |row| row.contains("│ run the tests"))
        .unwrap_or_else(|| panic!("the composed row left the pane:\n{pane}"));
    let live_row = row_index(&pane, |row| row.contains("tests are green"))
        .unwrap_or_else(|| panic!("the live reply left the pane:\n{pane}"));
    assert!(
        composed_row < live_row,
        "the live reply rendered above the message it answers:\n{pane}"
    );
    assert!(
        pane.lines().nth(live_row).unwrap_or_default().contains('↳'),
        "the live reply is not marked as threaded:\n{pane}"
    );
    eprintln!("--- the session thread, live ---\n{pane}");

    // And the thread is a chat surface, not a modal trap: one Esc returns to
    // the panel, because a thread has no card focus to step back through.
    send_key(session, "Escape");
    assert!(
        wait_for(session, "ACTION QUEUE", 20),
        "Esc did not return the thread to the Fleet panel:\n{}",
        capture_pane(session)
    );
}
