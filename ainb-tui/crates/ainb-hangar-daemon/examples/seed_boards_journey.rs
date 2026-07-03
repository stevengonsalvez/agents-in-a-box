//! Recording harness for the P4 / D8 Boards vhs journey (tmux-verify G2/G3).
//!
//! NOT a test and NOT shipped — a scratch harness the `boards-journey` tape
//! drives. It stands up the EXACT acceptance chain of
//! `tripwire_board_auto_move_e2e.rs` ("create board w/ custom columns → add card
//! → run → succeeded → card green + auto-moved") but leaves the daemon ALIVE so
//! the tape can launch `ainb tui` against it and READ the rendered outcome:
//!
//! ```text
//!  seed world + issue + BOARD(Backlog + Queued/Running/Failed/Done↦auto-move)
//!  + card(issue → Backlog) + queued task(issue)
//!         │  spawn REAL daemon (claim ON, fake-claude-happy)
//!         ▼
//!  claim → running ─(hook)─▶ card Backlog→Running
//!        → done    ─(hook)─▶ card Running→Done  +  state=done (card-green ✓)
//!         │
//!         ▼  (this harness polls until the card lands in Done, THEN exits)
//!  daemon left alive · tape opens Boards (tab B) · frame shows ✓ card in Done
//! ```
//!
//! Unlike the tripwire's `Pipeline` (kill-on-drop), this seeds the DB, spawns the
//! daemon DETACHED, waits for the auto-move to land, prints the `$HOME`, and EXITS
//! leaving the daemon alive — the tape then launches `ainb tui` under that `$HOME`.
//! The card is green through the REAL render path (daemon DB → `boards_list` RPC
//! → `render_boards`), not a mock.
//!
//! Usage: `seed_boards_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

