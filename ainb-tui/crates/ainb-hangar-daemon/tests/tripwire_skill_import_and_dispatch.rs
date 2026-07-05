//! P6.5 capstone e2e tripwire — skills sync → templates use → issue assign →
//! dispatch materialises the SKILL.md into the per-task env.
//!
//! This is the end-of-phase-6 tripwire: it drives the REAL `ainb` CLI against a
//! REAL `ainb-hangar-daemon` (claim loop ENABLED) over an isolated `$HOME`, and
//! proves the whole skill data-plane lands on disk at dispatch time:
//!
//! ```text
//! seed $HOME/.agents-in-a-box/hangar.db (workspace + claude runtime)
//!   + $AINB_TOOLKIT_SKILLS_DIR (3 fake skills)
//!          │
//!   ainb hangar skills sync            → imports the 3 skills
//!          │
//!   ainb hangar templates use code-reviewer  → creates an agent + attaches
//!          │                                    its bundled skills (commit, …)
//!   ainb hangar issue create --assign <agent_id>  → enqueues a task
//!          │
//!   daemon claim loop  → materialise (P6.4)  → run_claude (FAILS — no binary)
//!          │
//!   $HOME/.agents-in-a-box/hangar/workspaces/default/<task>/.claude/skills/commit/SKILL.md
//! ```
//!
//! The `claude` binary is absent in CI, so `run_claude` fails — that is FINE:
//! the materialiser runs BEFORE the provider exec, so the SKILL.md is written
//! regardless. We assert on the materialised file's bytes (after
//! `trim_end_matches('\n')`, per the insta trailing-newline trap) and let the
//! provider step fail gracefully.
//!
//! ## Form: real CLI + real daemon, not tmux-keystroke-driven
//!
//! Unlike the P4 per-screen TUI tripwires, the P6.5 capstone exercises the
//! *control plane* (CLI verbs → daemon claim loop → on-disk materialisation),
//! which has no keystrokes to route through a pane — the cheaper and more direct
//! proof is to invoke the REAL `ainb` CLI against a REAL daemon and assert the
//! materialised bytes. This is the "direct (non-tmux) dispatch-materialise
//! integration test that drives the same path" the P6.5 plan sanctions as an
//! acceptable form. We still honour the tripwire discipline: SKIP-not-fail when
//! the binaries/tmux are missing, `poll_until` (no bare sleep before a check),
//! and the daemon is killed by its exact pid on drop (never a wildcard).

#![allow(clippy::duration_suboptimal_units)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The two skills the `code-reviewer` template bundles (P6.3). We seed both into
/// the toolkit source so `templates use code-reviewer` resolves them.
const CODE_REVIEWER_SKILLS: &[&str] = &["commit", "find-missing-tests"];

/// A third skill so the sync reports "3 skills" (the template only needs two).
const EXTRA_SKILL: &str = "brainstorm";

/// The skill whose materialised SKILL.md we byte-assert.
const ASSERT_SKILL: &str = "commit";

/// The SKILL.md body for `commit` (the bytes that must reach the per-task env).
/// Distinct, multi-line, with a trailing newline so the trim-then-compare is
/// non-trivial.
const COMMIT_BODY: &str =
    "# Commit skill\n\nCreate well-formatted, atomic commits.\nNever mention AI.\n";

/// `true` when `tmux` is usable.
fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Locate the built `ainb` binary (walk `target/<profile>/ainb`). `None` SKIPs.
fn ainb_bin() -> Option<PathBuf> {
    if let Some(p) = option_env!("CARGO_BIN_EXE_ainb") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?; // deps → profile
    let candidate = profile_dir.join("ainb");
    candidate.exists().then_some(candidate)
}

/// The `ainb-hangar-daemon` binary (always defined for this crate's tests).
fn daemon_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_ainb-hangar-daemon"));
    p.exists().then_some(p)
}

/// Canonical SKIP line (greppable).
fn skip(reason: &str) {
    eprintln!(
        "SKIP: {reason} (need tmux + built `ainb` + `ainb-hangar-daemon`; \
         run `cargo build -p ainb -p ainb-hangar-daemon`)"
    );
}

