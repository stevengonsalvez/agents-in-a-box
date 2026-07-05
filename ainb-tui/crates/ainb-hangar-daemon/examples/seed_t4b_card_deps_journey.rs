//! Recording harness for the T4b (card-dependency chain) journey
//! (docs/hangar/assets/journeys/t4-card-deps.gif).
//!
//! NOT a test and NOT shipped — mirrors `seed_t1_worktree_journey.rs`, but seeds
//! the exact fixture the tcp T4 card-dependency tripwire
//! (`tests/tripwire_tcp_card_dependency_chain_e2e.rs`, via `tripwire_p4_common.rs`'s
//! `prepare_pipeline_dep_chain`) drives: a real git repo (`testrepo`) in the `@`
//! scan-cache roster, two cards A (`DepBlockerA`) and B (`DepDependentB`) on the
//! `Delivery` board's Todo column, B `depends-on` A with B's auto-run flag ON, and
//! a claim-enabled daemon running a headless fake-claude that BLOCKS every run
//! until the recording script touches `$HOME/interactive-go` — so the recording
//! can hold A's run live before releasing it to finish, observe B auto-launch, and
//! release B in turn.
//!
//! Usage: `seed_t4b_card_deps_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
use ainb_hangar_store::repo::card_parity::CardParityRepo;

const BOARD_ID: &str = "board-run-1";
const TODO_COL: &str = "board-run-todo";
const DONE_COL: &str = "board-run-done";
const PROFILE_SLUG: &str = "claude-agent";
const REPO_NAME: &str = "testrepo";
const DEP_BLOCKER_ISSUE: &str = "issue-dep-a";
const DEP_DEPENDENT_ISSUE: &str = "issue-dep-b";

/// A headless fake-claude that BLOCKS until the recording script touches
/// `$HOME/interactive-go`, then emits a success result and exits 0 — mirrors
/// `tripwire_p4_common::BLOCKING_FAKE_AGENT`. Self-exits after ~30s so a wiring
/// bug can never wedge the recording.
const BLOCKING_FAKE_AGENT: &str = "#!/bin/sh\ni=0\nwhile [ ! -f \"$HOME/interactive-go\" ] && \
     [ \"$i\" -lt 300 ]; do sleep 0.1; i=$((i+1)); done\n\
     echo '{\"type\":\"system\",\"session_id\":\"t4b-journey-sess\"}'\n\
     echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n";

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(
        args.next().expect("usage: seed_t4b_card_deps_journey <HOME> <DAEMON_BIN>"),
    );
    let daemon_bin = PathBuf::from(
        args.next().expect("usage: seed_t4b_card_deps_journey <HOME> <DAEMON_BIN>"),
    );

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);
    seed_profile_master(&hangar_dir);
    seed_scanned_repo(&home, REPO_NAME);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        ainb_hangar_daemon::seed::seed_p4_fixture(store.pool())
            .await
            .expect("seed P4 fixture");
        let pool = store.pool();
        let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("non-empty ws id");
        let now: i64 = 1_700_000_100_000;

        BoardRepo::create(pool, &ws, BOARD_ID, "Delivery", now).await.expect("create board");
        BoardRepo::column_add(pool, &ws, BOARD_ID, TODO_COL, "Todo", None, false)
            .await
            .expect("add Todo column");
        BoardRepo::column_add(pool, &ws, BOARD_ID, DONE_COL, "Done", Some("done"), true)
            .await
            .expect("add auto-move Done column");

        let repo_path = home.join(REPO_NAME).to_string_lossy().into_owned();
        seed_card_issue(pool, &ws, DEP_BLOCKER_ISSUE, "DepBlockerA", &repo_path, now).await;
        seed_card_issue(pool, &ws, DEP_DEPENDENT_ISSUE, "DepDependentB", &repo_path, now + 1).await;
        CardDependencyRepo::add_edge(pool, &ws, DEP_DEPENDENT_ISSUE, DEP_BLOCKER_ISSUE, now)
            .await
            .expect("B depends-on A");
        CardDependencyRepo::set_auto_run(pool, &ws, DEP_DEPENDENT_ISSUE, true)
            .await
            .expect("B auto-run on");

        // Free the fixture's seeded `running` task-1 so agent-1 is claimable.
        sqlx::query("UPDATE agent_task_queue SET status = 'done', finished_at = created_at WHERE id = 'task-1'")
            .execute(pool)
            .await
            .expect("free fixture running task-1");
    });
    // store/pool dropped here so the seed connection closes before the daemon opens.

    let fake_claude = write_executable(&home, "fake-claude.sh", BLOCKING_FAKE_AGENT);

    let mut cmd = Command::new(&daemon_bin);
    cmd.env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", "runtime-1")
        .env("HANGAR_CLAUDE_PATH", fake_claude.to_str().expect("utf8 fake-claude path"))
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .env("HANGAR_DAEMON_DISABLE_SANDBOX", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar.sock");
    wait_for(Duration::from_secs(15), || socket.exists());
    assert!(socket.exists(), "daemon never bound its socket under {}", hangar_dir.display());

    println!("HOME={}", home.display());
    println!("DAEMON_PID={}", child.id());
    println!("BOARD_ID={BOARD_ID}");
    println!("PROFILE_SLUG={PROFILE_SLUG}");
    println!("REPO_NAME={REPO_NAME}");
    // Intentionally do NOT wait on `child` — leave the daemon running.
}

