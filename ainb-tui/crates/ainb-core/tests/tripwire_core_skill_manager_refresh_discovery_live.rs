//! Tripwire: the `[m] refresh discovery` keybind actually clears a
//! prior skip-marker and RE-SHOWS the discovery banner — proving the
//! empty-state hint ("Or press [m] again to refresh discovery") tells
//! the truth end-to-end.
//!
//! The existing `tripwire_core_skill_manager_discovery_banner.rs`
//! covers the banner-on-first-open + import path. This file covers
//! the complementary suppress → refresh path that was unverified e2e:
//!
//! 1. Seed a Minimal-tier sandbox: EMPTY manifest (so the banner is
//!    *allowed* to appear) + walker candidates under the tool homes
//!    (so it has something to show).
//! 2. Plant the discovery skip-marker (`<ainb_home>/.discovery-skipped`)
//!    so the banner is suppressed on the first SkillManager open — the
//!    same state a prior `[s] skip` leaves behind.
//! 3. Open SkillManager (`m` from Home) and assert NO banner — the
//!    empty-state Units panel ("No units installed yet" + the
//!    "[m] again to refresh discovery" hint) renders instead.
//! 4. Press `[m]` and assert the discovery banner RE-APPEARS — i.e.
//!    `force_show_discovery_banner` cleared the skip-marker and the
//!    walkers re-populated the overlay.
//!
//! Drives the real `ainb` binary against the
//! `build_skill_manager_sandbox` fixture via tmux. Mirrors the launch
//! + onboarding-seed + capture-pane discipline of
//! `tripwire_core_skill_manager_sandbox_e2e.rs` and
//! `tripwire_core_skill_manager_keys_wired.rs`.
//!
//! Skips cleanly when `tmux` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{build_skill_manager_sandbox, SandboxLayout, SandboxTier};

/// Discovery skip-marker filename. MUST match
/// `skill_manager_screen::SKIP_MARKER_FILE` — that constant is private
/// to the binary crate, so we re-state the on-disk name here. If the
/// marker is ever renamed in the source, this tripwire goes red
/// (banner won't suppress on open), which is the correct signal.
const SKIP_MARKER_FILE: &str = ".discovery-skipped";

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

/// Seed the onboarding marker so the first-run wizard doesn't steal
/// our keystrokes. Same pattern as every other live tmux tripwire.
fn seed_onboarding(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
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
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

/// Quote a path for shell so spaces / special chars in the tempdir
/// path don't bork the `HOME=… exec ainb …` line the test sends.
fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn launch_line(layout: &SandboxLayout, bin: &Path) -> String {
    let mut launch = String::new();
    for (k, v) in layout.env_vars() {
        launch.push_str(k);
        launch.push('=');
        launch.push_str(&sh_quote(Path::new(&v)));
        launch.push(' ');
    }
    launch.push_str("exec ");
    launch.push_str(&sh_quote(bin));
    launch.push_str(" 2>&1");
    launch
}

#[test]
fn pressing_m_clears_skip_marker_and_reshows_discovery_banner() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    // Minimal tier: EMPTY manifest (banner is *allowed*) + orphan
    // skills / one marketplace plugin under the tool homes (walkers
    // find candidates). Full tier would write a non-empty manifest,
    // which `force_show_discovery_banner` rightly refuses to override.
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal)
        .expect("sandbox minimal");
    seed_onboarding(&layout);

    // Plant the skip-marker so the banner is suppressed on first open
    // — exactly the on-disk state a prior `[s] skip` leaves behind.
    let skip_marker = layout.ainb_home.join(SKIP_MARKER_FILE);
    std::fs::write(&skip_marker, b"").expect("plant skip-marker");
    assert!(skip_marker.exists(), "skip-marker not planted");

    let session = format!("tripwire-sm-refresh-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch_line(&layout, &bin), "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to render fully before keystroking.
    if poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    })
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "z"); // Home → SkillManager.

    // On open the skip-marker SUPPRESSES the banner. We must land on
    // the SkillManager screen with the empty-state Units panel — the
    // "No units installed yet" hint that promises [m] refreshes
    // discovery. Anchor on that text so we know the screen painted.
    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Sources") && c.contains("Units") && c.contains("No units installed")
    });
    let pre = match pre {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "SkillManager empty-state never rendered after pressing m.\n\
                 sandbox root: {}\nlast capture:\n{dump}",
                layout.root.display()
            );
        }
    };
    // The banner MUST NOT be present on open — the skip-marker
    // suppressed it. If this fires, the suppression path regressed
    // (or the fixture seeded a non-empty manifest by accident).
    assert!(
        !pre.contains("Detected existing units"),
        "discovery banner showed on open despite skip-marker present \
         — suppression regressed:\n{pre}"
    );
    // Sanity: the empty-state hint that advertises [m] is on-screen.
    assert!(
        pre.contains("refresh discovery"),
        "empty-state [m] hint missing — fixture/screen drift:\n{pre}"
    );

    // Press [m] — explicit discovery refresh. This clears the
    // skip-marker and force-shows the banner.
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "m");

    // The banner MUST re-appear. Substring-AND across the title +
    // overlay help keys so a partial paint can't false-pass.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Detected existing units")
            && c.contains("Marketplace plugins:")
            && c.contains("Orphan units:")
            && c.contains("[Enter]")
            && c.contains("[s]")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "discovery banner did NOT re-appear after pressing [m] — \
                 force_show_discovery_banner failed to clear the skip-marker \
                 / re-walk.\nlast capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    assert!(
        post.contains("Detected existing units"),
        "banner title missing after [m] refresh:\n{post}"
    );

    // The skip-marker must be gone on disk — `force_show_discovery_
    // banner` deletes it so the banner keeps re-appearing until the
    // user imports or skips again. This is the load-bearing side
    // effect the empty-state hint promises.
    assert!(
        !skip_marker.exists(),
        "skip-marker still present after [m] refresh — it should have \
         been cleared so the banner keeps re-appearing"
    );
}
