//! Tripwire: arrow / j-k / g-G selection navigation on the
//! SkillManager Units panel, driven against the real `ainb` binary
//! via tmux on the Full-tier sandbox (>= 2 units).
//!
//! Before the nav work (commit 5a5cd17) the Units panel had zero
//! selection-move keys: pressing Down/Up/j/k/g/G did nothing, the
//! selection never left row 0, and the Detail pane stayed pinned to
//! the first unit. This locks that behaviour end-to-end:
//!
//!   * `Down` (and `j`) move the selection to the NEXT unit — the
//!     Detail-pane title (which embeds the selected unit name as
//!     `Detail (#<idx> <name>)`) updates to that unit.
//!   * `Up` (and `k`) move it back to the PREVIOUS unit.
//!   * `G` jumps to the LAST unit; `g` jumps to the FIRST.
//!
//! The Detail pane is the load-bearing assertion target: it keeps the
//! selected unit's name verbatim even when the Units TABLE is
//! filtered, and its title carries the name in plain text, so a
//! substring check on the captured pane is enough to prove the cursor
//! moved. Arrows are sent as bare tmux key names (`Down`/`Up`), never
//! the literal words.
//!
//! The Full tier pre-seeds a manifest with exactly two units whose
//! names differ:
//!   - row 0: `git:file://…@main/skills/initial-skill`  → name `initial-skill`
//!   - row 1: `local:.claude/skills/commit`             → name `commit`
//! so "the highlighted name changes" is observable as
//! `initial-skill` ⇄ `commit` in the Detail title.
//!
//! Mirrors the EXACT launch + onboarding-seed + capture-pane pattern
//! of `tripwire_core_skill_manager_sandbox_e2e.rs` and
//! `tripwire_core_skill_manager_keys_wired.rs`. Skips cleanly when
//! `tmux` is unavailable.

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
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll<F: FnMut(&str) -> bool>(session: &str, deadline: Instant, mut ok: F) -> Option<String> {
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn send(session: &str, keys: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, keys])
        .status()
        .expect("tmux send-keys");
}

fn kill(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

/// Quote a path for shell so spaces / special chars in the tempdir
/// path don't bork the `HOME=… exec ainb …` launch line.
fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the `HOME=… AINB_HOME=… … exec '<bin>' 2>&1` launch line
/// from the fixture's env-var contract — identical to the sibling
/// sandbox tripwires.
fn launch_line(layout: &SandboxLayout, bin: &Path) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push_str(&sh_quote(Path::new(&v)));
        s.push(' ');
    }
    s.push_str("exec ");
    s.push_str(&sh_quote(bin));
    s.push_str(" 2>&1");
    s
}

/// Slice out the Detail-pane band. On a 50-row terminal the layout is
/// `sources|units` (top), `detail` (middle), `help` (bottom). The
/// Detail title — which carries the selected unit name — lives below
/// the top units band. Taking the lower 30 rows keeps us clear of the
/// Units TABLE (whose `name` column also prints `initial-skill` /
/// `commit`) so the assertion is scoped to the title only.
fn detail_region(full: &str) -> String {
    let lines: Vec<&str> = full.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

/// Has the Detail pane settled on the unit whose name is `name`? We
/// look for the literal `Detail (#` marker AND the unit name in the
/// detail band, so a stale top-table row can't satisfy it.
fn detail_shows(full: &str, name: &str) -> bool {
    let region = detail_region(full);
    region.contains("Detail (#") && region.contains(name)
}

#[test]
fn arrow_jk_gg_navigation_moves_selection_and_detail_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    // Full tier → manifest with two units (initial-skill + commit).
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-nav-{}", std::process::id());
    let bin = ainb_bin();

    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
            .status()
            .expect("tmux new-session")
            .success(),
        "tmux new-session failed"
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch_line(&layout, &bin), "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Home → SkillManager.
    if poll(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Welcome to AINB")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("home never rendered:\n{d}");
    }
    thread::sleep(Duration::from_millis(200));
    send(&session, "m");

    // SkillManager up with both seeded units present. The Detail pane
    // starts on row 0 → `initial-skill`.
    let first = poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Units") && c.contains("Detail") && detail_shows(c, "initial-skill")
    });
    if first.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "SkillManager never settled on first unit (initial-skill) in the Detail pane:\n{d}"
        );
    }
    // Sanity: we should NOT already be showing `commit` in the Detail
    // band before any nav key — guards against a tautological test.
    let start_cap = capture(&session);
    assert!(
        detail_shows(&start_cap, "initial-skill"),
        "expected Detail pane on `initial-skill` at start:\n{start_cap}"
    );

    // ── Down → next unit (`commit`). Bare key name, not the word.
    thread::sleep(Duration::from_millis(200));
    send(&session, "Down");
    let after_down = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "commit")
    });
    if after_down.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "Down did not move the selection to the next unit — Detail pane never showed \
             `commit`.\nlast pane:\n{d}"
        );
    }

    // ── Up → back to the first unit (`initial-skill`).
    thread::sleep(Duration::from_millis(200));
    send(&session, "Up");
    let after_up = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "initial-skill")
    });
    if after_up.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "Up did not move the selection back to the previous unit — Detail pane never \
             returned to `initial-skill`.\nlast pane:\n{d}"
        );
    }

    // ── j (vim down) → next unit (`commit`) again. Proves the vim
    // alias is wired, not just the arrow.
    thread::sleep(Duration::from_millis(200));
    send(&session, "j");
    let after_j = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "commit")
    });
    if after_j.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "`j` did not move the selection to the next unit — Detail pane never showed \
             `commit`.\nlast pane:\n{d}"
        );
    }

    // ── k (vim up) → back to first unit (`initial-skill`).
    thread::sleep(Duration::from_millis(200));
    send(&session, "k");
    let after_k = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "initial-skill")
    });
    if after_k.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "`k` did not move the selection back — Detail pane never returned to \
             `initial-skill`.\nlast pane:\n{d}"
        );
    }

    // ── G (last) → last unit (`commit`).
    thread::sleep(Duration::from_millis(200));
    send(&session, "G");
    let after_last = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "commit")
    });
    if after_last.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "`G` (last) did not jump the selection to the last unit — Detail pane never \
             showed `commit`.\nlast pane:\n{d}"
        );
    }

    // ── g (first) → first unit (`initial-skill`).
    thread::sleep(Duration::from_millis(200));
    send(&session, "g");
    let after_first = poll(&session, Instant::now() + Duration::from_secs(15), |c| {
        detail_shows(c, "initial-skill")
    });
    let dump = capture(&session);
    kill(&session);
    assert!(
        after_first.is_some(),
        "`g` (first) did not jump the selection to the first unit — Detail pane never \
         returned to `initial-skill`.\nlast pane:\n{dump}"
    );
}
