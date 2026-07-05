//! P1.6 per-task execution-environment layout tests.
//!
//! Exercises [`prepare_env`](ainb_hangar_daemon::execenv::prepare_env) /
//! [`cleanup`](ainb_hangar_daemon::execenv::cleanup) /
//! [`short_id`](ainb_hangar_daemon::execenv::short_id) against a real isolated
//! `$HOME` tempdir. Every test stands up its own `TempDir` home (no shared
//! state, no env mutation), so the suite is parallel-safe without `ENV_LOCK`.
//!
//! Layout under test (`build-plan.md:40`, `CLI_AND_DAEMON.md:185-193`):
//! `{home}/.agents-in-a-box/hangar/workspaces/{ws_slug}/{shortID(task.id)}/`
//! with `workdir/`, `output/`, `logs/`, `.gc_meta.json` siblings.

// The epoch-ms arithmetic below casts `Duration::as_millis()` (u128) to `i64`;
// every value is a near-present wall-clock time, well within `i64` range.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::path::Path;

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::execenv::{
    CONTEXT_PROMPT_FILE, CleanupKind, GcMeta, cleanup, prepare_env, short_id, write_context_prompt,
};
use ainb_hangar_store::repo::task::Task;
use tempfile::TempDir;

/// A frozen base "now" for deterministic `.gc_meta.json` timestamps.
const BASE_MS: i64 = 1_700_000_000_000;

/// The workspace slug every test threads into the layout path.
const WS_SLUG: &str = "alpha";

/// Build a fully-materialised [`Task`] with the fields P1.6 reads.
fn task_fixture(id: &str, issue_id: Option<&str>) -> Task {
    Task {
        id: id.to_string(),
        workspace_id: "ws-1".to_string(),
        runtime_id: "rt-1".to_string(),
        agent_id: "ag-1".to_string(),
        issue_id: issue_id.map(str::to_string),
        status: "dispatched".to_string(),
        result: None,
        session_id: None,
        work_dir: None,
        attempt: 1,
        max_attempts: 3,
        parent_task_id: None,
        failure_reason: None,
        priority: 0,
        created_at: BASE_MS,
        dispatched_at: Some(BASE_MS),
        started_at: None,
        finished_at: None,
        autopilot_run_id: None,
        generation: 0,
        mode: "headless".to_string(),
        session_name: None,
        repo_ref: None,
        agent_kind: "claude".to_string(),
        branch: None,
    }
}

/// The expected per-task root under an isolated home.
fn expected_root(home: &Path, slug: &str, task_id: &str) -> std::path::PathBuf {
    home.join(".agents-in-a-box")
        .join("hangar")
        .join("workspaces")
        .join(slug)
        .join(task_id)
}

#[test]
fn prepare_creates_full_env_layout() {
    let home = TempDir::new().expect("tempdir home");
    let clock = FixedClock(BASE_MS);
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));

    let env = prepare_env(&task, WS_SLUG, home.path(), &clock).expect("prepare_env");

    let root = expected_root(home.path(), WS_SLUG, &task.id);
    assert_eq!(env.workdir, root.join("workdir"));
    assert_eq!(env.output, root.join("output"));
    assert_eq!(env.logs, root.join("logs"));
    assert_eq!(env.gc_meta, root.join(".gc_meta.json"));

    assert!(env.workdir.is_dir(), "workdir must exist");
    assert!(env.output.is_dir(), "output must exist");
    assert!(env.logs.is_dir(), "logs must exist");
    assert!(env.gc_meta.is_file(), ".gc_meta.json must exist");
}

#[test]
fn gc_meta_json_contents() {
    let home = TempDir::new().expect("tempdir home");
    let clock = FixedClock(BASE_MS);
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-42"));

    let env = prepare_env(&task, WS_SLUG, home.path(), &clock).expect("prepare_env");

    let raw = std::fs::read_to_string(&env.gc_meta).expect("read .gc_meta.json");
    let meta: GcMeta = serde_json::from_str(&raw).expect("parse .gc_meta.json");

    assert_eq!(meta.task_id, task.id);
    assert_eq!(meta.issue_id.as_deref(), Some("iss-42"));
    assert_eq!(meta.workspace_id, task.workspace_id);
    assert_eq!(meta.created_at, BASE_MS);
    assert_eq!(meta.parent.kind, "issue");
    assert_eq!(meta.parent.id, "iss-42");
}

#[test]
fn short_id_is_ulid_short_form() {
    // First 8 chars PLUS the last 6 (the T4 collision-resistant slug form), so
    // same-instant ULIDs stay distinct. Deterministic.
    assert_eq!(short_id("01HZX0000000000000000ABCDE"), "01HZX0000ABCDE");
    assert_eq!(
        short_id("01HZX0000000000000000ABCDE"),
        short_id("01HZX0000000000000000ABCDE")
    );
    // Shorter-than-8 ids return the whole string (defensive, no panic).
    assert_eq!(short_id("abc"), "abc");
}

