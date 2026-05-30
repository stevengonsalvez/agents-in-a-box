//! P6.4 — dispatch-time skill materialisation tests.
//!
//! Seven behavioural assertions over [`materialise_for_agent`]: the agent's
//! attached skills are copied to disk in each provider's expected layout,
//! supporting files land at their relative paths, `scripts/` files get the unix
//! executable bit, an empty skill set is a no-op, and — the load-bearing
//! invariant — home-style provider layouts write *outside* the git worktree so
//! the agent's `git status` stays clean.
//!
//! Each test opens a fresh `Store` in a `tempfile::tempdir()` (parallel-safe; no
//! `$HOME`/process-env reliance), seeds one workspace + one agent, attaches the
//! skills under test, then materialises into a per-task tree that mirrors the
//! real `ExecEnv` layout (`{task_root}/workdir/` is the git worktree;
//! `task_root` is its sibling-parent).

use std::path::{Path, PathBuf};

use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_core::skill::SkillFileInput;
use ainb_hangar_daemon::materialise::{MaterialiseTarget, materialise_for_agent};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::skill::SkillRepo;
use sqlx::SqlitePool;

const AGENT_ID: &str = "ag-test-0001";

/// Seed one workspace + one agent (+ its runtime/owner rows) and return the
/// typed ids the materialiser keys off. `PRAGMA foreign_keys` is off in the
/// store crate, but we seed the parent rows anyway for fidelity.
async fn seed(pool: &SqlitePool) -> (WorkspaceId, AgentId) {
    let ws = "ws-test-0001";
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, 'test', 'Test', 0)")
        .bind(ws)
        .execute(pool)
        .await
        .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1', 'a@b.c', 0)")
        .execute(pool)
        .await
        .expect("seed user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1', ?, 'd-1', 'claude', 'local', 'online')",
    )
    .bind(ws)
    .execute(pool)
    .await
    .expect("seed runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, 'Tester', 'rt-1', 'workspace', 'u-1')",
    )
    .bind(AGENT_ID)
    .bind(ws)
    .execute(pool)
    .await
    .expect("seed agent");

    (
        WorkspaceId::from_str(ws).unwrap(),
        AgentId::from_str(AGENT_ID).unwrap(),
    )
}

/// Create one skill (body + ordered files) and attach it to the agent.
async fn attach_skill(
    pool: &SqlitePool,
    ws: &WorkspaceId,
    agent: &AgentId,
    name: &str,
    body: Option<&str>,
    files: Vec<SkillFileInput>,
) {
    let id = SkillRepo::create(pool, ws, name, None, body, files)
        .await
        .expect("create skill");
    SkillRepo::attach_to_agent(pool, agent, &id).await.expect("attach skill");
}

/// Build a per-task target whose `task_root` is the sibling-parent of a
/// `workdir/` git worktree, exactly like `ExecEnv` does at runtime. Returns
/// `(target, task_root)`.
fn target_in(home: &Path, provider: &str) -> (MaterialiseTarget, PathBuf) {
    let task_root = home.join("workspaces").join("test").join("01abcdef");
    let workdir = task_root.join("workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    (
        MaterialiseTarget {
            task_root: task_root.clone(),
            workdir,
            provider: provider.to_string(),
        },
        task_root,
    )
}

/// Open a fresh isolated store plus a separate tempdir for the per-task env
/// tree. Both `TempDir`s are returned so they outlive the test body (dropping a
/// `TempDir` deletes its directory, which would unlink the sqlite db).
async fn fresh() -> (Store, tempfile::TempDir, tempfile::TempDir) {
    let db_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(db_dir.path()).await.unwrap();
    (store, home, db_dir)
}

// 1. claude → `{task_root}/.claude/skills/commit/SKILL.md`.
#[tokio::test]
async fn claude_materialises_skill_under_dot_claude() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(pool, &ws, &agent, "commit", Some("commit body"), vec![]).await;

    let (target, task_root) = target_in(home.path(), "claude");
    let report = materialise_for_agent(pool, &agent, &target).await.unwrap();

    let md = task_root.join(".claude/skills/commit/SKILL.md");
    assert!(
        md.exists(),
        "claude SKILL.md must exist at {}",
        md.display()
    );
    assert_eq!(std::fs::read_to_string(&md).unwrap(), "commit body");
    assert_eq!(report.skill_names, vec!["commit".to_string()]);
    assert_eq!(
        report.home_env,
        Some(("CLAUDE_HOME".to_string(), task_root))
    );
}

// 2. codex → `{task_root}/.codex/skills/...` (per-task CODEX_HOME).
#[tokio::test]
async fn codex_materialises_under_dot_codex_with_codex_home() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(pool, &ws, &agent, "commit", Some("body"), vec![]).await;

    let (target, task_root) = target_in(home.path(), "codex");
    let report = materialise_for_agent(pool, &agent, &target).await.unwrap();

    assert!(task_root.join(".codex/skills/commit/SKILL.md").exists());
    assert_eq!(
        report.home_env,
        Some(("CODEX_HOME".to_string(), task_root.join(".codex")))
    );
}

