//! Tripwire: the SkillManager `[r] remove` help-bar key uninstalls the
//! selected unit and produces a *user-visible* effect in the live
//! binary — with `initial-skill` selected, pressing `r`:
//!   1. drops that row from the Units table, and
//!   2. surfaces a "removed: ..." success notification (top-right toast).
//!
//! Drives the real `ainb` binary against the sandbox fixture via tmux,
//! mirroring the exact launch + onboarding-seed + capture-pane pattern in
//! `tripwire_core_skill_manager_sandbox_e2e.rs`,
//! `tripwire_core_skill_manager_keys_wired.rs`, and
//! `tripwire_core_skill_manager_check_live.rs`.
//!
//! Assertions are on the captured pane only:
//!   * The "removed:" toast is transient (3s TTL on Success notifications),
//!     so we poll fast (200ms) to catch it inside its window.
//!   * The Detail pane (lower band of the screen) keeps showing the
//!     selected unit's *name* even after the Units TABLE is rebuilt, so the
//!     "row gone" assertion is scoped to the Units-table region (top rows),
//!     mirroring the search tripwire's region-scoping note.
//!
//! Skips cleanly when `tmux` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{SandboxLayout, SandboxTier, build_skill_manager_sandbox};

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

/// Seed the onboarding marker so the first-run wizard doesn't steal our
/// keystrokes. Same pattern as every other live tmux tripwire.
fn seed_onboarding(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("config dir");
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("onboarding");
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    std::fs::write(
        layout.root.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll<F: FnMut(&str) -> bool>(session: &str, deadline: Instant, mut ok: F) -> Option<String> {
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(200));
    }
    None
}

fn send(session: &str, keys: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, keys])
        .status()
        .expect("send-keys");
}

fn kill(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

fn launch_line(layout: &SandboxLayout, bin: &Path) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push('\'');
        s.push_str(&Path::new(&v).to_string_lossy());
        s.push('\'');
        s.push(' ');
    }
    s.push_str("exec '");
    s.push_str(&bin.to_string_lossy());
    s.push_str("' 2>&1");
    s
}

/// The Units table occupies the top band of the screen; the Detail pane
/// is the lower band and keeps echoing the selected unit's name even
/// after the table is rebuilt. Scope substring checks to the table by
/// taking the first 20 rows (well clear of the Detail pane on a 50-row
/// terminal). Mirrors the search tripwire's region-scoping helper.
fn units_region(full: &str) -> String {
    full.lines().take(20).collect::<Vec<_>>().join("\n")
}

#[test]
fn remove_key_uninstalls_unit_and_surfaces_notification_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    // Full tier pre-seeds a manifest with the `initial-skill` unit, so the
    // Units table has a row to remove and no discovery banner intercepts.
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-remove-{}", std::process::id());
    let bin = ainb_bin();
    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
            .status()
            .expect("new-session")
            .success()
    );
    Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &session,
            &launch_line(&layout, &bin),
            "Enter",
        ])
        .status()
        .expect("launch");

    // Home → SkillManager.
    if poll(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Skills (manager)")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("home never rendered:\n{d}");
    }
    thread::sleep(Duration::from_millis(200));
    send(&session, "z");

    // Wait for the Units table to render with the seeded unit. The row is
    // selected by default (cursor on the first/only unit), so `[r]` will
    // target it.
    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        units_region(c).contains("initial-skill")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("units table with initial-skill never rendered:\n{d}");
    }

    // [r] remove is a two-step confirm so a stray keypress can't uninstall:
    // the FIRST `r` arms a one-shot confirm for the selected row and shows a
    // "press r again to confirm" warning; the SECOND `r` runs `skill remove
    // --yes`, drops the unit from the manifest, reloads the screen from disk
    // (dropping the row), and emits the success notification.
    thread::sleep(Duration::from_millis(200));
    send(&session, "r");
    // The first `r` armed the one-shot confirm (its "press r again to
    // confirm" warning is often truncated in the toast, so we don't assert
    // it). Give the arm a beat to register, then the SECOND `r` — cursor
    // unmoved, so the same row stays armed — confirms and uninstalls.
    thread::sleep(Duration::from_millis(500));
    send(&session, "r");

    // Assertion 1: the "removed:" success toast surfaces top-right. This is
    // unique to the notification — the help bar shows only `[r]emove`, never
    // the word "removed" — so matching it proves the `[r]` keybind fired
    // `SkillManagerRemove` and the uninstall succeeded (not "remove failed").
    let toast = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        c.contains("removed:")
    });
    if toast.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "[r] remove did not surface the 'removed:' success toast within its TTL. \
             The uninstall either failed or the effect never reached the screen.\n\
             last pane:\n{d}"
        );
    }

    // Assertion 2: the Units table loses the row. The Detail pane keeps the
    // name, so scope to the Units-table region. Poll until the row is gone
    // (reload_from_disk runs synchronously inside the `[r]` handler, so the
    // row should already be absent by the time the toast surfaced — but
    // poll defensively against capture timing).
    let gone = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        !units_region(c).contains("initial-skill")
    });
    let dump = capture(&session);
    kill(&session);
    assert!(
        gone.is_some(),
        "[r] remove did not drop `initial-skill` from the Units table — the row is \
         still present in the Units region after uninstall.\nlast pane:\n{dump}"
    );
}