/// The board card's issue — a fresh issue distinct from the P4 fixture's
/// `issue-1..3`, so the fixture's pre-seeded running `task-1` never interferes.
const CARD_ISSUE: &str = "HG-42";
const CARD_TITLE: &str = "Ship the tables";
const BOARD_ID: &str = "board-delivery";

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed_boards_journey <HOME> <DAEMON_BIN>"));
    let daemon_bin =
        PathBuf::from(args.next().expect("usage: seed_boards_journey <HOME> <DAEMON_BIN>"));

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);

    // A fake `claude` that emits a system+result line and exits 0 — the daemon
    // walks the task queued→running→done, firing the board auto-move hook.
    let fake_claude = write_fake_claude(&hangar_dir);

    // Seed the DB through a connection that CLOSES before the daemon opens its
    // own (mirrors the no-live-race contract of the tripwire's seed).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        ainb_hangar_daemon::seed::seed_p4_fixture(store.pool()).await.expect("seed P4 fixture");
        let pool = store.pool();
        let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");
        let now: i64 = 1_700_000_000_000;

        // The issue the card represents (`card = issue`, D8), assigned to the
        // fixture agent so the enqueued task is claimable.
        let agent = ActorRef::new(ActorKind::Agent, "agent-1").expect("agent ref");
        let creator = ActorRef::new(ActorKind::Member, "user-1").expect("member ref");
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: CARD_ISSUE.into(),
                workspace_id: ainb_hangar_daemon::seed::WS_ID.into(),
                title: CARD_TITLE.into(),
                description: None,
                state: "open".into(),
                assignee: Some(agent),
                creator,
                created_at: now,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
            },
        )
        .await
        .expect("insert card issue");

        // A user-defined five-column board: a manual Backlog + the four FSM-mapped
        // auto-move columns (Queued/Running/Failed/Done). `BoardRepo::create` sets
        // the board-level auto_move flag on, so the D8 hook is armed.
        BoardRepo::create(pool, &ws, BOARD_ID, "Delivery", now).await.expect("create board");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-backlog", "Backlog", None, false)
            .await
            .expect("add Backlog");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-queued", "Queued", Some("queued"), true)
            .await
            .expect("add Queued");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-running", "Running", Some("running"), true)
            .await
            .expect("add Running");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-failed", "Failed", Some("failed"), true)
            .await
            .expect("add Failed");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-done", "Done", Some("done"), true)
            .await
            .expect("add Done");

        // The card starts in Backlog — the run + auto-move hook is what lands it
        // in Done (never pre-placed there).
        BoardRepo::card_add(pool, &ws, BOARD_ID, CARD_ISSUE, Some("col-backlog"), now)
            .await
            .expect("place card in Backlog");

        // Enqueue the run FOR THE CARD'S ISSUE with a REAL wall-clock timestamp —
        // the fixture's `now` (Nov 2023) would look stale to the daemon's startup
        // sweeper, which fails ancient queued tasks before they can run. When the
        // task walks to done the daemon's auto-move hook slides the card into the
        // done-mapped Done column.
        let queued_at = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_millis(),
        )
        .expect("millis fit i64");
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("task-board-journey")
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind("runtime-1")
        .bind("agent-1")
        .bind(CARD_ISSUE)
        .bind(queued_at)
        .execute(pool)
        .await
        .expect("enqueue card task");
    });
    // store/pool dropped here → the seed connection closes before the daemon opens.

    // Spawn the daemon DETACHED under the same $HOME, WITH the claim loop enabled
    // (fake-claude, fast poll) so the enqueued task actually runs to `done`.
    let child = Command::new(&daemon_bin)
        .env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", "runtime-1")
        .env("HANGAR_CLAUDE_PATH", &fake_claude)
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ainb-hangar-daemon");

    // Wait for the socket to appear (the daemon binds it during boot).
    let socket = hangar_dir.join("hangar").join("hangar.sock");
    let alt_socket = hangar_dir.join("hangar.sock");
    wait_for(Duration::from_secs(15), || {
        socket.exists() || alt_socket.exists()
    });
    assert!(
        socket.exists() || alt_socket.exists(),
        "daemon never bound its socket under {}",
        hangar_dir.display()
    );

    // Poll a fresh READ pool (WAL allows concurrent readers) until the auto-move
    // hook has landed the card in the Done column — THEN exit, leaving the daemon
    // alive with a deterministic, already-green board for the tape to render.
    let rt2 = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("poll runtime");
    let landed = rt2.block_on(async {
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(hangar_dir.join("hangar.db"))
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .expect("open read pool");
        let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            if let Ok(boards) = BoardRepo::list(&pool, &ws).await {
                if let Some(card) =
                    boards.iter().flat_map(|b| &b.cards).find(|c| c.issue_id == CARD_ISSUE)
                {
                    if card.column_id.as_deref() == Some("col-done") {
                        return true;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        false
    });
    assert!(
        landed,
        "card never auto-moved to the Done column within 40s"
    );

    println!("HOME={}", home.display());
    println!("DAEMON_PID={}", child.id());
    println!("CARD_LANDED_IN_DONE=true");
    // Intentionally do NOT wait on `child` — leave the daemon running.
}

/// Busy-wait up to `budget` for `cond`, sleeping 50ms between polls.
fn wait_for(budget: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + budget;
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Write a fake `claude` that emits a system + result line and exits 0, so the
/// daemon walks the task to `done`.
fn write_fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("fake-claude.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho '{\"type\":\"system\",\"session_id\":\"board-run-1\"}'\n\
         echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n",
    )
    .expect("write fake-claude");
    let mut perms = std::fs::metadata(&path).expect("stat fake-claude").permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake-claude");
    path
}

/// Write a completed `onboarding.toml` so the wizard is skipped.
fn seed_onboarding(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let version = workspace_version();
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{version}\"\nskipped_dependencies = []\ngit_directories = []\n"
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("write onboarding.toml");
}

/// Read `[workspace.package].version` from the workspace root `Cargo.toml`.
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

/// Pre-dismiss the notifyd first-run install prompt (full `InstallRecord` shape).
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

/// Pre-seed the `first_run` danger-full-access ack so the modal is skipped.
fn seed_first_run_ack(home: &Path) {
    let path = home.join(".agents-in-a-box").join("hangar").join("state.toml");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, "warnings_ack = [\"first_run\"]\n").expect("write state.toml");
}