// 3. gemini / default → `{workdir}/.agent_context/skills/...`, no home env.
#[tokio::test]
async fn gemini_and_default_materialise_under_agent_context() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(pool, &ws, &agent, "commit", Some("body"), vec![]).await;

    for provider in ["gemini", "totally-unknown-provider"] {
        let (target, _) = target_in(&home.path().join(provider), provider);
        let report = materialise_for_agent(pool, &agent, &target).await.unwrap();
        assert!(
            target.workdir.join(".agent_context/skills/commit/SKILL.md").exists(),
            "{provider}: skill must land under workdir/.agent_context"
        );
        assert_eq!(report.home_env, None, "{provider}: no home env var");
    }
}

// 4. nested files (`references/x.md` + `scripts/y.sh`) at correct relative paths.
#[tokio::test]
async fn nested_files_land_at_relative_paths() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(
        pool,
        &ws,
        &agent,
        "tool",
        Some("body"),
        vec![
            SkillFileInput::new("references/x.md", "ref x"),
            SkillFileInput::new("scripts/y.sh", "#!/bin/sh\necho hi\n"),
        ],
    )
    .await;

    let (target, task_root) = target_in(home.path(), "claude");
    materialise_for_agent(pool, &agent, &target).await.unwrap();

    let base = task_root.join(".claude/skills/tool");
    assert_eq!(
        std::fs::read_to_string(base.join("references/x.md")).unwrap(),
        "ref x"
    );
    assert!(base.join("scripts/y.sh").exists());
}

// 5. scripts under `scripts/` get 0o755 on unix.
#[cfg(unix)]
#[tokio::test]
async fn scripts_get_executable_bit() {
    use std::os::unix::fs::PermissionsExt;
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(
        pool,
        &ws,
        &agent,
        "tool",
        None,
        vec![
            SkillFileInput::new("scripts/run.sh", "#!/bin/sh\n"),
            SkillFileInput::new("references/note.md", "note"),
        ],
    )
    .await;

    let (target, task_root) = target_in(home.path(), "claude");
    materialise_for_agent(pool, &agent, &target).await.unwrap();

    let base = task_root.join(".claude/skills/tool");
    let script_mode = std::fs::metadata(base.join("scripts/run.sh")).unwrap().permissions().mode();
    let ref_mode = std::fs::metadata(base.join("references/note.md")).unwrap().permissions().mode();
    assert_eq!(
        script_mode & 0o777,
        0o755,
        "scripts/ file must be executable"
    );
    assert_ne!(
        ref_mode & 0o111,
        0o111,
        "non-script file must NOT be executable"
    );
}

// 6. empty agent_skill set → no dirs, dispatch still succeeds.
#[tokio::test]
async fn empty_skill_set_is_a_noop() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (_ws, agent) = seed(pool).await; // no skills attached

    let (target, task_root) = target_in(home.path(), "claude");
    let report = materialise_for_agent(pool, &agent, &target).await.unwrap();

    assert!(
        !task_root.join(".claude").exists(),
        "no skill dirs for empty set"
    );
    assert_eq!(report.files_written, 0);
    assert_eq!(report.total_bytes, 0);
    assert!(report.skill_names.is_empty());
    assert_eq!(
        report.home_env, None,
        "no env pointer when nothing materialised"
    );
}

// 7b. a skill file path that escapes the skill dir (`../`) is skipped, not
//     written outside the materialisation root.
#[tokio::test]
async fn escaping_file_path_is_skipped() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(
        pool,
        &ws,
        &agent,
        "tool",
        Some("body"),
        vec![
            SkillFileInput::new("../escape.md", "should not be written"),
            SkillFileInput::new("references/ok.md", "ok"),
        ],
    )
    .await;

    let (target, task_root) = target_in(home.path(), "claude");
    let report = materialise_for_agent(pool, &agent, &target).await.unwrap();

    let base = task_root.join(".claude/skills/tool");
    assert!(
        base.join("references/ok.md").exists(),
        "safe file still written"
    );
    assert!(
        !task_root.join(".claude/skills/escape.md").exists(),
        "escaping path must not write outside the skill dir"
    );
    // body + one safe file = 2 (the escaping file was skipped).
    assert_eq!(report.files_written, 2);
}

// 7. skill files land OUTSIDE the worktree git root → `git status` stays clean.
#[tokio::test]
async fn materialised_skills_do_not_pollute_git_worktree() {
    let (store, home, _db) = fresh().await;
    let pool = store.pool();
    let (ws, agent) = seed(pool).await;
    attach_skill(pool, &ws, &agent, "commit", Some("body"), vec![]).await;

    let (target, task_root) = target_in(home.path(), "claude");

    // Make `workdir` a real git repo with a clean tree.
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&target.workdir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@e.com"]);
    git(&["config", "user.name", "T"]);
    std::fs::write(target.workdir.join("README.md"), b"hi").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "init"]);

    materialise_for_agent(pool, &agent, &target).await.unwrap();

    // The skills landed under task_root (sibling of workdir), not inside it.
    assert!(task_root.join(".claude/skills/commit/SKILL.md").exists());
    assert!(
        !target.workdir.join(".claude").exists(),
        "no .claude written inside the git worktree"
    );

    let status = git(&["status", "--porcelain"]);
    let dirty = String::from_utf8_lossy(&status.stdout);
    assert!(
        dirty.trim().is_empty(),
        "git worktree must stay clean after materialisation; got:\n{dirty}"
    );
}
