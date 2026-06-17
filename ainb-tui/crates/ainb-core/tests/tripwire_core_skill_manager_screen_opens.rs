//! Tripwire: pressing `z` on the HomeScreen opens the SkillManager
//! screen with the spec §10.1 layout markers visible.
//!
//! Catches: HomeScreen `M` keybind missing, View::SkillManager
//! render branch dropped from layout dispatch, state mutation
//! handler removed. The in-process TestBackend renders show the
//! layout works in isolation; this tripwire proves the LIVE binary
//! actually reaches the screen on `M` press.
//!
//! Skips when `tmux` isn't on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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

/// Seed an isolated `$HOME` so the first-run onboarding wizard
/// doesn't intercept our keystrokes. Mirrors the canonical pattern
/// from `tmux-ui-tripwire` on `feat/plugin`.
fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
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
    // Suppress the first-run ainb-hooks install nudge (intercepts the
    // first key on the home screen). See build_skill_manager_sandbox.
    fs::write(
        cfg.parent().unwrap().join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n",
    )
    .expect("seed install.json");
}

/// Seed `$HOME/.agents-in-a-box/manifest.yaml` + `lock.yaml` so the
/// SkillManager screen's live-data binding (hdt.9) has real rows to
/// render. The default `default_ainb_home()` resolution lands at
/// `$HOME/.agents-in-a-box` when neither `$AINB_HOME` nor
/// `$XDG_CONFIG_HOME` is set, which matches the `HOME=<tmp>` env
/// we hand the spawned `ainb` binary.
///
/// The seeded fixture is intentionally non-empty so the discovery
/// banner trigger (`manifest.units.is_empty()` guard) does NOT
/// fire — the banner overlay would otherwise sit on top of the
/// Sources/Units panels and break the substring assertions below.
fn seed_manifest_and_lockfile(home: &Path) {
    let ainb_home = home.join(".agents-in-a-box");
    fs::create_dir_all(&ainb_home).expect("create ainb_home");

    // Manifest: one source + one unit. Unit URI uses gh: so the URI
    // parser's source-type/locator split lines up cleanly in the
    // Detail pane (the live binding reassembles the URI from the
    // row projection).
    let manifest_yaml = r#"schema_version: 1
sources:
  - name: stevie-skills
    type: gh
    uri: gh:stevie/skills
    ref: main
    enabled: true
units:
  - uri: gh:stevie/skills@main/skills/commit
    targets: [claude]
"#;
    fs::write(ainb_home.join("manifest.yaml"), manifest_yaml).expect("seed manifest.yaml");

    // Lockfile: matching LockedUnit pointing at a synthetic deploy
    // path so the Detail pane has a concrete "Deployed:" line to
    // assert against.
    let lock_yaml = r#"schema_version: 1
generated_at: "2026-05-26T00:00:00+00:00"
sources: []
units:
  - uri: gh:stevie/skills@abc123/skills/commit
    declared_uri: gh:stevie/skills@main/skills/commit
    kind: skill
    sha: abc123
    deployed:
      claude:
        status: deployed
        path: /tmp/tripwire-deployed/claude/skills/commit
        file_hashes:
          SKILL.md: deadbeef
"#;
    fs::write(ainb_home.join("lock.yaml"), lock_yaml).expect("seed lock.yaml");
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

#[test]
fn pressing_m_on_home_opens_skill_manager_screen() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_manifest_and_lockfile(home_tmp.path());

    // Use process-id-suffixed exact session name so concurrent test
    // runs don't collide. NEVER wildcard-kill — only exact name.
    let session = format!("tripwire-sm-open-{}", std::process::id());
    let bin = ainb_bin();

    // Spawn a sized tmux session, then launch ainb inside it.
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to render FULLY. The previous bare
    // (Agents && Catalog) pair could match mid-paint, before the
    // welcome panel had painted, leading to a race where m was
    // sent while the binary was still booting and got lost.
    // Require the welcome-panel literal too so the predicate only
    // fires when both the sidebar AND the right-hand panel are
    // alive — the binary is then ready to consume keystrokes.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    let home_render = match home_render {
        Some(r) => r,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!("HomeScreen never rendered. last capture:\n{dump}");
        }
    };

    // Small settle window before keystroke. ratatui paints at
    // 4-30 Hz; 200ms guarantees at least 1 idle frame between
    // initial-paint completion and the keystroke arriving.
    thread::sleep(Duration::from_millis(200));

    // Negative pre-press: we should NOT already be on the
    // SkillManager screen. If "Sources" already appears here the
    // test is testing the wrong starting state.
    assert!(
        !home_render.contains("Sources") || !home_render.contains("Units"),
        "test invariant broken: SkillManager markers visible on HomeScreen capture:\n{home_render}"
    );

    // Press m (lowercase — see app/events.rs HomeScreen V2 match arm).
    // NEVER append Enter to a single-char nav key, per tripwire-skill
    // hard rule #3.
    send_key(&session, "z");

    // Positive AND negative markers per hard rule #2 — substring-OR
    // on lone chrome strings ("Sources", "ainb") would silently pass
    // if the wrong screen rendered. Wait until the LIVE DATA has
    // painted — the seeded source name "stevie-skills" only appears
    // when SkillsScreenData::reload_from_disk wired into the screen-
    // open handler ran and the manifest+lockfile are reflected in
    // the Sources/Units panels.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
            && c.contains("[i]")
            && c.contains("stevie-skills")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "SkillManager screen never rendered live data after pressing M. last capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    // Chrome — the screen layout is alive.
    assert!(post.contains("Sources"), "missing Sources panel: {post}");
    assert!(post.contains("Units"), "missing Units panel: {post}");
    assert!(post.contains("Detail"), "missing Detail pane: {post}");
    assert!(
        post.contains("[i]") && post.contains("[s]"),
        "missing help-bar hotkeys: {post}"
    );

    // Live data — hdt.9 live-data binding (P8). Each substring is
    // anchored to a distinct part of the manifest/lockfile fixture
    // so a partial wire-up (e.g. sources rendered but not units, or
    // detail still showing the "(select a unit to see details)"
    // placeholder) fails loudly.
    //
    // Sources panel: the seeded source name AND its URI both have
    // to render — the placeholder "(no sources configured)" must
    // NOT appear.
    assert!(
        post.contains("stevie-skills"),
        "Sources panel missing seeded source name: {post}"
    );
    assert!(
        !post.contains("(no sources configured)"),
        "Sources placeholder still present after live-data binding: {post}"
    );

    // Units table: the unit name parsed out of
    // `gh:stevie/skills@main/skills/commit` is `skills/commit`. The
    // rendered row uses the URI's path component, which the parser
    // emits as `skills/commit`. We assert the trailing segment
    // ("commit") since the column may be width-clipped.
    assert!(
        post.contains("commit"),
        "Units table missing seeded unit name 'commit': {post}"
    );

    // Detail pane: the URI lookup is keyed by `declared_uri` in the
    // lockfile, so the deployed path from the seeded lockfile must
    // appear. The placeholder "(select a unit to see details)" must
    // NOT survive past live binding.
    assert!(
        post.contains("/tmp/tripwire-deployed/claude/skills/commit"),
        "Detail pane missing seeded deployed path: {post}"
    );
    assert!(
        !post.contains("(select a unit to see details)"),
        "Detail placeholder still present after live-data binding: {post}"
    );
}
