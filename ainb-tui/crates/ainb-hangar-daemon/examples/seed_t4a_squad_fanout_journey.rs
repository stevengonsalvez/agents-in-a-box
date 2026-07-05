//! Recording harness for the T4a (squad-card fan-out) journey
//! (docs/hangar/assets/journeys/t4-squad-fanout.gif).
//!
//! NOT a test and NOT shipped — mirrors `seed_t1_worktree_journey.rs`, but seeds
//! the exact fixture the tcp T4 squad-fanout tripwire
//! (`tests/tripwire_tcp_squad_card_fanout_e2e.rs`, via `tripwire_p4_common.rs`'s
//! `prepare_pipeline_squad_card`) drives: a real git repo (`testrepo`) in the `@`
//! scan-cache roster, a `shippers` squad (leader `agent-1` + two members
//! `agent-m1`/`agent-m2`), a card ALREADY assigned to that squad and placed on the
//! `Delivery` board's Todo column, and a claim-enabled daemon running a headless
//! fake-claude that BLOCKS every fanned run until the recording script touches
//! `$HOME/interactive-go` — so the recording can hold all three fanned runs (leader
//! + two members, each on its own `ainb/<slug>` worktree) live at once before
//! releasing them to finish and tear down.
//!
//! Usage: `seed_t4a_squad_fanout_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::card_parity::CardParityRepo;
use ainb_hangar_store::repo::squad::SquadRepo;

const BOARD_ID: &str = "board-run-1";
const TODO_COL: &str = "board-run-todo";
const DONE_COL: &str = "board-run-done";
const PROFILE_SLUG: &str = "claude-agent";
const REPO_NAME: &str = "testrepo";
const SQUAD_ID: &str = "squad-t4";
const SQUAD_CARD_ISSUE: &str = "issue-squad-card";

/// A headless fake-claude that BLOCKS until the recording script touches
/// `$HOME/interactive-go`, then emits a success result and exits 0 — mirrors
/// `tripwire_p4_common::BLOCKING_FAKE_AGENT`. Self-exits after ~30s so a wiring
/// bug can never wedge the recording.
const BLOCKING_FAKE_AGENT: &str = "#!/bin/sh\ni=0\nwhile [ ! -f \"$HOME/interactive-go\" ] && \
     [ \"$i\" -lt 300 ]; do sleep 0.1; i=$((i+1)); done\n\
     echo '{\"type\":\"system\",\"session_id\":\"t4a-journey-sess\"}'\n\
     echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n";

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(
        args.next().expect("usage: seed_t4a_squad_fanout_journey <HOME> <DAEMON_BIN>"),
    );
    let daemon_bin = PathBuf::from(
        args.next().expect("usage: seed_t4a_squad_fanout_journey <HOME> <DAEMON_BIN>"),
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

        // Two extra member agents bound to the fixture's `runtime-1`.
        seed_extra_agent(pool, "agent-m1", "member-one").await;
        seed_extra_agent(pool, "agent-m2", "member-two").await;

        // The squad: leader `agent-1` (the fixture's seeded agent) + two members.
        let leader = actor_ref("agent-1");
        SquadRepo::create(pool, &ws, SQUAD_ID, "shippers", &leader, now).await.expect("create squad");
        SquadRepo::add_member(pool, &ws, SQUAD_ID, &actor_ref("agent-m1")).await.expect("add m1");
        SquadRepo::add_member(pool, &ws, SQUAD_ID, &actor_ref("agent-m2")).await.expect("add m2");

        // The card: assigned to the squad, repo set, placed on the board's Todo col.
        let repo_path = home.join(REPO_NAME).to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, 'member', 'user-1', ?)",
        )
        .bind(SQUAD_CARD_ISSUE)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind("SquadFanoutCard")
        .bind(now)
        .execute(pool)
        .await
        .expect("seed squad card issue");
        BoardRepo::card_add(pool, &ws, BOARD_ID, SQUAD_CARD_ISSUE, Some(TODO_COL), now)
            .await
            .expect("place squad card");
        CardParityRepo::set_issue_repo_agent(pool, ainb_hangar_daemon::seed::WS_ID, SQUAD_CARD_ISSUE, Some(&repo_path), None)
            .await
            .expect("set repo on squad card");
        CardParityRepo::set_issue_squad(pool, &ws, SQUAD_CARD_ISSUE, Some(SQUAD_ID))
            .await
            .expect("assign squad to card");

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

fn actor_ref(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Agent, id).expect("valid agent ref")
}

async fn seed_extra_agent(pool: &sqlx::SqlitePool, id: &str, name: &str) {
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, 'runtime-1', 'workspace', 'user-1')",
    )
    .bind(id)
    .bind(ainb_hangar_daemon::seed::WS_ID)
    .bind(name)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed extra agent {id}: {e}"));
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Seed a real git repo at `$HOME/<name>` (one commit) + a `cache/repositories.json`
/// scan cache pointing at it, so the daemon's `hangar/repo_list` offers it in the
/// card-create `@` roster with a local checkout path. Mirrors
/// `tripwire_p4_common::seed_scanned_repo`.
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
