//! Tripwire: the Fleet panel opens from Home, renders hook-materialized
//! `current_state`, lets the operator move an ASK option, attempts the answer
//! dispatch path, and returns to Home via `Esc`.
//!
//! This is the live-terminal sibling of the Fleet panel unit tests. It drives
//! the real `ainb` binary in tmux with an isolated HOME seeded with:
//!
//! 1. completed onboarding + a complete dismissed notify install record, so no
//!    first-run modal swallows the `f` key;
//! 2. a notifyd SQLite store populated via the real `Store::upsert_current_state`
//!    API the materializer uses.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Paths, StateRow, Store};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-06-30T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(base.join("install.json"), install_record).expect("seed install.json");

    let paths = Paths::under(&base);
    fs::create_dir_all(&paths.base).expect("create ainb base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    let ask = StateRow {
        session_id: "fleet-panel-ask-1".to_string(),
        cwd: home.join("fleet-tripwire-project").display().to_string(),
        kind: "ASK".to_string(),
        context: Some(
            r#"{"question":"Deploy patched fleet bridge?","header":"Release gate","options":[{"label":"Yes","description":"ship verified fix"},{"label":"Continue","description":"keep watching"}]}"#
                .to_string(),
        ),
        parent: Some("atc-main".to_string()),
        last_event_ts: 300,
        source: "hook".to_string(),
    };
    let wait = StateRow {
        session_id: "fleet-panel-wait-1".to_string(),
        cwd: home.join("waiting-project").display().to_string(),
        kind: "WAIT".to_string(),
        context: Some(
            r#"{"reason":"permission_prompt","message":"allow cargo test?"}"#.to_string(),
        ),
        parent: Some("atc-main".to_string()),
        last_event_ts: 200,
        source: "hook".to_string(),
    };
    store.upsert_current_state(&ask).expect("seed ASK current_state");
    store.upsert_current_state(&wait).expect("seed WAIT current_state");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        send_key(session, key);
        thread::sleep(Duration::from_millis(500));
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
    }
    None
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn fleet_panel_opens_renders_answers_and_returns_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-fleet-panel-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let peers_db = home_tmp.path().join("peers.db");
    let jobs_dir = home_tmp.path().join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_DISABLE_PLUGINS=1 CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
        peers = peers_db.display(),
        jobs = jobs_dir.display(),
        bin = ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Fleet") && c.contains("[f]")
    });
    let Some(pre_cap) = pre else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered Fleet shortcut; last capture:\n---\n{last}\n---");
    };
    assert!(
        !pre_cap.contains("Deploy patched fleet bridge?"),
        "pre-key capture already on Fleet panel — state leaked:\n{pre_cap}"
    );

    let opened = poll_capture_resending(
        &session,
        "f",
        Instant::now() + Duration::from_secs(30),
        |c| {
            c.contains("Fleet")
                && c.contains("current_state")
                && c.contains("ASK")
                && c.contains("Deploy patched fleet bridge?")
                && c.contains("Yes")
                && c.contains("Continue")
                && c.contains("WAIT")
        },
    );
    let Some(open_cap) = opened else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Fleet panel did not render seeded current_state; last capture:\n---\n{last}\n---");
    };
    assert!(
        open_cap.contains("Enter/a") && open_cap.contains("q/Esc"),
        "Fleet help bar missing answer/back controls:\n{open_cap}"
    );

    send_key(&session, "Tab");
    let advanced = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("▶ Continue")
    });
    let Some(tab_cap) = advanced else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Tab did not move ASK option cursor to Continue; last capture:\n---\n{last}\n---");
    };
    assert!(
        tab_cap.contains("keep watching"),
        "selected option description missing after Tab:\n{tab_cap}"
    );

    send_key(&session, "Enter");
    let answered = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("answering ask with 'Continue'")
            || c.contains("answered ask")
            || c.contains("no live session matched")
    });
    let Some(answer_cap) = answered else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("Fleet answer dispatch feedback never rendered; last capture:\n---\n{last}\n---");
    };
    assert!(
        answer_cap.contains("Continue")
            && (answer_cap.contains("answering ask")
                || answer_cap.contains("answered ask")
                || answer_cap.contains("no live session matched")),
        "answer feedback did not name the selected option/path:\n{answer_cap}"
    );

    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("Deploy patched fleet bridge?")
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back.is_some(),
        "Esc from Fleet panel did not return to Home. Final capture:\n---\n{final_cap}\n---"
    );
}