async fn seed_card_issue(pool: &sqlx::SqlitePool, ws: &WorkspaceId, id: &str, title: &str, repo_ref: &str, now: i64) {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, 'member', 'user-1', ?)",
    )
    .bind(id)
    .bind(ainb_hangar_daemon::seed::WS_ID)
    .bind(title)
    .bind(now)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed card issue {id}: {e}"));
    BoardRepo::card_add(pool, ws, BOARD_ID, id, Some(TODO_COL), now)
        .await
        .unwrap_or_else(|e| panic!("place card {id}: {e}"));
    CardParityRepo::set_issue_repo_agent(pool, ainb_hangar_daemon::seed::WS_ID, id, Some(repo_ref), None)
        .await
        .unwrap_or_else(|e| panic!("set repo on card {id}: {e}"));
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Seed a real git repo at `$HOME/<name>` (one commit) + a `cache/repositories.json`
/// scan cache pointing at it. Mirrors `tripwire_p4_common::seed_scanned_repo`.
fn seed_scanned_repo(home: &Path, name: &str) {
    let repo = home.join(name);
    std::fs::create_dir_all(&repo).expect("create scanned repo dir");
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "t@e.com"],
        &["config", "user.name", "t"],
    ] {
        let ok = Command::new("git").args(args).current_dir(&repo).status();
        assert!(ok.is_ok_and(|s| s.success()), "git {args:?} in the scanned repo");
    }
    std::fs::write(repo.join("README.md"), "seed").expect("write scanned repo README");
    for args in [&["add", "."][..], &["commit", "--quiet", "-m", "seed"]] {
        let ok = Command::new("git").args(args).current_dir(&repo).status();
        assert!(ok.is_ok_and(|s| s.success()), "git {args:?} in the scanned repo");
    }

    let cache_dir = home.join(".agents-in-a-box").join("cache");
    std::fs::create_dir_all(&cache_dir).expect("create scan-cache dir");
    let json = format!(
        "{{\"version\":1,\"repositories\":[{{\"path\":\"{}\",\"name\":\"{name}\"}}]}}",
        repo.display()
    );
    std::fs::write(cache_dir.join("repositories.json"), json).expect("write scan cache");
}

/// Write the `claude-agent` assignee profile master.
fn seed_profile_master(hangar_dir: &Path) {
    let dir = hangar_dir.join("profiles");
    std::fs::create_dir_all(&dir).expect("create profiles dir");
    std::fs::write(
        dir.join(format!("{PROFILE_SLUG}.md")),
        "---\nname: claude-agent\ndescription: Board card runner\nmodel: balanced\n---\nRun the card's issue.\n",
    )
    .expect("write assignee profile master");
}

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write executable script");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

fn seed_onboarding(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let version = workspace_version();
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{version}\"\nskipped_dependencies = []\ngit_directories = []\n"
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("write onboarding.toml");
}

fn workspace_version() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root_cargo = manifest_dir.parent().and_then(Path::parent).map(|p| p.join("Cargo.toml"));
    if let Some(path) = root_cargo {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut in_pkg = false;
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_pkg = t == "[workspace.package]";
                    continue;
                }
                if in_pkg {
                    if let Some(rest) = t.strip_prefix("version") {
                        if let Some(v) = rest.split('"').nth(1) {
                            return v.to_string();
                        }
                    }
                }
            }
        }
    }
    "1.0.0".to_string()
}

fn seed_notify_prompt_dismissed(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let _ = std::fs::create_dir_all(&base);
    std::fs::write(
        base.join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"claude_plugin_dir\":null,\
         \"codex_hooks_json\":null,\"plugin_version\":null,\"prompt_dismissed\":true}\n",
    )
    .expect("write install.json");
}

fn seed_first_run_ack(home: &Path) {
    let path = home.join(".agents-in-a-box").join("hangar").join("state.toml");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, "warnings_ack = [\"first_run\"]\n").expect("write state.toml");
}
