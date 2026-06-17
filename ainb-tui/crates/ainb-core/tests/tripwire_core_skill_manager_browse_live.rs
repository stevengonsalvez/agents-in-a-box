//! Tripwire: the SkillManager `[b]` catalog browse modal — bead ai-a20.
//!
//! Drives the real `ainb` binary against the Full-tier sandbox via tmux:
//!   1. `m` → SkillManager screen.
//!   2. `b` → the catalog browse modal opens (Query mode).
//!   3. type a query → `Enter` → results render (the canned mock hit
//!      `catalog-demo-skill`).
//!   4. `Enter` on the selected hit → installs it through the existing
//!      install flow → the manifest gains the installed source.
//!
//! ZERO network: the launch sets `AINB_CATALOG_MOCK=1` so the binary's
//! `SkillsShHttpBackend` returns canned data without a socket, and
//! `AINB_CATALOG_MOCK_INSTALL_URI` points the top hit's install URI at
//! the sandbox's LOCAL bare remote (`git:file://…`) so the install's
//! fetch is a local clone — never GitHub.
//!
//! Mirrors the EXACT launch + onboarding-seed + capture-pane pattern of
//! `tripwire_core_skill_manager_library_live.rs` /
//! `tripwire_core_skill_manager_keys_wired.rs`. Build the binary under
//! the real HOME; exec with HOME set to the sandbox tempdir. Skips
//! cleanly when `tmux` is unavailable.

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

/// Seed the onboarding marker so the first-run wizard doesn't steal our
/// keystrokes. Same pattern as every other live tmux tripwire.
fn seed_onboarding(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
}

/// Suppress the first-run ainb-hooks install nudge — a confirmation modal on
/// the home screen that would otherwise intercept our `m` keystroke. Seeding
/// an install record with `prompt_dismissed = true` makes
/// `ainb_plugin_notifyd::prompt_state` return `None`.
fn seed_notify_dismissed(layout: &SandboxLayout) {
    let base = layout.root.join(".agents-in-a-box");
    std::fs::create_dir_all(&base).expect("create .agents-in-a-box dir");
    std::fs::write(
        base.join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n",
    )
    .expect("seed install.json");
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

/// Send a literal string (no key-name interpretation).
fn send_literal(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send-keys -l");
}

fn kill(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_AUTHOR_NAME", "ainb-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "ainb-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Seed a SECOND local bare repo (distinct from the fixture's `sandbox`
/// remote) holding one new skill `browse-demo-skill`, and return the
/// full unit URI for it. This represents installing something the
/// manifest does NOT already declare — the genuine browse-then-install
/// flow. The repo is local (`git:file://…`) so the install fetch never
/// hits the network.
fn seed_catalog_remote(root: &Path) -> String {
    let bare = root.join("browse-catalog-remote.git");
    git(&["init", "--bare", "-b", "main", bare.to_str().unwrap()], root);

    let work = root.join(".browse-seed-work");
    std::fs::create_dir_all(&work).expect("work dir");
    git(&["init", "-q", "-b", "main"], &work);
    let skill_dir = work.join("skills/browse-demo-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: browse-demo-skill\nkind: skill\n---\nInstalled via the [b] browse modal.\n",
    )
    .expect("write SKILL.md");
    git(&["add", "."], &work);
    git(&["commit", "-q", "-m", "seed: browse-demo-skill"], &work);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
    git(&["push", "-q", "origin", "main"], &work);
    std::fs::remove_dir_all(&work).ok();

    format!("git:file://{}@main/skills/browse-demo-skill", bare.display())
}

/// Launch line: the fixture's env_vars + the two catalog-mock env knobs
/// that keep the browse + install flow fully offline. `install_uri`
/// points the top hit at the local catalog remote.
fn launch_line(layout: &SandboxLayout, bin: &Path, install_uri: &str) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push_str(&sh_quote(&Path::new(&v).to_string_lossy()));
        s.push(' ');
    }
    // Force canned catalog data (no network) and point the top hit's
    // install URI at the local catalog remote so the install fetch is a
    // local clone.
    s.push_str("AINB_CATALOG_MOCK=1 ");
    s.push_str("AINB_CATALOG_MOCK_INSTALL_URI=");
    s.push_str(&sh_quote(install_uri));
    s.push(' ');
    s.push_str("exec ");
    s.push_str(&sh_quote(&bin.to_string_lossy()));
    s.push_str(" 2>&1");
    s
}

