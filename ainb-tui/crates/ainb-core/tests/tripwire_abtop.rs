//! Tripwire (abtop epic): pressing `t` (or selecting the "abtop
//! (top-for-agents)" sidebar item) embeds abtop's own full-screen monitor
//! end-to-end against the real `ainb` binary in tmux.
//!
//! abtop's flagship UX — the live agent table + quota / tokens / projects /
//! ports / mcp / sessions panels — lives only inside abtop's ratatui TUI; it
//! has no JSON/WireBuffer equivalent, so it cannot be stubbed. ainb embeds it
//! the same way it attaches to an agent session: `t` → `AppEvent::GoToAbtop`
//! → (first launch) a one-time consent dialog offering `abtop --setup` →
//! `AsyncAction::AttachAbtop` → the main loop runs `tmux new-session -A -d -s
//! ainb-abtop "abtop --exit-on-jump"` and attaches (TUI suspend → abtop's TUI
//! → resume on quit).
//!
//! This single test drives the real chain end-to-end: launch ainb in tmux,
//! press `t`, assert the first-launch consent dialog renders (J6), choose
//! "Just open abtop", then assert the `ainb-abtop` session now exists running
//! abtop's real monitor (J1). (Inside a tmux harness ainb's `attach` nests,
//! but the detached `new-session -A -d` that precedes it still spawns the
//! monitor, which is the user-visible proof. In a plain terminal it's a clean
//! full-screen handoff.)
//!
//! Requires the REAL `abtop` binary on PATH (no stub possible) and tmux —
//! skips gracefully otherwise, like `real_abtop_smoke.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// tmux session ainb spawns for the embedded abtop monitor. Must match
/// `ABTOP_SESSION` in `crates/ainb-core/src/main.rs`'s `AttachAbtop` arm.
const ABTOP_SESSION: &str = "ainb-abtop";

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

/// The embed runs the REAL `abtop` — there is no way to stub an interactive
/// ratatui TUI — so the test skips when abtop isn't installed (CI without the
/// binary), mirroring `real_abtop_smoke.rs`. `--version` exits 0 and prints
/// `abtop <ver>`; accept either signal.
fn abtop_available() -> bool {
    Command::new("abtop")
        .arg("--version")
        .output()
        .map(|o| o.status.success() || String::from_utf8_lossy(&o.stdout).contains("abtop"))
        .unwrap_or(false)
}

/// Seed an isolated HOME with `onboarding.toml` so the wizard skips and the
/// TUI boots straight to home. Deliberately does NOT seed
/// `~/.claude/abtop-rate-limits.json`, so the first-launch setup consent
/// dialog fires (the J6 leg of this test).
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

    // Suppress the first-run ainb-hooks notify-install prompt — it pops a
    // confirmation dialog on the home screen that swallows the `t` shortcut
    // (the dialog handler only consumes arrows/Enter/Esc). A complete
    // `InstallRecord` JSON with `prompt_dismissed = true` makes
    // `prompt_state` return None. (Fields are not all `#[serde(default)]`, so
    // every key must be present.)
    let base = home.join(".agents-in-a-box");
    fs::create_dir_all(&base).expect("create base dir");
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
fn pressing_t_offers_setup_then_embeds_abtop() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !abtop_available() {
        eprintln!(
            "SKIP: real `abtop` binary not on PATH — the embed runs `abtop --exit-on-jump`, which can't be stubbed"
        );
        return;
    }

    // Start from a clean slate — a leftover ainb-abtop would make the
    // assertion pass without ainb doing anything.
    kill_session(ABTOP_SESSION);

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());

    let session = format!("tripwire-abtop-embed-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Launch ainb inheriting the real PATH so ~/.cargo/bin/abtop resolves.
    // Single send-keys + Enter to avoid the launch-command truncation race.
    // The embed is pure host code (AttachAbtop) + the real abtop binary —
    // plugins aren't needed, so disable discovery to keep the launch fast and
    // free of unrelated plugin-spawn noise.
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    // Wait for HomeScreen (Stats sidebar entry + its `[i]` shortcut).
    let home_ok = poll(Instant::now() + Duration::from_secs(45), || {
        let c = capture_pane(&session);
        c.contains("Stats") && c.contains("[i]")
    });
    if !home_ok {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // J6: press `t` → first launch should offer to run `abtop --setup`
    // (the rate-limit consent dialog) BEFORE attaching, and must NOT have
    // spawned the monitor session yet.
    send_key(&session, "t");
    let consent_ok = poll(Instant::now() + Duration::from_secs(20), || {
        capture_pane(&session).contains("rate-limit")
    });
    if !consent_ok {
        let last = capture_pane(&session);
        kill_session(ABTOP_SESSION);
        kill_session(&session);
        panic!(
            "first-launch abtop setup consent dialog never rendered after `t`:\n---\n{last}\n---"
        );
    }
    assert!(
        !has_session(ABTOP_SESSION),
        "abtop was launched before the user answered the setup consent dialog"
    );

    // Choose "Just open abtop" (the 2nd tri-option): Right cycles forward,
    // Enter confirms → `AsyncAction::AttachAbtop` (no `abtop --setup`).
    send_key(&session, "Right");
    thread::sleep(Duration::from_millis(300));
    send_key(&session, "Enter");

    // J1: ainb should now spawn the ainb-abtop session running the monitor.
    let spawned = poll(Instant::now() + Duration::from_secs(20), || {
        has_session(ABTOP_SESSION)
    });
    if !spawned {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "choosing 'Just open abtop' did not spawn the `{ABTOP_SESSION}` tmux session; ainb pane:\n---\n{last}\n---"
        );
    }

    // The ainb-abtop pane must show abtop's real monitor, not a shell or a
    // "command not found". Positive: the version header + panel chrome abtop
    // always paints (even with no agents). Negative: no launch failure.
    let monitor_ok = poll(Instant::now() + Duration::from_secs(30), || {
        let c = capture_pane(ABTOP_SESSION);
        c.contains("abtop v")
            && (c.contains("quota") || c.contains("sessions"))
            && !c.contains("command not found")
    });
    let abtop_pane = capture_pane(ABTOP_SESSION);

    kill_session(ABTOP_SESSION);
    kill_session(&session);

    assert!(
        monitor_ok,
        "`{ABTOP_SESSION}` exists but isn't running abtop's monitor:\n---\n{abtop_pane}\n---"
    );
}

