//! Tripwire: the SkillManager `[u] update` help-bar key is wired and
//! produces a *user-visible* result notification in the live binary —
//! with a unit selected (Full-tier sandbox seeds `initial-skill`),
//! pressing `u` runs the in-process update flow against the local bare
//! remote and surfaces a result toast top-right: either a success
//! `updated: …` (the update applied / was already current) or a clean
//! `update failed: …` error. EITHER outcome proves the `[u]` keybind
//! dispatched `SkillManagerUpdate` and the effect reached the screen —
//! the help bar shows only `[u]pdate`, never the colon-suffixed
//! `updated:` / `update failed:` forms, so a match cannot come from the
//! static chrome.
//!
//! Drives the real `ainb` binary against the sandbox fixture via tmux,
//! mirroring the exact launch + onboarding-seed + capture-pane pattern
//! in `tripwire_core_skill_manager_sandbox_e2e.rs`,
//! `tripwire_core_skill_manager_keys_wired.rs`, and
//! `tripwire_core_skill_manager_check_live.rs`.
//!
//! The result toast is the proof, not a screen-alive check: a bare
//! `update: no unit selected` warning would mean the keybind fired but
//! no unit was selected, which the test rejects — it asserts the
//! `updated:` / `update failed:` forms that only run AFTER a unit URI
//! is resolved from the Units table.
//!
//! Offline-safe: the bare remote is a `file://` path created by the
//! fixture on disk, so the update's `git` fetch reaches it without a
//! network. Skips cleanly when `tmux` is unavailable on PATH.

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

#[test]
fn update_key_surfaces_result_notification_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    // Full tier pre-seeds a manifest with `initial-skill` (declared
    // `git:file://<bare>@main/skills/initial-skill`) and the bare remote
    // on disk, so a unit is selected by default and `[u]` has a real
    // upstream to fetch from — no banner intercepts our `u` keystroke.
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-update-{}", std::process::id());
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

    // Wait for the Units table to render (Full tier → `initial-skill`).
    // Its presence means a unit row exists and is the default selection,
    // so `[u]` resolves a URI rather than warning "no unit selected".
    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("initial-skill")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("units table with initial-skill never rendered:\n{d}");
    }

    // [u] → update the selected unit. `run_skill_cli` runs synchronously
    // and the result notification posts immediately; success toasts have
    // a 3s TTL and error toasts 5s, so poll fast (200ms) to catch it.
    thread::sleep(Duration::from_millis(200));
    send(&session, "u");

    // The result toast is one of:
    //   * `updated: …`        (success — applied or already current)
    //   * `update failed: …`  (clean error against the local bare remote)
    // Both are emitted ONLY after the handler resolves a selected unit's
    // URI and runs the update CLI — neither string appears in the static
    // help bar (`[u]pdate`). Matching either proves the keybind
    // dispatched `SkillManagerUpdate` end-to-end, not just that the
    // screen is alive. A bare `update: no unit selected` warning would
    // NOT match (no `updated:` / `update failed:` prefix), so the test
    // also proves a unit was actually selected.
    let surfaced = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("updated:") || c.contains("update failed:")
    });
    let dump = capture(&session);
    kill(&session);
    assert!(
        surfaced.is_some(),
        "[u] update did not surface a result toast (`updated: …` or \
         `update failed: …`) top-right within its TTL. The keybind effect \
         never reached the screen — either `[u]` did not dispatch \
         `SkillManagerUpdate`, no unit was selected, or the notification \
         was dropped.\nlast pane:\n{dump}"
    );
}
