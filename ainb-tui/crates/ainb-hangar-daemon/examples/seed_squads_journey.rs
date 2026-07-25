//! Recording harness for the P7 / D17 Squads vhs journey (tmux-verify G2/G3).
//!
//! NOT a test and NOT shipped — a scratch harness the `squads-journey` tape
//! drives. It reuses the isolated-`$HOME` seed shape of `seed_control_center`
//! (completed onboarding, dismissed notify prompt, first-run ack, the P4 fixture)
//! and then seeds the P7 team primitive: a workspace-scoped squad `shippers` led
//! by an AGENT, with two AGENT members and one HUMAN member, each agent on its
//! own online runtime so `hangar/agents_list` resolves live presence + display
//! names for the squad screen.
//!
//! Unlike a tripwire's `Pipeline` (which kills the daemon on drop), this seeds the
//! DB, spawns the daemon DETACHED, waits for its socket, prints the `$HOME` it
//! prepared, and EXITS leaving the daemon alive — the vhs tape then launches
//! `ainb tui` under that same `$HOME`, opens the Squads screen (`S`), and presses
//! `x` to fan the current issue across the squad (leader brief + parallel member
//! dispatch), surfacing the green "briefed <leader> + N members" note.
//!
//! The leader (`agent-2`) + members (`agent-3`, `agent-4`) are FRESH agents with no
//! in-flight work, so fanning out on the seeded `issue-1` (whose only task belongs
//! to the unrelated `agent-1`) never collides on the per-(issue, agent) guard —
//! the fan-out genuinely succeeds through the real daemon dispatch path.
//!
//! Usage: `seed_squads_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_daemon::seed::{self, WS_ID};
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};
use ainb_hangar_store::repo::squad::SquadRepo;

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed_squads_journey <HOME> <DAEMON_BIN>"));
    let daemon_bin =
        PathBuf::from(args.next().expect("usage: seed_squads_journey <HOME> <DAEMON_BIN>"));

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);

    // Seed the DB through a connection that CLOSES before the daemon opens its
    // own (mirrors `seed_control_center`'s no-live-race contract).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        seed::seed_p4_fixture(store.pool()).await.expect("seed P4 fixture");
        let pool = store.pool();

        // A leader agent + two member agents, each on its own online runtime (a
        // distinct provider so the (workspace, daemon, provider) index never
        // collides). Fresh agents with no in-flight work → fan-out never conflicts.
        seed_agent(pool, "agent-2", "runtime-2", "lead-bot", "codex").await;
        seed_agent(pool, "agent-3", "runtime-3", "worker-bot", "gemini").await;
        seed_agent(pool, "agent-4", "runtime-4", "deploy-bot", "cursor").await;

        // The squad: `shippers`, led by agent-2, with two agent members + one
        // human member (the human is shown but skipped by the fan-out).
        let ws = WorkspaceId::from_str(WS_ID).expect("workspace id");
        let agent = |id: &str| ActorRef::new(ActorKind::Agent, id).expect("agent ref");
        SquadRepo::create(
            pool,
            &ws,
            "squad-1",
            "shippers",
            &agent("agent-2"),
            1_700_000_000_000,
        )
        .await
        .expect("create squad");
        SquadRepo::add_member(pool, &ws, "squad-1", &agent("agent-3"))
            .await
            .expect("add agent-3");
        SquadRepo::add_member(pool, &ws, "squad-1", &agent("agent-4"))
            .await
            .expect("add agent-4");
        SquadRepo::add_member(
            pool,
            &ws,
            "squad-1",
            &ActorRef::new(ActorKind::Member, "user-1").expect("member ref"),
        )
        .await
        .expect("add human member");
    });
    // store/pool dropped here → the seed connection is closed before the daemon opens.

    // Spawn the daemon DETACHED under the same $HOME (binds hangar.sock). No
    // kill-on-drop: it must outlive this process for the tape's `ainb tui`.
    let child = Command::new(&daemon_bin)
        .env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ainb-hangar-daemon");

    // Wait for the socket to appear (the daemon binds it during boot).
    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        socket.exists(),
        "daemon never bound its socket at {}",
        socket.display()
    );

    println!("HOME={}", home.display());
    println!("DAEMON_PID={}", child.id());
    // Intentionally do NOT wait on `child` — leave the daemon running.
}

/// Seed one online `agent_runtime` + `agent` pair (`agent_id` on `runtime_id`),
/// the runtime carrying a distinct `provider` so two runtimes in one workspace do
/// not collide on the `(workspace_id, daemon_id, provider)` index. `name` is the
/// agent's display name (what the Squads screen renders).
async fn seed_agent(
    pool: &sqlx::SqlitePool,
    agent_id: &str,
    runtime_id: &str,
    name: &str,
    provider: &str,
) {
    AgentRuntimeRepo::insert(
        pool,
        &AgentRuntime {
            id: runtime_id.into(),
            workspace_id: WS_ID.into(),
            daemon_id: "daemon-1".into(),
            provider: provider.into(),
            runtime_mode: "local".into(),
            last_seen_at: Some(1_700_000_000_000),
            status: "online".into(),
        },
    )
    .await
    .expect("insert runtime");
    AgentRepo::insert(
        pool,
        &Agent {
            id: agent_id.into(),
            workspace_id: WS_ID.into(),
            name: name.into(),
            runtime_id: runtime_id.into(),
            instructions: None,
            visibility: "workspace".into(),
            permission_mode: "private".into(),
            owner_id: "user-1".into(),
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
            provider: None,
            token_budget: None,
        },
    )
    .await
    .expect("insert agent");
}

/// Write a completed `onboarding.toml` under the isolated `$HOME` so the wizard
/// is skipped. Uses the workspace `[workspace.package].version` (the `ainb`
/// major), NOT this crate's `0.1.0`.
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

/// Pre-dismiss the notifyd first-run install prompt (the full `InstallRecord`
/// shape — a partial one fails to deserialize and the modal re-appears).
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