/// Overlay-panels parity: abtop opened FROM THE SESSION LIST (the `t`
/// shortcut is now mirrored there, like stats/witr/skills) returns to the
/// session list when the user quits abtop — not home.
///
/// Like witr, abtop is a tmux suspend/attach (`AttachAbtop` never touches
/// `current_screen`), so resume-on-origin is automatic — this proves it
/// end-to-end and that the new session-list `t` binding actually launches
/// abtop. Seeds `abtop-setup-dismissed` so the first-run consent dialog
/// doesn't intercept `t` (that leg is covered by the test above).
#[test]
fn abtop_opened_from_session_list_resumes_on_session_list() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !abtop_available() {
        eprintln!("SKIP: real `abtop` binary not on PATH — the embed runs `abtop --exit-on-jump`");
        return;
    }

    kill_session(ABTOP_SESSION);

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());
    // Skip the one-time setup consent so `t` launches abtop directly.
    fs::write(
        home_tmp.path().join(".agents-in-a-box").join("abtop-setup-dismissed"),
        b"1",
    )
    .expect("seed abtop-setup-dismissed");

    let session = format!("tripwire-abtop-resume-{}", std::process::id());
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

    // Home → session list (plugins enabled — abtop needs the real PATH —
    // so cold start is slower; keep the deadline generous).
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

    // Press `t` → ainb spawns `ainb-abtop` running the monitor and attaches.
    send_key(&session, "t");
    let spawned = poll(Instant::now() + Duration::from_secs(25), || {
        has_session(ABTOP_SESSION)
    });
    if !spawned {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "`t` from session list did not spawn `{ABTOP_SESSION}`; ainb pane:\n---\n{last}\n---"
        );
    }

    // Quit abtop (`q`). Its tmux session ends and ainb's attach returns.
    send_key(ABTOP_SESSION, "q");
    let gone = poll(Instant::now() + Duration::from_secs(15), || {
        !has_session(ABTOP_SESSION)
    });
    if !gone {
        send_key(ABTOP_SESSION, "q");
        let _ = poll(Instant::now() + Duration::from_secs(10), || {
            !has_session(ABTOP_SESSION)
        });
    }

    // Resume must land back on the SESSION LIST (origin), not home.
    let resumed = poll(Instant::now() + Duration::from_secs(15), || {
        let c = capture_pane(&session);
        c.contains("del-sel") && !c.contains("Getting Started")
    });
    let final_cap = capture_pane(&session);
    kill_session(ABTOP_SESSION);
    kill_session(&session);

    assert!(
        resumed,
        "after quitting abtop (opened from the session list) ainb did not resume on the \
         session list within 15s. Final pane:\n---\n{final_cap}\n---"
    );
}
