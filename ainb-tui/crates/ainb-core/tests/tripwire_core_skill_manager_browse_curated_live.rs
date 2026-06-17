//! Tripwire: the SkillManager `[b]` modal browsing the CURATED (`ainb`)
//! catalog — the toolkit-as-external-provider feature.
//!
//! Drives the real `ainb` binary against the Full-tier sandbox via tmux:
//!   1. `m` → SkillManager screen.
//!   2. `b` → the catalog browse modal opens, defaulting to `ainb curated`.
//!   3. `Enter` on a blank query → the WHOLE curated shelf renders, showing
//!      BOTH an owned entry AND a vetted-external entry (success criterion
//!      #2's "BOTH" check).
//!   4. `Enter` on the selected (top-ranked) hit → installs it through the
//!      existing install flow → the manifest gains the installed source and
//!      the lockfile records the deployed unit.
//!
//! ZERO network: the launch sets `AINB_CATALOG_INDEX_FILE` to a LOCAL index
//! file the test writes, so the curated backend reads it without a socket.
//! The selected entry's `install_uri` points at the sandbox's LOCAL bare
//! remote (`git:file://…`) so the install fetch is a local clone — never
//! GitHub.
//!
//! Mirrors the launch + onboarding-seed + capture-pane pattern of
//! `tripwire_core_skill_manager_browse_live.rs`. Skips when `tmux` is
//! unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::catalog_index::{CatalogIndex, CatalogIndexEntry, CatalogOrigin};
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
/// the home screen (`InstallPrompt::OfferInstall`) that would otherwise
/// intercept our `m` keystroke before it reaches the SkillManager shortcut.
/// Seeding an install record with `prompt_dismissed = true` makes
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

/// Seed a local bare repo holding one new skill `curated-demo-skill`, and
/// return the full unit URI for it (`git:file://…`, so the install fetch is
/// a local clone, never the network).
fn seed_catalog_remote(root: &Path) -> String {
    let bare = root.join("curated-catalog-remote.git");
    git(&["init", "--bare", "-b", "main", bare.to_str().unwrap()], root);

    let work = root.join(".curated-seed-work");
    std::fs::create_dir_all(&work).expect("work dir");
    git(&["init", "-q", "-b", "main"], &work);
    let skill_dir = work.join("skills/curated-demo-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: curated-demo-skill\nkind: skill\n---\nInstalled via the [b] curated browse modal.\n",
    )
    .expect("write SKILL.md");
    git(&["add", "."], &work);
    git(&["commit", "-q", "-m", "seed: curated-demo-skill"], &work);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
    git(&["push", "-q", "origin", "main"], &work);
    std::fs::remove_dir_all(&work).ok();

    format!("git:file://{}@main/skills/curated-demo-skill", bare.display())
}

/// Write a local curated index with two entries: an OWNED entry whose
/// `install_uri` points at the local catalog remote (sorts first → selected
/// by default), and a vetted-EXTERNAL entry (display only — proves the shelf
/// shows BOTH classes). Returns the index file path.
fn write_curated_index(root: &Path, local_install_uri: &str) -> PathBuf {
    let index = CatalogIndex::new(
        "v-tripwire",
        vec![
            CatalogIndexEntry {
                // `aaa…` sorts to the top so it is the default selection.
                name: "aaa-curated-owned".to_string(),
                description: "owned curated demo (local install)".to_string(),
                repo: "stevengonsalvez/agents-in-a-box".to_string(),
                install_uri: local_install_uri.to_string(),
                origin: CatalogOrigin::Owned,
                stars: 0,
                kind: ainb_skill_core::catalog::CatalogEntryKind::Skill,
            },
            CatalogIndexEntry {
                name: "zzz-curated-external".to_string(),
                description: "vetted external curated demo".to_string(),
                repo: "acme/external-skill".to_string(),
                install_uri: "gh:acme/external-skill@main/.claude/skills".to_string(),
                origin: CatalogOrigin::External,
                stars: 0,
                kind: ainb_skill_core::catalog::CatalogEntryKind::Skill,
            },
        ],
    );
    let path = root.join("curated-index.json");
    std::fs::write(&path, index.to_json()).expect("write curated index");
    path
}

/// Write a curated index with ONE command-kind (npx) entry whose install
/// "command" just `touch`es a marker file — a harmless, offline stand-in for
/// `npx skills add …`. Returns `(index_path, marker_path)`. The double-Enter
/// confirm then runs the command and the marker proves it ran.
fn write_command_index(root: &Path) -> (PathBuf, PathBuf) {
    let marker = root.join("cmd-install-marker");
    let index = CatalogIndex::new(
        "v-tripwire",
        vec![CatalogIndexEntry {
            name: "npx-demo-skill".to_string(),
            description: "command-kind demo (runs a shell command)".to_string(),
            repo: "vercel-labs/agent-browser".to_string(),
            install_uri: format!("touch '{}'", marker.display()),
            origin: CatalogOrigin::External,
            stars: 0,
            kind: ainb_skill_core::catalog::CatalogEntryKind::Npx,
        }],
    );
    let path = root.join("command-index.json");
    std::fs::write(&path, index.to_json()).expect("write command index");
    (path, marker)
}

/// Launch line: the fixture's env_vars + `AINB_CATALOG_INDEX_FILE` so the
/// curated backend reads the LOCAL index (offline).
fn launch_line(layout: &SandboxLayout, bin: &Path, index_file: &Path) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push_str(&sh_quote(&Path::new(&v).to_string_lossy()));
        s.push(' ');
    }
    s.push_str("AINB_CATALOG_INDEX_FILE=");
    s.push_str(&sh_quote(&index_file.to_string_lossy()));
    s.push(' ');
    s.push_str("exec ");
    s.push_str(&sh_quote(&bin.to_string_lossy()));
    s.push_str(" 2>&1");
    s
}

