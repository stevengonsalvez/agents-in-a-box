//! Tripwire (witr epic): pressing `w` embeds witr's own interactive
//! browser (`witr -i`) full-screen, end-to-end against the real `ainb`
//! binary in tmux.
//!
//! witr's flagship UX — the sortable all-process list + live ancestry
//! pane (Processes/Ports/Containers/Locks tabs) — lives only inside
//! witr's bubbletea TUI; it has no JSON/WireBuffer equivalent, so it
//! cannot be stubbed. ainb therefore embeds it the same way it attaches
//! to an agent session: `w` → `AppEvent::GoToWitr` → `AsyncAction::
//! AttachWitr` → the main loop runs `tmux new-session -A -d -s ainb-witr
//! "witr -i"` and attaches (TUI suspend → witr's TUI → resume on quit).
//!
//! This test drives the real chain: launch ainb in tmux, press `w`, and
//! assert the `ainb-witr` session now exists running witr's interactive
//! browser. (Inside a tmux harness ainb's `attach` nests, but the
//! detached `new-session -A -d` that precedes it still spawns the
//! browser, which is the user-visible proof. In a plain terminal it's a
//! clean full-screen handoff.)
//!
//! Requires the REAL `witr` binary on PATH (no stub possible) and tmux —
//! skips gracefully otherwise, like `real_witr_smoke.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// tmux session ainb spawns for the embedded witr browser. Must match
/// `WITR_SESSION` in `crates/ainb-core/src/main.rs`'s `AttachWitr` arm.
const WITR_SESSION: &str = "ainb-witr";

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