/// A running daemon child + its isolated `$HOME`. Kills the daemon by its own
/// pid on drop (never a wildcard).
struct DaemonProc {
    child: Child,
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write a fake `<root>/<name>/SKILL.md` with YAML frontmatter + `body`.
fn write_fake_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    let md = format!("---\nname: {name}\ndescription: fake {name} skill\n---\n{body}");
    std::fs::write(dir.join("SKILL.md"), md).expect("write SKILL.md");
}

/// Seed `$HOME/.agents-in-a-box/hangar.db` with a workspace + owner + a claude runtime
/// bound to `runtime_id`. `templates use` requires a runtime to bind agents to,
/// and the daemon claims tasks for this runtime.
fn seed_database(home: &Path, runtime_id: &str) {
    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open seed store");
        let pool = store.pool();
        let now: i64 = 1_700_000_000_000;
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, 'default', 'Default', ?)")
            .bind("01HANGARP65WS0000000000000")
            .bind(now)
            .execute(pool)
            .await
            .expect("seed workspace");
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('user-1', 'stevie@local', ?)")
            .bind(now)
            .execute(pool)
            .await
            .expect("seed user");
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('01HANGARP65WS0000000000000', 'user-1', 'owner')")
            .execute(pool)
            .await
            .expect("seed member");
        sqlx::query(
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
             VALUES (?, '01HANGARP65WS0000000000000', 'daemon-1', 'claude', 'local', ?, 'online')",
        )
        .bind(runtime_id)
        .bind(now)
        .execute(pool)
        .await
        .expect("seed runtime");
    });
}

/// Run an `ainb hangar …` CLI command under the isolated `$HOME`, capturing
/// stdout. Panics on spawn failure (the caller gated on the binaries).
fn run_ainb(ainb: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ainb)
        .args(args)
        .env("HOME", home)
        .env_remove("AINB_HANGAR_HOME")
        .output()
        .expect("run ainb")
}