/// Journey scaffolding (NOT a test assertion — `#[ignore]` so it never runs
/// in the suite). Materializes a PERSISTENT curated-browse sandbox under
/// `$AINB_JOURNEY_DIR` and prints `export …` lines for the launch env, so a
/// vhs `.tape` (the tmux-verify G2/G3 visual gate) can drive the real binary
/// against the same offline curated shelf the tripwire above asserts.
///
/// Run: `AINB_JOURNEY_DIR=/path cargo test -p ainb \
///   --test tripwire_core_skill_manager_browse_curated_live \
///   materialize_curated_journey_sandbox -- --ignored --nocapture`
#[test]
#[ignore = "journey scaffolding, run on demand for vhs recording"]
fn materialize_curated_journey_sandbox() {
    let dir = std::env::var("AINB_JOURNEY_DIR")
        .expect("set AINB_JOURNEY_DIR to a persistent directory");
    let root = PathBuf::from(&dir);
    std::fs::create_dir_all(&root).expect("create journey dir");
    let layout =
        build_skill_manager_sandbox(&root, SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);
    seed_notify_dismissed(&layout);
    let install_uri = seed_catalog_remote(layout.root.as_path());
    let index_file = write_curated_index(layout.root.as_path(), &install_uri);

    // Emit the launch env so the tape can `export` it before exec-ing ainb.
    for (k, v) in layout.env_vars() {
        println!("export {k}={}", sh_quote(&Path::new(&v).to_string_lossy()));
    }
    println!(
        "export AINB_CATALOG_INDEX_FILE={}",
        sh_quote(&index_file.to_string_lossy())
    );
    println!("export AINB_JOURNEY_HOME={}", sh_quote(&layout.root.to_string_lossy()));
}

#[test]
fn curated_browse_blank_lists_shelf_and_installs_live() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);
    seed_notify_dismissed(&layout);
    let install_uri = seed_catalog_remote(layout.root.as_path());
    let index_file = write_curated_index(layout.root.as_path(), &install_uri);

    let session = format!("tripwire-sm-curated-{}", std::process::id());
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
            &launch_line(&layout, &bin, &index_file),
            "Enter",
        ])
        .status()
        .expect("tmux send-keys launch");

    // Home → SkillManager. The v2 home lists a "Skills (manager)" tile;
    // wait for it as the "booted on home" signal.
    if poll(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Skills (manager)")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("home never rendered:\n{d}");
    }
    thread::sleep(Duration::from_millis(300));
    send(&session, "z");

    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Units") && c.contains("Detail")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("SkillManager never rendered:\n{d}");
    }

    // ── [b] → curated browse modal (defaults to `ainb curated`).
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

    // ── Blank Enter → the whole curated shelf renders BOTH classes.
    thread::sleep(Duration::from_millis(200));
    send(&session, "Enter");
    let shelf = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("aaa-curated-owned") && c.contains("zzz-curated-external")
    });
    if shelf.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "blank Enter did not list the curated shelf with BOTH an owned \
             and an external entry.\nlast pane:\n{d}"
        );
    }

    // ── Enter on the selected (top) owned entry → install from the local
    // remote. Prove it via manifest + lockfile, not just a transient toast.
    thread::sleep(Duration::from_millis(300));
    send(&session, "Enter");

    let manifest = layout.ainb_home.join("manifest.yaml");
    let lock = layout.ainb_home.join("lock.yaml");
    let catalog_marker = "curated-catalog-remote.git";
    let mut installed = false;
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        let manifest_text = std::fs::read_to_string(&manifest).unwrap_or_default();
        let lock_text = std::fs::read_to_string(&lock).unwrap_or_default();
        if manifest_text.contains(catalog_marker)
            && lock_text.contains("curated-demo-skill")
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
        "[b] curated browse → Enter did not install the selected hit — manifest \
         did not gain the `{catalog_marker}` source AND a deployed \
         `curated-demo-skill` unit in lock.yaml.\nlast pane:\n{dump}"
    );
}

#[test]
fn curated_command_kind_install_runs_on_double_enter_live() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);
    seed_notify_dismissed(&layout);
    let (index_file, marker) = write_command_index(layout.root.as_path());

    let session = format!("tripwire-sm-cmd-{}", std::process::id());
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
            &launch_line(&layout, &bin, &index_file),
            "Enter",
        ])
        .status()
        .expect("tmux send-keys launch");

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
    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Units") && c.contains("Detail")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("SkillManager never rendered:\n{d}");
    }

    // [b] → curated modal, blank Enter → the single command-kind row.
    thread::sleep(Duration::from_millis(200));
    send(&session, "b");
    if poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Browse Catalog") && c.contains("ainb curated")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("[b] did not open the curated browse modal");
    }
    thread::sleep(Duration::from_millis(200));
    send(&session, "Enter");
    if poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("npx-demo-skill") && c.contains("npx")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("command-kind row did not render with its npx badge:\n{d}");
    }

    // First Enter ARMS the confirm (must NOT run yet — marker absent).
    thread::sleep(Duration::from_millis(200));
    send(&session, "Enter");
    if poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Enter again to run")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("first Enter did not arm the command confirm:\n{d}");
    }
    assert!(
        !marker.exists(),
        "the command ran on the FIRST Enter — the confirm gate failed"
    );

    // Second Enter RUNS the command → marker created.
    thread::sleep(Duration::from_millis(200));
    send(&session, "Enter");
    let mut ran = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if marker.exists() {
            ran = true;
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }
    let dump = capture(&session);
    kill(&session);
    assert!(
        ran,
        "second Enter did not run the command-kind install (marker absent).\n\
         last pane:\n{dump}"
    );
}
