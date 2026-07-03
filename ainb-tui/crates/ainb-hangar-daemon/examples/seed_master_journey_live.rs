//! Live-agent recording harness for the MASTER converged-control-center
//! journey's CH2+ chapters (docs/hangar/assets/journeys/, real claude).
//!
//! NOT a test and NOT shipped. Unlike `seed_master_journey` (CH0/CH1,
//! deterministic vhs, claim disabled, no real agent), this harness:
//!
//! - isolates ONLY the Hangar data plane via `$AINB_HANGAR_HOME` (a bare
//!   verbatim root — no `.agents-in-a-box` segment, per
//!   `ainb_hangar_core::paths::hangar_home`), so the board/task/issue db never
//!   touches the operator's real `~/.agents-in-a-box`;
//! - does NOT override `$HOME` — the daemon (and anything it spawns) inherits
//!   the REAL operator `$HOME`, so a `claude` child process resolves the
//!   operator's REAL auth (macOS Keychain `Claude Code-credentials` + the
//!   `~/.claude.json` account state) with zero credential copying. A prior
//!   attempt to copy `~/.claude.json` into a fresh isolated `$HOME` still
//!   read "Not logged in" — the login CLI needs more than that file, so this
//!   harness routes auth through the real, already-authenticated `$HOME`
//!   instead of trying to fake it;
//! - spawns the daemon CLAIM-ENABLED (`HANGAR_DAEMON_RUNTIME_ID=runtime-1`,
//!   fast poll) with NO `HANGAR_CLAUDE_PATH` override, so the interactive
//!   `Run ▾` path resolves and execs a REAL `claude` off `$PATH`
//!   (`runner.rs::provider_command` default) inside its own attachable
//!   `tmux_hangar-<task_id>` session (`ainb-hangar-daemon/src/interactive.rs`);
//! - seeds an EMPTY "Delivery" board (`Backlog`/`Running`/`Done`↦`done`) and a
//!   `journey-runner` profile master — the card is created live via the UI,
//!   exactly as CH1 does;
//! - lowers `autostandup.stagnant_min` / `.cooldown_min` to 1 for CH6.
//!
//! # Scope note (why this harness exists separately from `seed_master_journey`)
//!
//! The interactive tmux path spawns a BARE `claude` REPL — `provider_command`
//! returns an EMPTY argv for the `Claude` backend (`runner.rs::claude_spec`),
//! and the daemon never writes the card's issue title into the pane (only a
//! workspace-level `context_prompt` lands as `CLAUDE.md`, which is unset here).
//! So the task prompt is typed into the live pane by the operator (or this
//! harness's driver script) after attaching — the same as a real human would
//! do the first time they attach to a freshly spawned interactive session.
//!
//! Usage: `seed_master_journey_live <AINB_HANGAR_HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

const BOARD_ID: &str = "board-journey-live";
const PROFILE_SLUG: &str = "journey-runner";

fn main() {
    let mut args = std::env::args().skip(1);
    let hangar_home = PathBuf::from(
        args.next().expect("usage: seed_master_journey_live <AINB_HANGAR_HOME> <DAEMON_BIN>"),
    );
    let daemon_bin = PathBuf::from(
        args.next().expect("usage: seed_master_journey_live <AINB_HANGAR_HOME> <DAEMON_BIN>"),
    );

    std::fs::create_dir_all(&hangar_home).expect("create AINB_HANGAR_HOME");
    seed_profile_master(&hangar_home);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_home).await.expect("open seed store");
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
        // No card is pre-seeded — CH2 creates it live via the UI, same as CH1.
    });
    // store/pool dropped here → the seed connection closes before the daemon opens.

    let mut cmd = Command::new(&daemon_bin);
    cmd.env("AINB_HANGAR_HOME", &hangar_home)
        // Deliberately do NOT touch $HOME — inherited verbatim from this
        // process's real environment, so a spawned `claude` child resolves
        // real auth. PATH is likewise inherited verbatim.
        .env("HANGAR_DAEMON_RUNTIME_ID", "runtime-1")
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_home.join("hangar").join("hangar.sock");
    let alt_socket = hangar_home.join("hangar.sock");
    wait_for(Duration::from_secs(15), || socket.exists() || alt_socket.exists());
    assert!(
        socket.exists() || alt_socket.exists(),
        "daemon never bound its socket under {}",
        hangar_home.display()
    );

    println!("AINB_HANGAR_HOME={}", hangar_home.display());
    println!("DAEMON_PID={}", child.id());
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

/// Write the CH2+ profile master: slug `journey-runner`. No `model:` tier
/// override — the profile's assignee resolution only needs the slug + a
/// runnable agent row (seeded by `seed_p4_fixture`) to route to `runtime-1`.
fn seed_profile_master(hangar_home: &Path) {
    let dir = hangar_home.join("profiles");
    std::fs::create_dir_all(&dir).expect("create profiles dir");
    let master = "---\n\
name: journey-runner\n\
description: Runs the master control-center journey end to end\n\
model: balanced\n\
tools: Read, Grep, Glob\n\
color: cyan\n\
---\n\
You are journey-runner, the agent driving the master control-center journey.\n";
    std::fs::write(dir.join("journey-runner.md"), master).expect("write profile master");
}
