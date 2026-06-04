//! Tripwire: the provenance matcher (bead `ai-ya9`, v1.3) surfaces an
//! external-clone orphan as its real `gh:` upstream — not a bare
//! `local:` orphan — in the live SkillManager Units table.
//!
//! The Full-tier sandbox seeds:
//!   - `~/.claude/skills/fireworks-tech-graph/SKILL.md` (an orphan that
//!     simulates the legacy bootstrap cloning an external git repo), and
//!   - `external-dependencies.yaml` whose `agent-skills[]` lists
//!     `fireworks-tech-graph` → `https://github.com/yizhiyanhua-ai/fireworks-tech-graph`.
//!
//! Flow:
//!   1. Launch the real `ainb` binary against the sandbox (HOME =
//!      sandbox root) and open the SkillManager (`m`).
//!   2. The Full tier seeds a NON-EMPTY manifest, so the first-open
//!      banner is suppressed. Press `[m]` again to FORCE discovery
//!      (`force_show_discovery_banner` ignores the non-empty manifest),
//!      then `[Enter]` to import all discovered units.
//!   3. The provenance-aware import reconciles the fireworks orphan to a
//!      `gh:` URI. Assert the Units table now shows it as `gh:`
//!      (external) and NOT as `local:` for that row.
//!
//! Mirrors the launch + onboarding-seed + capture-pane discipline of
//! `tripwire_core_skill_manager_sandbox_e2e.rs` and
//! `tripwire_core_skill_manager_keys_wired.rs`. Skips cleanly when
//! `tmux` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{build_skill_manager_sandbox, SandboxLayout, SandboxTier};

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

/// Width of the Sources panel (see `skill_manager_screen::render` —
/// `Constraint::Length(32)`). The Units table starts at this column,
/// so dropping the first 32 chars of each physical line strips the
/// Sources-panel text (which carries `local:` source URIs that would
/// otherwise bleed onto the same line as a `gh:`-sourced Units row).
const SOURCES_PANEL_COLS: usize = 32;

/// The Units TABLE occupies the top band of the screen, to the RIGHT
/// of the Sources panel; the Detail pane (which keeps the selected
/// unit's full URI even when the table is scrolled) sits lower. Scope
/// substring checks to the Units columns ONLY: take the top ~22 rows
/// (clear of the Detail pane on a 50-row terminal) and drop the
/// leading Sources panel from each line so the Sources panel's
/// `local:` URIs can't false-trigger the "no local: fireworks" guard.
fn units_region(full: &str) -> String {
    full.lines()
        .take(22)
        .map(|line| {
            line.chars()
                .skip(SOURCES_PANEL_COLS)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn external_clone_orphan_shows_as_gh_not_local_in_units() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    // Full tier seeds the fireworks-tech-graph orphan + the
    // external-dependencies.yaml that names it. It also seeds a
    // non-empty manifest, so the banner is suppressed on open — we
    // force discovery with a second [m] below.
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full)
        .expect("sandbox full");
    seed_onboarding(&layout);

    // Sanity: the fixture really seeded the inputs the matcher needs.
    assert!(
        layout
            .claude_home
            .join("skills/fireworks-tech-graph/SKILL.md")
            .is_file(),
        "fixture drift: fireworks-tech-graph orphan not seeded"
    );
    assert!(
        layout.root.join("external-dependencies.yaml").is_file(),
        "fixture drift: external-dependencies.yaml not seeded"
    );

    let session = format!("tripwire-sm-provenance-{}", std::process::id());
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
        c.contains("Agents") && c.contains("Catalog") && c.contains("Welcome to AINB")
    })
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "m"); // Home → SkillManager.

    // SkillManager opens with the Full-tier pre-seeded units (banner
    // suppressed by the non-empty manifest).
    if poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources") && c.contains("Units") && c.contains("initial-skill")
    })
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!(
            "SkillManager screen never rendered.\nsandbox root: {}\nlast capture:\n{dump}",
            layout.root.display()
        );
    }

    // [m] again → force discovery banner past the non-empty manifest.
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "m");
    if poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Detected existing units") && c.contains("[Enter]")
    })
    .is_none()
    {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!(
            "discovery banner did not appear after forced [m] refresh.\nlast capture:\n{dump}"
        );
    }

    // [Enter] → import all (provenance-aware reconcile writes the
    // fireworks orphan as its gh: upstream).
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "Enter");

    // The Units table must now show the fireworks unit sourced from a
    // `gh:` URI — its real external provenance — NOT a `local:` orphan.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        let region = units_region(c);
        region.contains("fireworks") && region.contains("gh:")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            // Also dump the manifest so a failure shows what the import
            // actually wrote.
            let manifest =
                std::fs::read_to_string(layout.ainb_home.join("manifest.yaml")).unwrap_or_default();
            kill_session(&session);
            panic!(
                "fireworks unit never showed as gh: in the Units table after import.\n\
                 manifest.yaml:\n{manifest}\n\nlast capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    let region = units_region(&post);
    assert!(
        region.contains("fireworks") && region.contains("gh:"),
        "fireworks unit should be sourced from gh: in the Units table:\n{region}"
    );
    // And it must NOT appear on a `local:` row in the Units table —
    // that would mean the provenance matcher failed to resolve the
    // external clone to its upstream. Check no Units line carries BOTH
    // `fireworks` and `local:`.
    let local_fireworks_line = region
        .lines()
        .any(|line| line.contains("fireworks") && line.contains("local:"));
    assert!(
        !local_fireworks_line,
        "fireworks unit still shows a local: source in the Units table — \
         provenance matcher did not resolve the external clone to gh::\n{region}"
    );

    // Belt-and-braces: the manifest the import wrote must carry a gh:
    // URI for fireworks (the load-bearing side effect).
    let manifest =
        std::fs::read_to_string(layout.ainb_home.join("manifest.yaml")).unwrap_or_default();
    assert!(
        manifest
            .lines()
            .any(|l| l.contains("gh:") && l.contains("fireworks-tech-graph")),
        "manifest.yaml missing a gh: entry for fireworks-tech-graph after import:\n{manifest}"
    );
}
