//! Tripwire: `[s]` keybind on SkillManager Units panel flips the
//! `shadowed_by` relationship between a marketplace plugin and a
//! same-name orphan unit (spec §User Flow 3 + §P7 hdt.8).
//!
//! Three scenarios, one file:
//!
//! 1. **positive** — seed BOTH a marketplace plugin and a same-name
//!    orphan skill. After `[Enter] import all`, the manifest holds two
//!    `commit` units with the orphan active and the marketplace
//!    shadowed. Press `[s]` → manifest flips on disk.
//! 2. **roundtrip** — same seed; press `[s]` twice → manifest is
//!    byte-identical to the post-import baseline (persistence is
//!    deterministic).
//! 3. **negative** — seed only the orphan (no conflict pair). Press
//!    `[s]` → manifest unchanged (no shadowed_by field appears /
//!    disappears).
//!
//! Skips when `tmux` isn't on PATH. Mirrors the discipline of
//! `tripwire_core_skill_manager_discovery_banner.rs` — poll-capture
//! with substring-AND assertions, per-pid session names, isolated
//! `$HOME` tempdir, on-disk manifest as the source of truth.

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

fn poll_file<F>(path: &Path, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        if let Ok(s) = fs::read_to_string(path) {
            if ok(&s) {
                return Some(s);
            }
        }
        thread::sleep(Duration::from_millis(200));
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

/// Seed onboarding marker so the first-run wizard does NOT eat the
/// `m` keystroke before SkillManager nav can see it.
fn seed_onboarding_skip(home: &Path) {
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
}

/// Seed marketplace plugin shipping a `commit` skill AND a same-name
/// orphan `commit` skill in `~/.claude/skills/`. Discovery + import
/// will produce a conflict pair: orphan active, marketplace shadowed.
fn seed_conflict_pair(home: &Path) {
    seed_onboarding_skip(home);

    let claude = home.join(".claude");
    let plugins = claude.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(
        plugins.join("known_marketplaces.json"),
        b"{\"tripwire-mp\": {}}\n",
    )
    .unwrap();

    // class-A: marketplace plugin shipping a skill named `commit`.
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
    let plugin_skill = plugin_root.join("skills").join("commit");
    fs::create_dir_all(&plugin_skill).unwrap();
    fs::write(
        plugin_skill.join("SKILL.md"),
        b"---\nname: commit\nkind: skill\n---\nmarketplace body\n",
    )
    .unwrap();

    // class-C: orphan skill named `commit` in the claude tool home —
    // by spec, the orphan is the active winner and the marketplace
    // copy is shadowed.
    let orphan = claude.join("skills").join("commit");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(
        orphan.join("SKILL.md"),
        b"---\nname: commit\nkind: skill\n---\norphan body\n",
    )
    .unwrap();
}

/// Seed only an orphan skill — no marketplace counterpart, so there
/// is no conflict to flip. Used for the negative scenario.
fn seed_no_conflict(home: &Path) {
    seed_onboarding_skip(home);
    let claude = home.join(".claude");
    let orphan = claude.join("skills").join("solo-skill");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(
        orphan.join("SKILL.md"),
        b"---\nname: solo-skill\nkind: skill\n---\nbody\n",
    )
    .unwrap();
    // known_marketplaces.json must still exist for the class-A
    // walker to no-op cleanly (no entries → no class-A units).
    let plugins = claude.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("known_marketplaces.json"), b"{}\n").unwrap();
}

/// Boot ainb in a tmux session with HOME/AINB_HOME pointed at the
/// seeded tempdir, navigate to SkillManager, import the banner, and
/// return the running session name. The manifest is now on disk at
/// `<home>/.agents-in-a-box/manifest.yaml`.
fn boot_and_import(home: &Path, session: &str) {
    let bin = ainb_bin();
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "200", "-y", "60"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let ainb_home = home.join(".agents-in-a-box");
    let cmd = format!(
        "unset XDG_CONFIG_HOME; \
         HOME={home} AINB_HOME={ainb_home} AINB_USE_REAL_HOMES=1 exec {bin} 2>&1",
        home = home.display(),
        ainb_home = ainb_home.display(),
        bin = bin.display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // HomeScreen must be fully painted before keystrokes.
    let home_render = poll_capture(session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Welcome to AINB")
    });
    if home_render.is_none() {
        let dump = capture_pane(session);
        kill_session(session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }
    thread::sleep(Duration::from_millis(200));

    // Navigate to SkillManager.
    send_key(session, "m");

    // Wait for the discovery banner to paint, then press Enter to import.
    let banner = poll_capture(session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("Detected existing units") && c.contains("[Enter]")
    });
    if banner.is_none() {
        let dump = capture_pane(session);
        kill_session(session);
        panic!("discovery banner never rendered. last capture:\n{dump}");
    }
    send_key(session, "Enter");

    // After Enter, banner dismisses + Units table populates. Wait
    // for the SkillManager view to be steady-state before returning.
    let post_import = poll_capture(session, Instant::now() + Duration::from_secs(60), |c| {
        !c.contains("Detected existing units")
            && c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
    });
    if post_import.is_none() {
        let dump = capture_pane(session);
        kill_session(session);
        panic!("post-import SkillManager view never settled. last capture:\n{dump}");
    }
    thread::sleep(Duration::from_millis(300));
}

