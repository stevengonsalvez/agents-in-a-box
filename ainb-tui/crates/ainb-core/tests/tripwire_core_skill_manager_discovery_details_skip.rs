//! Tripwire: the discovery banner's `[d] details` and `[s] skip`
//! keybinds are wired end-to-end in the real `ainb` binary.
//!
//! The existing `tripwire_core_skill_manager_discovery_banner.rs`
//! covers the banner-on-first-open + `[Enter]` import path, and
//! `tripwire_core_skill_manager_refresh_discovery_live.rs` covers the
//! `[m]` suppress→refresh path. This file covers the two remaining
//! banner keybinds that had no live coverage:
//!
//!   1. `[d] details` — with the banner visible (Minimal tier, empty
//!      manifest), pressing `[d]` expands the compact banner into the
//!      per-tool breakdown: indented `~/.<tool>/skills/  N` rows for
//!      every tool home the Class-C walker found orphans in. The
//!      Minimal sandbox seeds orphans under both `~/.claude/skills/`
//!      and `~/.codex/skills/`, so both detail rows must appear after
//!      `[d]` (and must be ABSENT before, so the keystroke is proven
//!      load-bearing).
//!
//!   2. `[s] skip` — pressing `[s]` while the banner is visible must
//!      (a) dismiss the banner (no more "Detected existing units") AND
//!      (b) write the on-disk skip-marker that the `[m] refresh`
//!      feature later clears. This is the skip-persistence the whole
//!      suppress→refresh cycle depends on.
//!
//! NOTE on the skip-marker filename: the production `[s]` handler
//! (`apply_discovery_skip`) writes `<ainb_home>/.discovery-skipped`
//! (the `SKILL_MANAGER::SKIP_MARKER_FILE` constant), and that is the
//! exact file `maybe_show_discovery_banner` / `force_show_discovery_
//! banner` ([m]) read + delete. The work item's prose names the marker
//! `.skip-banner`, but `.skip-banner` is only a *fixture* artifact
//! (`fixtures.rs` seeds it for the Full tier) that production code
//! never reads. Asserting against the real `.discovery-skipped` marker
//! is what actually proves the "[m] depends on it" persistence — so
//! that is what this tripwire checks. If the production constant is
//! ever renamed, this test goes red, which is the correct signal.
//!
//! Drives the real `ainb` binary against the
//! `build_skill_manager_sandbox` fixture via tmux. Mirrors the launch +
//! onboarding-seed + capture-pane discipline of
//! `tripwire_core_skill_manager_sandbox_e2e.rs`,
//! `tripwire_core_skill_manager_keys_wired.rs`, and
//! `tripwire_core_skill_manager_refresh_discovery_live.rs`.
//!
//! Skips cleanly when `tmux` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{SandboxLayout, SandboxTier, build_skill_manager_sandbox};

/// Discovery skip-marker filename. MUST match
/// `skill_manager_screen::SKIP_MARKER_FILE` — that constant is private
/// to the binary crate, so we re-state the on-disk name here. If the
/// marker is ever renamed in the source, this tripwire goes red, which
/// is the correct signal (the [s]→[m] persistence broke).
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
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
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

/// Spawn a Minimal-tier sandbox with an EMPTY manifest (so the banner
/// is *allowed*) and NO skip-marker (so the banner shows on first
/// `[m]`), launch `ainb` under it, drive Home → SkillManager, and wait
/// for the discovery banner to render. Returns the live tmux session
/// name + the seeded layout. Caller is responsible for `kill_session`.
fn open_with_visible_banner(session_suffix: &str) -> (String, SandboxLayout, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("home tempdir");
    // Minimal tier: empty manifest (banner allowed) + orphan skills
    // under ~/.claude/skills (commit, fireworks-tech-graph) and
    // ~/.codex/skills (deploy) + one marketplace plugin, so the
    // Class-A + Class-C walkers both find candidates.
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal).expect("sandbox minimal");
    seed_onboarding(&layout);

    // Belt-and-braces: there must be NO skip-marker on disk, else the
    // banner would be suppressed on open.
    let _ = std::fs::remove_file(layout.ainb_home.join(SKIP_MARKER_FILE));

    let session = format!("tripwire-sm-{}-{}", session_suffix, std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &session,
            &launch_line(&layout, &bin),
            "Enter",
        ])
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
    send_key(&session, "z"); // Home → SkillManager (banner pops).

    // The banner MUST appear (no skip-marker, empty manifest, walkers
    // found candidates). Substring-AND across the title + the compact
    // rows + the overlay help keys so a partial paint can't false-pass.
    let banner = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Detected existing units")
            && c.contains("Marketplace plugins:")
            && c.contains("Orphan units:")
            && c.contains("[Enter]")
            && c.contains("[d]")
            && c.contains("[s]")
    });
    if banner.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!(
            "discovery banner never rendered after pressing m.\n\
             sandbox root: {}\nlast capture:\n{dump}",
            layout.root.display()
        );
    }

    (session, layout, tmp)
}