#[test]
fn prepare_idempotent_on_existing_dir() {
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));

    let env1 = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("first");
    // Drop a sentinel into workdir; a re-prepare must NOT wipe it.
    let sentinel = env1.workdir.join("keep.txt");
    std::fs::write(&sentinel, b"keep").expect("write sentinel");

    let later = BASE_MS + 5_000;
    let env2 = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(later)).expect("second");

    assert_eq!(env1.workdir, env2.workdir, "reuses same dirs");
    assert!(
        sentinel.is_file(),
        "idempotent prepare must not clobber existing workdir"
    );

    let raw = std::fs::read_to_string(&env2.gc_meta).expect("read meta");
    let meta: GcMeta = serde_json::from_str(&raw).expect("parse meta");
    assert_eq!(
        meta.created_at, BASE_MS,
        "created_at preserved across re-prepare"
    );
    assert_eq!(
        meta.last_seen_at,
        Some(later),
        "last_seen_at updated on re-prepare"
    );
}

#[test]
fn context_prompt_is_written_into_the_workdir_as_claude_md() {
    // e38.21: the per-workspace context prompt must land in the agent's CWD (the
    // execenv workdir) as a `CLAUDE.md`, so the agent run actually sees it — not
    // just live in the database.
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    let written = write_context_prompt(&env, Some("Always run cargo fmt before committing."))
        .expect("write context prompt");

    let claude_md = env.workdir.join(CONTEXT_PROMPT_FILE);
    assert_eq!(
        written.as_deref(),
        Some(claude_md.as_path()),
        "returns the written path"
    );
    assert!(
        claude_md.is_file(),
        "CLAUDE.md must exist in the agent's workdir"
    );
    let body = std::fs::read_to_string(&claude_md).expect("read CLAUDE.md");
    assert_eq!(
        body, "Always run cargo fmt before committing.",
        "the agent's CLAUDE.md carries the workspace context prompt verbatim"
    );
}

#[test]
fn no_context_prompt_writes_no_claude_md() {
    // An unconfigured workspace (None / blank prompt) writes no file — the v1
    // behaviour (the agent runs with no per-workspace context).
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    assert_eq!(write_context_prompt(&env, None).expect("none prompt"), None);
    assert_eq!(
        write_context_prompt(&env, Some("   \n  ")).expect("blank prompt"),
        None,
        "a whitespace-only prompt is treated as unset"
    );
    assert!(
        !env.workdir.join(CONTEXT_PROMPT_FILE).exists(),
        "no CLAUDE.md is written when there is no prompt"
    );
}

#[test]
fn cleanup_full_removes_tree() {
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    let root = expected_root(home.path(), WS_SLUG, &task.id);
    assert!(root.is_dir());

    cleanup(&env, CleanupKind::Full).expect("cleanup full");
    assert!(
        !root.exists(),
        "full cleanup removes the whole per-task tree"
    );
}

#[test]
fn cleanup_artifact_only_keeps_workdir() {
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    // Simulate a git checkout + an emitted artifact + a log line.
    std::fs::create_dir_all(env.workdir.join(".git")).expect("fake .git");
    std::fs::write(env.output.join("patch.diff"), b"diff").expect("artifact");
    std::fs::write(env.logs.join("claude.jsonl"), b"{}").expect("log");

    cleanup(&env, CleanupKind::ArtifactOnly).expect("cleanup artifact-only");

    assert!(!env.output.exists(), "artifact-only removes output/");
    assert!(
        env.workdir.join(".git").is_dir(),
        "artifact-only keeps workdir/.git/"
    );
    assert!(
        env.logs.join("claude.jsonl").is_file(),
        "artifact-only keeps logs"
    );
}

#[test]
fn cleanup_orphan_removes_dir_without_meta() {
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    let root = expected_root(home.path(), WS_SLUG, &task.id);
    // Remove the meta marker → orphan. The orphan scan compares the dir's real
    // filesystem mtime (just created ≈ wall-clock now) against the injected
    // `now_ms`, so place `now_ms` 73h past real now to push the dir over the 72h
    // grace window deterministically (no `filetime` dependency needed).
    std::fs::remove_file(&env.gc_meta).expect("rm meta");

    let real_now_ms = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis() as i64;
    let now_73h_later = real_now_ms + 73 * 3_600 * 1_000;
    cleanup(
        &env,
        CleanupKind::OrphanScan {
            now_ms: now_73h_later,
        },
    )
    .expect("cleanup orphan");

    assert!(
        !root.exists(),
        "orphan (no .gc_meta.json, mtime > 72h) is removed"
    );
}

#[test]
fn cleanup_orphan_keeps_live_marked_dir() {
    let home = TempDir::new().expect("tempdir home");
    let task = task_fixture("01HZX0000000000000000ABCDE", Some("iss-1"));
    let env = prepare_env(&task, WS_SLUG, home.path(), &FixedClock(BASE_MS)).expect("prepare");

    let root = expected_root(home.path(), WS_SLUG, &task.id);
    // Marker present → live dir. Even with `now_ms` far in the future, the scan
    // must leave it alone.
    let far_future = BASE_MS + 1_000 * 365 * 24 * 3_600 * 1_000;
    cleanup(&env, CleanupKind::OrphanScan { now_ms: far_future }).expect("cleanup orphan");

    assert!(
        root.is_dir(),
        "a marked (live) dir is never removed by orphan scan"
    );
}
