//! Recording harness for the MASTER converged-control-center journey
//! (docs/hangar/assets/journeys/, CH0-CH7).
//!
//! NOT a test and NOT shipped — a scratch harness the journey recording
//! scripts drive. Mirrors `seed_boards_journey.rs` / `seed_control_center.rs`:
//! isolated `$HOME`, the P4 fixture, a Boards world, and a profile master
//! file on disk. Unlike those, it also:
//!
//! - writes an on-disk profile master (`journey-runner.md`, tier `fast`) for
//!   the live P5 profile-editor screen to index and preview (CH0);
//! - seeds a "Delivery" board with a Backlog card already assigned to that
//!   profile (CH1) — because the live Boards-screen `c`/`Enter`/`a` keys are
//!   reducer-only right now (see `app_screens.rs::route_boards`; the
//!   `pending_boards_action` drain only lifts `⇧←/→`/`x`/`n`/`m`). This
//!   harness seeds the RESULT those keys would one day produce, exactly as
//!   `seed_boards_journey.rs` already does for its run-to-done state;
//! - lowers `autostandup.stagnant_min` / `.cooldown_min` to 1 so CH6's idle
//!   standup fires inside a recording window instead of the 15/60-minute
//!   production default (`crates/ainb-hangar-daemon/src/standup.rs`);
//! - spawns the daemon with `HANGAR_DAEMON_DISABLE_CLAIM=1` and
//!   `AINB_FLEET_TRANSPORT=tmux-only` — no fake-claude headless runner; the
//!   real-agent chapters spawn a REAL claude session out-of-band via
//!   `ainb run --interactive`, a separate mechanism from the daemon's task
//!   queue, and ASK answers deliver over real tmux, not the broker.
//!
//! Usage: `seed_master_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

const CARD_ISSUE: &str = "HG-77";
const CARD_TITLE: &str = "Journey task: ask me which environment to ship to";
const BOARD_ID: &str = "board-journey";
const PROFILE_SLUG: &str = "journey-runner";

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed_master_journey <HOME> <DAEMON_BIN>"));
    let daemon_bin =
        PathBuf::from(args.next().expect("usage: seed_master_journey <HOME> <DAEMON_BIN>"));

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);
    seed_profile_master(&hangar_dir);

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

        DaemonConfigRepo::set(pool, ainb_hangar_daemon::standup::KEY_STAGNANT_MIN, "1")
            .await
            .expect("lower autostandup.stagnant_min");
        DaemonConfigRepo::set(pool, ainb_hangar_daemon::standup::KEY_COOLDOWN_MIN, "1")
            .await
            .expect("lower autostandup.cooldown_min");

        let agent = ActorRef::new(ActorKind::Agent, PROFILE_SLUG).expect("agent ref");
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

        BoardRepo::create(pool, &ws, BOARD_ID, "Delivery", now).await.expect("create board");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-backlog", "Backlog", None, false)
            .await
            .expect("add Backlog");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-running", "Running", Some("running"), true)
            .await
            .expect("add Running");
        BoardRepo::column_add(pool, &ws, BOARD_ID, "col-done", "Done", Some("done"), true)
            .await
            .expect("add Done");
        BoardRepo::card_add(pool, &ws, BOARD_ID, CARD_ISSUE, Some("col-backlog"), now)
            .await
            .expect("place card in Backlog");
    });
    // store/pool dropped here → the seed connection closes before the daemon opens.

    let mut cmd = Command::new(&daemon_bin);
    cmd.env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .env("AINB_FLEET_TRANSPORT", "tmux-only")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar").join("hangar.sock");
    let alt_socket = hangar_dir.join("hangar.sock");
    wait_for(Duration::from_secs(15), || socket.exists() || alt_socket.exists());
    assert!(
        socket.exists() || alt_socket.exists(),
        "daemon never bound its socket under {}",
        hangar_dir.display()
    );

    println!("HOME={}", home.display());
    println!("DAEMON_PID={}", child.id());
    println!("CARD_ISSUE={CARD_ISSUE}");
    println!("BOARD_ID={BOARD_ID}");
    println!("PROFILE_SLUG={PROFILE_SLUG}");
    // Intentionally do NOT wait on `child` — leave the daemon running.
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Write the CH0 profile master: slug `journey-runner`, tier `fast` (the TUI
/// cycles it to `balanced` live via `t`).
fn seed_profile_master(hangar_dir: &Path) {
    let dir = hangar_dir.join("profiles");
    std::fs::create_dir_all(&dir).expect("create profiles dir");
    let master = "---\n\
name: journey-runner\n\
description: Runs the master control-center journey end to end\n\
model: fast\n\
tools: Read, Grep, Glob\n\
color: cyan\n\
---\n\
You are journey-runner, the agent driving the master control-center journey.\n";
    std::fs::write(dir.join("journey-runner.md"), master).expect("write profile master");
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