/// Poll `pred` until it holds or `deadline` passes (200ms gap, no bare sleep
/// before the first check). Returns whether it held.
fn poll_until(deadline: Instant, mut pred: impl FnMut() -> bool) -> bool {
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Seed the skills source + db and spawn the claim-enabled daemon, returning the
/// daemon handle + the skills source dir. Extracted from the test body to keep
/// it under the line budget; the runtime id binds the daemon to the seeded
/// runtime so its claim loop drains the assigned task.
fn setup_pipeline(home: &Path, daemon: &Path) -> (DaemonProc, PathBuf) {
    let skills_src = home.join("skills");
    std::fs::create_dir_all(&skills_src).expect("create skills src");
    // Seed 3 fake skills (the two the template needs + one extra).
    write_fake_skill(&skills_src, ASSERT_SKILL, COMMIT_BODY);
    write_fake_skill(
        &skills_src,
        "find-missing-tests",
        "# Find missing tests\nbody\n",
    );
    write_fake_skill(&skills_src, EXTRA_SKILL, "# Brainstorm\nbody\n");
    assert_eq!(CODE_REVIEWER_SKILLS.len(), 2, "template skill count guard");

    let runtime_id = "runtime-p65";
    seed_database(home, runtime_id);

    // Spawn the daemon with the claim loop ENABLED, bound to our runtime, with a
    // bogus `claude` path so `run_claude` fails AFTER the materialiser runs.
    let daemon_proc = DaemonProc {
        child: Command::new(daemon)
            .env("HOME", home)
            .env_remove("AINB_HANGAR_HOME")
            .env("HANGAR_DAEMON_RUNTIME_ID", runtime_id)
            .env("HANGAR_CLAUDE_PATH", "/nonexistent/claude-binary")
            .env("HANGAR_DAEMON_POLL_MS", "200")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    };

    // Wait for the daemon socket so the claim loop is up.
    let socket = home.join(".agents-in-a-box").join("hangar.sock");
    assert!(
        poll_until(Instant::now() + Duration::from_secs(10), || socket.exists()),
        "daemon never bound its socket"
    );
    (daemon_proc, skills_src)
}

#[test]
fn tripwire_skill_import_and_dispatch_materialises_skill_md() {
    if !tmux_available() {
        skip("tmux not available");
        return;
    }
    let (Some(ainb), Some(daemon)) = (ainb_bin(), daemon_bin()) else {
        skip("ainb or ainb-hangar-daemon binary not built");
        return;
    };

    let home = tempfile::tempdir().expect("isolated HOME");
    let (daemon_proc, skills_src) = setup_pipeline(home.path(), &daemon);

    // 1. skills sync — imports the 3 skills.
    let sync = run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar",
            "skills",
            "sync",
            "--source",
            skills_src.to_str().unwrap(),
        ],
    );
    let sync_out = String::from_utf8_lossy(&sync.stdout);
    assert!(sync.status.success(), "skills sync failed: {sync:?}");
    assert!(
        sync_out.contains("imported 3 skill"),
        "sync did not report 3 skills:\n{sync_out}"
    );

    // 2. templates use code-reviewer — creates an agent + attaches its skills.
    let use_out = run_ainb(
        &ainb,
        home.path(),
        &["hangar", "templates", "use", "code-reviewer"],
    );
    let use_stdout = String::from_utf8_lossy(&use_out.stdout);
    assert!(
        use_out.status.success(),
        "templates use failed: {use_out:?}\n{use_stdout}"
    );
    let agent_id = parse_agent_id(&use_stdout)
        .unwrap_or_else(|| panic!("could not parse agent id from:\n{use_stdout}"));

    // 3. issue create --assign <agent_id> — enqueues a task for the agent.
    let issue = run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar", "issue", "create", "--title", "fix bug", "--assign", &agent_id,
        ],
    );
    let issue_stdout = String::from_utf8_lossy(&issue.stdout);
    assert!(
        issue.status.success(),
        "issue create failed: {issue:?}\n{issue_stdout}"
    );
    assert!(
        issue_stdout.contains("queued task"),
        "issue create did not queue a task:\n{issue_stdout}"
    );

    // 4. Poll for the materialised SKILL.md under the per-task env. The on-disk
    //    layout uses a short id of the task, so we glob the workspaces dir rather
    //    than computing it.
    let workspaces = home
        .path()
        .join(".agents-in-a-box")
        .join("hangar")
        .join("workspaces")
        .join("default");
    let materialised = poll_for_skill_md(&workspaces, ASSERT_SKILL, Duration::from_secs(30));

    let path = materialised.unwrap_or_else(|| {
        panic!(
            "SKILL.md never materialised under {} within 30s",
            workspaces.display()
        )
    });

    // 5. Byte-assert the materialised content matches the source SKILL.md
    //    (trimmed). The importer stores the full file (frontmatter + body) as the
    //    skill content, so the materialised file must equal the source file
    //    verbatim modulo a trailing newline.
    let source_md = skills_src.join(ASSERT_SKILL).join("SKILL.md");
    let want = std::fs::read_to_string(&source_md).expect("read source SKILL.md");
    let got = std::fs::read_to_string(&path).expect("read materialised SKILL.md");
    assert_eq!(
        got.trim_end_matches('\n'),
        want.trim_end_matches('\n'),
        "materialised SKILL.md must byte-match the imported source SKILL.md"
    );
    // Sanity: the asserted bytes carry the distinctive body line (non-vacuous).
    assert!(
        got.contains("Never mention AI."),
        "materialised SKILL.md missing the source body:\n{got}"
    );

    // Cleanup: daemon killed by `DaemonProc::drop` (exact pid, never wildcard).
    drop(daemon_proc);
    drop(home);
}

/// Parse the agent id from `templates use` stdout
/// (`created agent <id> from template …`).
fn parse_agent_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("created agent ") {
            return rest.split_whitespace().next().map(str::to_string);
        }
        if let Some(rest) = line.strip_prefix("agent ") {
            // idempotent path: "agent <id> already exists …"
            return rest.split_whitespace().next().map(str::to_string);
        }
    }
    None
}

/// Poll for `*/.claude/skills/<skill>/SKILL.md` under `workspaces`, returning the
/// first match within `budget`.
fn poll_for_skill_md(workspaces: &Path, skill: &str, budget: Duration) -> Option<PathBuf> {
    let deadline = Instant::now() + budget;
    let mut found = None;
    poll_until(deadline, || {
        found = find_skill_md(workspaces, skill);
        found.is_some()
    });
    found
}

/// Walk the per-task dirs under `workspaces` for `<task>/.claude/skills/<skill>/SKILL.md`.
fn find_skill_md(workspaces: &Path, skill: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(workspaces).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(".claude").join("skills").join(skill).join("SKILL.md");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