#[test]
fn pressing_d_expands_discovery_banner_with_per_tool_detail_rows() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let (session, _layout, _tmp) = open_with_visible_banner("ddetails");

    // Before [d] the banner is in the COMPACT view: the per-tool
    // breakdown rows (`~/.claude/skills/` / `~/.codex/skills/`) must be
    // ABSENT. Capture now and assert their absence so the [d] keystroke
    // is proven load-bearing (the detail rows can't already be on
    // screen from the compact render).
    let compact = capture_pane(&session);
    assert!(
        !compact.contains("~/.claude/skills/") && !compact.contains("~/.codex/skills/"),
        "per-tool detail rows already present in the COMPACT banner — \
         the [d] toggle would be vacuous:\n{compact}"
    );

    // Press [d] — expand to the detailed per-tool breakdown.
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "d");

    // After [d] the banner gains one indented row per tool home with
    // orphans. The Minimal sandbox seeds orphans under both ~/.claude
    // and ~/.codex, so both rows must appear. Substring-AND, never OR
    // — both rows prove the per-tool loop ran over the full walker
    // output, not just the first tool. The banner stays visible (its
    // title + help keys are still on screen).
    let detailed = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("Detected existing units")
            && c.contains("~/.claude/skills/")
            && c.contains("~/.codex/skills/")
            && c.contains("[d]")
            && c.contains("[s]")
    });
    let detailed = match detailed {
        Some(d) => d,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "[d] did not expand the banner into per-tool detail rows.\n\
                 expected `~/.claude/skills/` AND `~/.codex/skills/` after [d].\n\
                 last capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    assert!(
        detailed.contains("~/.claude/skills/"),
        "missing ~/.claude/skills/ detail row after [d]:\n{detailed}"
    );
    assert!(
        detailed.contains("~/.codex/skills/"),
        "missing ~/.codex/skills/ detail row after [d]:\n{detailed}"
    );
    // The banner itself must still be on screen — [d] is a toggle, not
    // a dismiss.
    assert!(
        detailed.contains("Detected existing units"),
        "banner dismissed by [d] (should only toggle detail rows):\n{detailed}"
    );
}

#[test]
fn pressing_s_dismisses_banner_and_writes_skip_marker() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let (session, layout, _tmp) = open_with_visible_banner("sskip");

    // The skip-marker must NOT exist yet (open_with_visible_banner
    // removed any stale one). This guards against a false-positive
    // where the file pre-exists.
    let skip_marker = layout.ainb_home.join(SKIP_MARKER_FILE);
    assert!(
        !skip_marker.exists(),
        "skip-marker present BEFORE pressing [s] — cannot prove [s] wrote it"
    );

    // Press [s] — skip + persist.
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "s");

    // (a) The banner must DISMISS — "Detected existing units" gone, and
    //     we land back on the SkillManager screen (Sources / Units /
    //     Detail chrome still alive). The Minimal manifest is empty so
    //     the Units panel shows its empty-state hint.
    let dismissed = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        !c.contains("Detected existing units")
            && c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
    });
    let dismissed = match dismissed {
        Some(d) => d,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "[s] did not dismiss the discovery banner / land on SkillManager.\n\
                 last capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    assert!(
        !dismissed.contains("Detected existing units"),
        "banner still visible after [s]:\n{dismissed}"
    );

    // (b) The on-disk skip-marker must now exist — this is the
    //     persistence the [m] refresh feature later clears. Poll briefly
    //     since the fs write happens just after the keystroke is
    //     processed.
    let mut wrote = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if skip_marker.exists() {
            wrote = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(
        wrote,
        "[s] did not write the skip-marker `{}` on disk — the [s]→[m] \
         suppress/refresh persistence is broken.\nexpected at: {}",
        SKIP_MARKER_FILE,
        skip_marker.display()
    );
}