#[test]
fn browse_modal_searches_and_enter_installs_live() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    // Full tier seeds a manifest + the bare remote with `initial-skill`.
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);
    seed_notify_dismissed(&layout);
    // A SECOND local repo holding a NEW skill — the thing the browse
    // modal installs (not already in the manifest). Local → no network.
    let install_uri = seed_catalog_remote(layout.root.as_path());

    let session = format!("tripwire-sm-browse-{}", std::process::id());
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
        .args([
            "send-keys",
            "-t",
            &session,
            &launch_line(&layout, &bin, &install_uri),
            "Enter",
        ])
        .status()
        .expect("tmux send-keys launch");

    // Home → SkillManager. The v2 home lists a "Skills (manager)" tile;
    // wait for it as the "booted on home" signal (the old "Skills (manager)"
    // banner was dropped in the home redesign).
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

    // SkillManager renders (Full tier → units present, no banner).
    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Units") && c.contains("Detail")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("SkillManager never rendered:\n{d}");
    }

    // ── [b] → browse modal. Defaults to the curated (`ainb`) catalog…
    thread::sleep(Duration::from_millis(200));
    send(&session, "b");
    if poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Browse Catalog") && c.contains("ainb curated")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("[b] did not open the curated browse modal:\n{d}");
    }

    // ── …Tab switches to the skills.sh catalog (this test's mock path).
    thread::sleep(Duration::from_millis(200));
    send(&session, "Tab");
    if poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Browse Catalog") && c.contains("skills.sh")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("Tab did not switch the browse modal to skills.sh:\n{d}");
    }

    // ── Type a query + Enter → results render the canned hit.
    send_literal(&session, "demo");
    thread::sleep(Duration::from_millis(400));
    send(&session, "Enter");
    let results = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("catalog-demo-skill")
    });
    if results.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "browse search did not render the canned mock hit `catalog-demo-skill`.\n\
             last pane:\n{d}"
        );
    }

    // ── Enter on the selected (top-ranked) result → install.
    thread::sleep(Duration::from_millis(300));
    send(&session, "Enter");

    // The manifest must gain the installed source (the NEW catalog
    // remote, distinct from the Full-tier `sandbox` source), and the
    // lockfile must record the deployed `browse-demo-skill` unit. Poll
    // both files directly — that's the durable proof the install ran
    // end-to-end, not just a transient toast.
    let manifest = layout.ainb_home.join("manifest.yaml");
    let lock = layout.ainb_home.join("lock.yaml");
    let catalog_marker = "browse-catalog-remote.git";
    let mut installed = false;
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        let manifest_text = std::fs::read_to_string(&manifest).unwrap_or_default();
        let lock_text = std::fs::read_to_string(&lock).unwrap_or_default();
        // Manifest gains the new catalog source; lockfile records the
        // deployed new skill.
        if manifest_text.contains(catalog_marker)
            && lock_text.contains("browse-demo-skill")
            && lock_text.contains("deployed")
        {
            installed = true;
            break;
        }
        thread::sleep(Duration::from_millis(400));
    }
    let dump = capture(&session);
    kill(&session);
    assert!(
        installed,
        "[b] browse → Enter did not install the selected hit — manifest did \
         not gain the `{catalog_marker}` source AND a deployed \
         `browse-demo-skill` unit in lock.yaml.\nlast pane:\n{dump}"
    );
}
