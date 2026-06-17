//! Tripwire: discovery banner overlay on first SkillManager open
//! (spec §User Flow 1 + §P5).
//!
//! Boots the live `ainb` binary inside an isolated `$HOME` that has
//! been pre-seeded with one marketplace plugin (class A) and one
//! orphan skill (class C). Drives the binary via tmux:
//!
//! 1. wait for HomeScreen to fully paint (Agents + Catalog +
//!    Welcome) — same poll discipline as the existing
//!    `tripwire_core_skill_manager_screen_opens` so a mid-paint
//!    keystroke can't get swallowed by the boot sequence;
//! 2. press `m` — the §User Flow 1 nav key;
//! 3. assert the banner border + every spec-mandated marker is
//!    rendered (substring-AND, never OR, per project memory
//!    `reference_tmux_ui_tripwire_skill`);
//! 4. press `Enter` — the §User Flow 1 step-5 import action;
//! 5. assert the banner is dismissed AND the imported rows appear
//!    in the Units / Sources panels.
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

/// Seed the per-tool homes the discovery walkers scan, plus the
/// onboarding marker so the first-run wizard doesn't eat our `m`
/// keystroke before the SkillManager nav handler can see it.
///
/// Layout written under `home`:
///
/// ```text
/// <home>/
/// ├── .agents-in-a-box/config/onboarding.toml      # skip wizard
/// └── .claude/
///     ├── plugins/
///     │   ├── known_marketplaces.json              # marketplace=registry-known
///     │   └── cache/
///     │       └── tripwire-mp/
///     │           └── tripwire-plugin/
///     │               └── v0.1.0/
///     │                   ├── .claude-plugin/plugin.json
///     │                   └── skills/
///     │                       └── plugin-skill/SKILL.md
///     └── skills/
///         └── tripwire-orphan/SKILL.md             # class-C orphan
/// ```
fn seed_isolated_home_with_discovery_fixtures(home: &Path) {
    // ---- onboarding wizard skip ---------------------------------
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
    fs::write(cfg.parent().unwrap().join("install.json"), "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n")
        .expect("seed install.json");

    // ---- class-A: one marketplace plugin in the Claude cache ----
    let claude = home.join(".claude");
    let plugins = claude.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(
        plugins.join("known_marketplaces.json"),
        b"{\"tripwire-mp\": {}}\n",
    )
    .unwrap();
    let plugin_root = plugins
        .join("cache")
        .join("tripwire-mp")
        .join("tripwire-plugin")
        .join("v0.1.0");
    fs::create_dir_all(plugin_root.join(".claude-plugin")).unwrap();
    fs::write(
        plugin_root.join(".claude-plugin").join("plugin.json"),
        b"{\"name\": \"tripwire-plugin\"}\n",
    )
    .unwrap();
    let plugin_skill = plugin_root.join("skills").join("plugin-skill");
    fs::create_dir_all(&plugin_skill).unwrap();
    fs::write(
        plugin_skill.join("SKILL.md"),
        b"---\nname: plugin-skill\nkind: skill\n---\nbody\n",
    )
    .unwrap();

    // ---- class-C: one orphan skill in ~/.claude/skills/ ---------
    let orphan = claude.join("skills").join("tripwire-orphan");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(
        orphan.join("SKILL.md"),
        b"---\nname: tripwire-orphan\nkind: skill\n---\nbody\n",
    )
    .unwrap();
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
fn discovery_banner_renders_then_import_populates_units() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home_with_discovery_fixtures(home_tmp.path());

    // Exact per-pid session name — never wildcard-kill.
    let session = format!("tripwire-sm-discovery-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "60"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Env precedence:
    //  - HOME → seeded tempdir (per-tool homes + onboarding marker).
    //  - AINB_USE_REAL_HOMES=1 → class-C walker resolves tool homes
    //    under $HOME/.<tool>/ instead of the managed sandbox.
    //  - AINB_HOME explicitly under the tempdir so the manifest /
    //    skip-marker / cache paths can't leak from the host shell
    //    (default_ainb_home walks $AINB_HOME → $XDG_CONFIG_HOME → $HOME).
    //  - XDG_CONFIG_HOME unset first (inline `unset` then `exec`) so
    //    it can't pre-empt $AINB_HOME / $HOME resolution.
    let ainb_home = home_tmp.path().join(".agents-in-a-box");
    let cmd = format!(
        "unset XDG_CONFIG_HOME; \
         HOME={home} AINB_HOME={ainb_home} AINB_USE_REAL_HOMES=1 exec {bin} 2>&1",
        home = home_tmp.path().display(),
        ainb_home = ainb_home.display(),
        bin = bin.display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to render FULLY before keystroking.
    // Same boot-paint markers as `tripwire_core_skill_manager_screen_opens`.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    if home_render.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    // Settle window — ratatui paints at 4-30 Hz; 200ms guarantees
    // at least 1 idle frame between paint completion + keystroke.
    thread::sleep(Duration::from_millis(200));

    // Press m (lowercase, per the HomeScreen V2 nav arm).
    send_key(&session, "z");

    // Banner-on assertions (substring-AND, all required). Each
    // marker proves a distinct banner element painted; substring-
    // OR would silently pass if only one of them showed up.
    let banner = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Detected existing units")
            && c.contains("Marketplace plugins:")
            && c.contains("Orphan units:")
            && c.contains("Conflicts")
            && c.contains("[Enter]")
            && c.contains("[d]")
            && c.contains("[s]")
    });
    if banner.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("discovery banner never rendered after pressing m. last capture:\n{dump}");
    }

    // Press Enter to import.
    send_key(&session, "Enter");

    // After import the banner dismisses AND the imported entries
    // appear in the manifest-backed view. Three strong tells (all
    // visible after column truncation by the Units table):
    //  - banner gone (no "Detected existing units").
    //  - orphan unit name `tripwire-orphan` in the Units rows.
    //  - marketplace slug `tripwire-mp` in the Sources panel
    //    (visible as `marketplace-tripwire-mp`; the Units row's
    //    plugin slug `tripwire-plugin` gets truncated by the
    //    14-col source column so we can't safely assert it).
    let post_import = poll_capture(&session, Instant::now() + Duration::from_secs(60), |c| {
        !c.contains("Detected existing units")
            && c.contains("tripwire-orphan")
            && c.contains("tripwire-mp")
    });
    let post_import = match post_import {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "after pressing Enter the banner did not dismiss / units did not populate. \
                 last capture:\n{dump}"
            );
        }
    };

    // Confirm we landed on the SkillManager view (Sources / Units
    // / Detail panels), not some other screen that happens to
    // include the strings.
    assert!(
        post_import.contains("Sources")
            && post_import.contains("Units")
            && post_import.contains("Detail"),
        "expected to remain on SkillManager screen post-import:\n{post_import}"
    );

    kill_session(&session);

    // Verify the import landed on disk too — the SkillManager
    // tells the user it imported, but the on-disk manifest is the
    // source of truth the next ainb open will read.
    let manifest_path = home_tmp
        .path()
        .join(".agents-in-a-box")
        .join("manifest.yaml");
    let manifest_yaml =
        fs::read_to_string(&manifest_path).expect("manifest.yaml was not written by import");
    assert!(
        manifest_yaml.contains("tripwire-orphan") && manifest_yaml.contains("tripwire-plugin"),
        "manifest.yaml missing imported units:\n{manifest_yaml}"
    );
}
