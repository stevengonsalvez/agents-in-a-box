//! Recording harness for the control-center vhs journey (tmux-verify G2/G3).
//!
//! NOT a test and NOT shipped — a scratch harness the `control-center-journey`
//! tape drives. It reuses the EXACT seed shape of
//! `tripwire_p2_control_center_attention.rs` / `tripwire_p4_common.rs` (isolated
//! `$HOME`, completed onboarding, dismissed notify prompt, first-run ack, the P4
//! fixture, and a WAIT+ASK attention pair) but seeds an ASK carrying THREE
//! options (staging / prod / canary) so the recording asserts the ①②③ glyphs.
//!
//! Unlike the tripwire's `Pipeline` (which kills the daemon on drop), this seeds
//! the DB, spawns the daemon DETACHED, waits for its socket, prints the `$HOME`
//! it prepared, and EXITS leaving the daemon alive — the vhs tape then launches
//! `ainb tui` under that same `$HOME`.
//!
//! ## Making the answer deliverable (the C1 target)
//!
//! The daemon's answer router ([`crate::answer`]) resolves a LIVE delivery
//! target before it flips a row to `answered` — it refuses to mark an ASK
//! answered if it can't actually deliver the picked option to the blocked agent.
//! For the recording to show the answered flip, the seeded ASK's raising session
//! (`s-deploy`) must therefore be a discoverable + tmux-deliverable live session.
//! We wire that with two env overrides on the spawned daemon:
//!   - `AINB_BIN=<fake-ainb>` — a stub whose `list --format json` reports one
//!     running session with `session_id = "s-deploy"` bound to a REAL tmux
//!     session (created by the recording harness), so `discover_from_ainb`
//!     returns an exact-id match.
//!   - `AINB_FLEET_TRANSPORT=tmux-only` — deliver via tmux send-keys only (the
//!     broker is flaky and irrelevant here).
//! Pressing an option digit then delivers "prod"/etc into that tmux session and
//! the row genuinely flips to `answered`, dropping off the open board.
//!
//! Usage: `seed_control_center <HOME_DIR> <DAEMON_BIN> [FAKE_AINB_BIN]`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed_control_center <HOME> <DAEMON_BIN>"));
    let daemon_bin =
        PathBuf::from(args.next().expect("usage: seed_control_center <HOME> <DAEMON_BIN>"));
    let fake_ainb = args.next();

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);

    // Seed the DB through a connection that CLOSES before the daemon opens its
    // own (mirrors `prepare_pipeline_seeded`'s no-live-race contract).
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

        // Newer WAIT row (idle-at-prompt marker) — ranks BELOW the ASK.
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: "att-wait-1".to_string(),
                session_id: "s-idle".to_string(),
                cwd: "/work/idle".to_string(),
                workspace_id: None,
                kind: AttentionKind::Waiting,
                payload: serde_json::json!({
                    "kind": "WAIT",
                    "context": { "marker": "WAITING: still idle" }
                })
                .to_string(),
                degraded: false,
                created_at: 1_700_000_000_000,
                raise_transcript: None,
                // Waiting routes board-only under the seeded default (tcp T5).
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .expect("seed waiting attention row");

        // Older ASK row with THREE options — floats to the top by urgency rank.
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: "att-ask-1".to_string(),
                session_id: "s-deploy".to_string(),
                cwd: "/work/deploy".to_string(),
                workspace_id: None,
                kind: AttentionKind::AskUserQuestion,
                payload: serde_json::json!({
                    "kind": "ASK",
                    "context": {
                        "question": "Ship to which env?",
                        "options": [
                            { "label": "staging" },
                            { "label": "prod" },
                            { "label": "canary" },
                        ]
                    }
                })
                .to_string(),
                degraded: false,
                created_at: 1_699_999_000_000,
                raise_transcript: None,
                // ASK routes to web+os under the seeded default (tcp T5).
                channels: ainb_hangar_core::channel::ChannelSet::from_channels([
                    ainb_hangar_core::channel::Channel::Web,
                    ainb_hangar_core::channel::Channel::Os,
                ]),
            },
        )
        .await
        .expect("seed ask attention row");
    });
    // store/pool dropped here → the seed connection is closed before the daemon opens.

    // Spawn the daemon DETACHED under the same $HOME (binds hangar.sock). No
    // kill-on-drop: it must outlive this process for the tape's `ainb tui`.
    let mut cmd = Command::new(&daemon_bin);
    cmd.env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(fake) = &fake_ainb {
        // Point the daemon's session discovery at the stub `ainb list` + force
        // tmux-only delivery so the seeded ASK's target (`s-deploy`) resolves to
        // the real tmux session the harness stood up.
        cmd.env("AINB_BIN", fake).env("AINB_FLEET_TRANSPORT", "tmux-only");
    }
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

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