#[test]
fn s_flips_marketplace_orphan_conflict_in_manifest() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_conflict_pair(home_tmp.path());

    let session = format!("tripwire-sm-flip-pos-{}", std::process::id());
    boot_and_import(home_tmp.path(), &session);

    let manifest_path = home_tmp
        .path()
        .join(".agents-in-a-box")
        .join("manifest.yaml");

    // Baseline: import produced an orphan-wins shadow.
    let baseline = fs::read_to_string(&manifest_path).expect("manifest.yaml must exist");
    // The orphan row appears as `local:~/.claude/skills@head/commit`
    // and is the ACTIVE side (no shadowed_by line attached). The
    // marketplace row appears as
    // `marketplace:tripwire-plugin@tripwire-mp/skills/commit` and
    // points its shadowed_by at the orphan URI.
    assert!(
        baseline.contains("local:~/.claude/skills@head/commit")
            && baseline.contains("marketplace:tripwire-plugin@tripwire-mp/skills/commit")
            && baseline.contains("shadowed_by: local:~/.claude/skills@head/commit"),
        "baseline manifest missing expected conflict shape:\n{baseline}"
    );
    assert!(
        !baseline.contains("shadowed_by: marketplace:tripwire-plugin@tripwire-mp/skills/commit"),
        "baseline should NOT have orphan shadowed by marketplace yet:\n{baseline}"
    );

    // Press [s] — flip the conflict pair.
    send_key(&session, "s");

    // Poll the manifest until the shadowed_by line points the OTHER
    // way (orphan now shadowed by marketplace). Bounded so a missing
    // handler can't hang the test.
    let flipped = poll_file(&manifest_path, Instant::now() + Duration::from_secs(15), |s| {
        s.contains("shadowed_by: marketplace:tripwire-plugin@tripwire-mp/skills/commit")
            && !s.contains("shadowed_by: local:~/.claude/skills@head/commit")
    });
    let flipped = match flipped {
        Some(s) => s,
        None => {
            let on_disk = fs::read_to_string(&manifest_path).unwrap_or_default();
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "[s] did not flip shadowed_by on disk within 15s.\n\
                 manifest:\n{on_disk}\n\nscreen:\n{dump}"
            );
        }
    };

    // Both units must still be in the manifest — flip swaps a field,
    // it does not delete rows.
    assert!(
        flipped.contains("local:~/.claude/skills@head/commit")
            && flipped.contains("marketplace:tripwire-plugin@tripwire-mp/skills/commit"),
        "flipped manifest missing one of the unit rows:\n{flipped}"
    );

    kill_session(&session);
}

#[test]
fn s_twice_roundtrips_to_original_shadowed_by_state() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_conflict_pair(home_tmp.path());

    let session = format!("tripwire-sm-flip-rt-{}", std::process::id());
    boot_and_import(home_tmp.path(), &session);

    let manifest_path = home_tmp
        .path()
        .join(".agents-in-a-box")
        .join("manifest.yaml");
    let baseline = fs::read_to_string(&manifest_path).expect("manifest.yaml must exist");

    // Press [s] once → flipped.
    send_key(&session, "s");
    let flipped_state = poll_file(&manifest_path, Instant::now() + Duration::from_secs(15), |s| {
        s.contains("shadowed_by: marketplace:tripwire-plugin@tripwire-mp/skills/commit")
    });
    if flipped_state.is_none() {
        let on_disk = fs::read_to_string(&manifest_path).unwrap_or_default();
        kill_session(&session);
        panic!("first [s] did not flip manifest:\n{on_disk}");
    }

    // Press [s] again → must restore the baseline byte-for-byte.
    send_key(&session, "s");
    let restored = poll_file(&manifest_path, Instant::now() + Duration::from_secs(15), |s| {
        s.contains("shadowed_by: local:~/.claude/skills@head/commit")
            && !s.contains("shadowed_by: marketplace:tripwire-plugin@tripwire-mp/skills/commit")
    });
    let restored = match restored {
        Some(s) => s,
        None => {
            let on_disk = fs::read_to_string(&manifest_path).unwrap_or_default();
            kill_session(&session);
            panic!("second [s] did not restore baseline:\n{on_disk}");
        }
    };

    // Byte-identical roundtrip — same YAML serialization on both
    // sides of the flip pair proves the persistence path is
    // deterministic, not just "did something happen to disk".
    assert_eq!(
        restored, baseline,
        "roundtrip should be byte-identical to baseline manifest"
    );

    kill_session(&session);
}

#[test]
fn s_on_unit_without_conflict_is_noop() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_no_conflict(home_tmp.path());

    let session = format!("tripwire-sm-flip-neg-{}", std::process::id());
    boot_and_import(home_tmp.path(), &session);

    let manifest_path = home_tmp
        .path()
        .join(".agents-in-a-box")
        .join("manifest.yaml");
    let baseline = fs::read_to_string(&manifest_path).expect("manifest.yaml must exist");

    // Sanity: the seed has no marketplace pair, so the baseline must
    // not contain any shadowed_by line — otherwise the negative
    // assertion below would pass for the wrong reason.
    assert!(
        !baseline.contains("shadowed_by"),
        "no-conflict seed should produce a manifest with no shadowed_by:\n{baseline}"
    );

    // Press [s]. There is no conflict pair — the handler must leave
    // the manifest untouched.
    send_key(&session, "s");

    // Wait a beat for any (incorrect) write to land before snapshotting.
    thread::sleep(Duration::from_millis(1500));
    let after = fs::read_to_string(&manifest_path).expect("manifest.yaml must still exist");

    assert_eq!(
        after, baseline,
        "no-conflict [s] must NOT modify manifest:\nbefore:\n{baseline}\nafter:\n{after}"
    );

    kill_session(&session);
}