/// The embed runs the REAL `witr -i` — there is no way to stub an
/// interactive bubbletea TUI — so the test skips when witr isn't
/// installed (CI without the binary), mirroring `real_witr_smoke.rs`.
fn witr_available() -> bool {
    Command::new("witr")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Seed an isolated HOME with `onboarding.toml` so the wizard skips and
/// the TUI boots straight to home.
fn seed_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
    // Suppress the ainb-hooks first-run install dialog — it overlays the
    // home screen and swallows the first keystroke (`s`/`w`) these tests
    // send.
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
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

fn poll<F>(deadline: Instant, mut ok: F) -> bool
where
    F: FnMut() -> bool,
{
    while Instant::now() < deadline {
        if ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(400));
    }
    false
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn has_session(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn pressing_w_embeds_witr_interactive_browser() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !witr_available() {
        eprintln!(
            "SKIP: real `witr` binary not on PATH — the embed runs `witr -i`, which can't be stubbed"
        );
        return;
    }

    // Start from a clean slate — a leftover ainb-witr would make the
    // assertion pass without ainb doing anything.
    kill_session(WITR_SESSION);

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());

    let session = format!("tripwire-witr-embed-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Launch ainb inheriting the real PATH so /opt/homebrew/bin/witr (etc.)
    // resolves. Single send-keys + Enter to avoid the launch-command
    // truncation race.
    let cmd = format!(
        "HOME={} exec {} tui",
        home_tmp.path().display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    // Wait for HomeScreen (Stats sidebar entry + its `[i]` shortcut).
    let home_ok = poll(Instant::now() + Duration::from_secs(90), || {
        let c = capture_pane(&session);
        c.contains("Stats") && c.contains("[i]")
    });
    if !home_ok {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Press `w` → ainb should spawn the ainb-witr session running `witr -i`.
    send_key(&session, "w");
    let spawned = poll(Instant::now() + Duration::from_secs(40), || {
        has_session(WITR_SESSION)
    });
    if !spawned {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`w` did not spawn the `{WITR_SESSION}` tmux session; ainb pane:\n---\n{last}\n---");
    }

    // The ainb-witr pane must show witr's real interactive browser, not a
    // shell or a "command not found". Positive: the tab strip + the search
    // prompt witr's TUI paints. Negative: no launch failure.
    let browser_ok = poll(Instant::now() + Duration::from_secs(30), || {
        let c = capture_pane(WITR_SESSION);
        c.contains("Processes") && c.contains("Search") && !c.contains("command not found")
    });
    let witr_pane = capture_pane(WITR_SESSION);

    kill_session(WITR_SESSION);
    kill_session(&session);

    assert!(
        browser_ok,
        "`{WITR_SESSION}` exists but isn't running witr's interactive browser:\n---\n{witr_pane}\n---"
    );
}

/// Overlay-panels: witr opened FROM THE SESSION LIST returns to the
/// session list when the user quits witr — not to home.
///
/// Witr is the odd panel out: instead of a wire-rendered screen it hands
/// the terminal to `witr -i` (suspend/attach) and resumes ainb when witr
/// quits. The overlay-panels contract is that resume lands on the ORIGIN
/// screen, and it gets that for free precisely because `AttachWitr` never
/// touches `current_screen`. This test proves the for-free guarantee
/// holds end-to-end: a regression that navigated somewhere on the witr
/// path (or reset `current_screen` on resume) would surface as ainb
/// resuming on the wrong screen.
///
/// Harness note: inside tmux, ainb's attach to the `ainb-witr` session
/// switches THIS pane's client to display witr (the nested-attach the
/// header test documents). Quitting witr ends that session, ainb's
/// attach returns, the main loop resumes, and the pane repaints
/// `current_screen` — which is still the session list.
#[test]
fn witr_opened_from_session_list_resumes_on_session_list() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !witr_available() {
        eprintln!("SKIP: real `witr` binary not on PATH — the embed runs `witr -i`");
        return;
    }

    kill_session(WITR_SESSION);

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());

    let session = format!("tripwire-witr-resume-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} exec {} tui",
        home_tmp.path().display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    // Home → session list. `del-sel` is unique to the session-list
    // legend. Plugins are enabled here (witr needs the real PATH), so
    // cold start is slower — keep the deadline generous.
    let home_ok = poll(Instant::now() + Duration::from_secs(90), || {
        let c = capture_pane(&session);
        c.contains("Stats") && c.contains("[i]")
    });
    if !home_ok {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }
    send_key(&session, "s");
    let on_sessions = poll(Instant::now() + Duration::from_secs(40), || {
        capture_pane(&session).contains("del-sel")
    });
    if !on_sessions {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("session list never rendered after `s`; last:\n---\n{last}\n---");
    }

    // Press `w` → ainb spawns `ainb-witr` running `witr -i` and attaches.
    send_key(&session, "w");
    let spawned = poll(Instant::now() + Duration::from_secs(40), || {
        has_session(WITR_SESSION)
    });
    if !spawned {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "`w` from session list did not spawn `{WITR_SESSION}`; ainb pane:\n---\n{last}\n---"
        );
    }

    // Quit witr (`q` is witr's quit binding). Once witr exits, its tmux
    // session ends and ainb's attach returns.
    send_key(WITR_SESSION, "q");
    let witr_gone = poll(Instant::now() + Duration::from_secs(30), || {
        !has_session(WITR_SESSION)
    });
    if !witr_gone {
        // Some witr views need a second `q` to leave a detail pane first.
        send_key(WITR_SESSION, "q");
        let _ = poll(Instant::now() + Duration::from_secs(25), || {
            !has_session(WITR_SESSION)
        });
    }

    // Resume must land back on the SESSION LIST (origin), not home.
    let resumed = poll(Instant::now() + Duration::from_secs(30), || {
        let c = capture_pane(&session);
        c.contains("del-sel") && !c.contains("Getting Started")
    });
    let final_cap = capture_pane(&session);
    kill_session(WITR_SESSION);
    kill_session(&session);

    assert!(
        resumed,
        "after quitting witr (opened from the session list) ainb did not resume on the \
         session list within 30s. Final pane:\n---\n{final_cap}\n---"
    );
}
