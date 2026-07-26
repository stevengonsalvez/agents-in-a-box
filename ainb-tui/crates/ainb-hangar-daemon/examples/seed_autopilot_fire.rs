//! Recording harness for the S3 "autopilot fire-now" vhs journey
//! (`docs/hangar/verify-converged-goal.md` journey catalogue).
//!
//! NOT a test and NOT shipped — a scratch harness the `s3-autopilot-firenow`
//! tape drives. Unlike `seed_control_center` / `seed_master_journey` (which
//! reuse `seed_p4_fixture`'s full tenancy — issues + a PRE-EXISTING running
//! task), this seeds a deliberately MINIMAL fixture (workspace + user +
//! runtime + agent, mirroring `tripwire_autopilot_fires_on_schedule.rs`'s
//! `seed_parents`): the P4 fixture's baggage task would get claimed the moment
//! the claim loop is armed below, polluting the recording with an unrelated
//! run + an unrelated first-provider-use warning. The only row of interest is
//! ONE cron-scheduled autopilot ("nightly-report", `0 9 * * *`) whose next
//! tick is hours in the future — the real scheduler must NEVER auto-fire it
//! inside the recording window, so the only way a run appears is the operator
//! pressing `r` (fire-now) in the Autopilots manager.
//!
//! ## Making the fired run actually complete
//!
//! Unlike `seed_control_center` (which spawns the daemon with
//! `HANGAR_DAEMON_DISABLE_CLAIM=1` — attention-only, no dispatch), this
//! journey needs the fired task to walk the REAL claim → dispatch → finalize
//! path so the manager's STATUS/LAST RUN columns and the Usage/run-history
//! screen pick up a genuine completed run. We wire that with:
//!   - `HANGAR_DAEMON_RUNTIME_ID=runtime-1` — arms the claim loop for the
//!     seeded runtime (unset, the claim loop is a no-op).
//!   - `HANGAR_CLAUDE_PATH=<fake-claude.sh>` — a stub provider that emits a
//!     `system` line (pins `session_id`) then a `result` line and exits 0, so
//!     the dispatched task reaches `done` in well under a second.
//!
//! Usage: `seed_autopilot_fire <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};
use ainb_hangar_store::repo::autopilot::{
    AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};

/// Minimal tenancy ids (mirrors `tripwire_autopilot_fires_on_schedule.rs`'s
/// `seed_parents`), but slugged `default` so the plugin's fixed
/// `DEFAULT_WORKSPACE_ID` subscribe resolves this workspace.
const WS_ID: &str = "ws-autopilot-fire";
const WS_SLUG: &str = "default";
const USER_ID: &str = "user-1";
const RUNTIME_ID: &str = "runtime-1";
const AGENT_ID: &str = "agent-1";

/// The seeded autopilot's name + cron, exactly as the manager screen renders
/// them (mirrors `tripwire_autopilots_screen_renders.rs`'s naming style).
const AUTOPILOT_NAME: &str = "nightly-report";
const AUTOPILOT_CRON: &str = "0 9 * * *";

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed_autopilot_fire <HOME> <DAEMON_BIN>"));
    let daemon_bin =
        PathBuf::from(args.next().expect("usage: seed_autopilot_fire <HOME> <DAEMON_BIN>"));

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);

    let fake_claude = write_fake_claude(&home);

    // Seed the DB through a connection that CLOSES before the daemon opens its
    // own (mirrors `prepare_pipeline_seeded` / `seed_control_center`'s
    // no-live-race contract).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    let autopilot_id = rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        let pool = store.pool();
        seed_minimal_tenancy(pool).await;

        let ws = WorkspaceId::from_str(WS_ID).expect("valid ws id");
        let agent = AgentId::from_str(AGENT_ID).expect("valid agent id");
        // SystemClock (real wall time), NOT a fixed historical instant: the
        // LIVE scheduler thread evaluates `next_tick_at` against real now, so a
        // stale clock here would compute a next tick already in the past and
        // the autopilot would auto-fire the instant the daemon boots — before
        // the operator ever presses `r`.
        AutopilotRepo::create(
            pool,
            &SystemClock,
            &NewAutopilot {
                workspace_id: ws,
                agent_id: agent,
                name: AUTOPILOT_NAME.to_string(),
                instructions: Some("summarise the day's runs".to_string()),
                cron_expr: AUTOPILOT_CRON.to_string(),
                max_concurrent_runs: 1,
                execution_mode: ExecutionMode::default(),
                concurrency_policy: ConcurrencyPolicy::default(),
            },
        )
        .await
        .expect("create autopilot")
    });
    // store/pool dropped here → the seed connection is closed before the daemon opens.

    println!("AUTOPILOT_ID={}", autopilot_id.as_str());

    // Spawn the daemon DETACHED under the same $HOME (binds hangar.sock), claim
    // loop ENABLED against the fake provider. No kill-on-drop: it must outlive
    // this process for the tape's `ainb tui`.
    let child = Command::new(&daemon_bin)
        .env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", RUNTIME_ID)
        .env("HANGAR_CLAUDE_PATH", &fake_claude)
        .env("HANGAR_DAEMON_POLL_MS", "200")
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

/// Seed the minimal tenancy the fired task's claim/dispatch/finalize path
/// needs: one workspace (slug `default`, the plugin's fixed subscribe target),
/// one user, one `claude`-provider runtime, one agent bound to it. Deliberately
/// NOT `seed_p4_fixture` — no issues, no pre-existing running task, so the
/// fired autopilot's run is the ONLY activity the recording shows.
async fn seed_minimal_tenancy(pool: &sqlx::SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS_ID)
        .bind(WS_SLUG)
        .bind("Default")
        .bind(1_700_000_000_000_i64)
        .execute(pool)
        .await
        .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(USER_ID)
        .bind("operator@example.com")
        .bind(1_700_000_000_000_i64)
        .execute(pool)
        .await
        .expect("seed user");
    AgentRuntimeRepo::insert(
        pool,
        &AgentRuntime {
            id: RUNTIME_ID.to_string(),
            workspace_id: WS_ID.to_string(),
            daemon_id: "daemon-1".to_string(),
            provider: "claude".to_string(),
            runtime_mode: "local".to_string(),
            last_seen_at: Some(1_700_000_000_000),
            status: "online".to_string(),
        },
    )
    .await
    .expect("seed runtime");
    AgentRepo::insert(
        pool,
        &Agent {
            id: AGENT_ID.to_string(),
            workspace_id: WS_ID.to_string(),
            name: "claude-agent".to_string(),
            runtime_id: RUNTIME_ID.to_string(),
            instructions: None,
            visibility: "workspace".to_string(),
            permission_mode: "private".to_string(),
            owner_id: USER_ID.to_string(),
            ..Agent::default()
        },
    )
    .await
    .expect("seed agent");
}

/// Write an executable fake `claude` that emits a `system` line (pinning
/// `session_id`) then a `result` line, and exits 0 — the P1.7 tripwire's
/// `fake_claude_happy` shape, inlined here so this harness has no test-only
/// dependency.
fn write_fake_claude(home: &Path) -> PathBuf {
    let path = home.join("fake-claude.sh");
    let body = "#!/bin/sh\n\
                echo '{\"type\":\"system\",\"session_id\":\"autopilot-fire-1\"}'\n\
                echo '{\"type\":\"result\",\"content\":\"ok\"}'\n\
                exit 0\n";
    std::fs::write(&path, body).expect("write fake-claude.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&path).expect("stat fake-claude").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).expect("chmod fake-claude");
    }
    path
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
