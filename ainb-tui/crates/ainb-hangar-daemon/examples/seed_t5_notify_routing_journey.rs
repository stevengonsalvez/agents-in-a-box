//! Recording harness for the T5 (notification routing) journey
//! (docs/hangar/assets/journeys/t5-notify-routing.gif).
//!
//! NOT a test and NOT shipped — mirrors `seed_t1_worktree_journey.rs`, but seeds
//! only the plain P4 fixture (mirrors `tripwire_p4_common::prepare_pipeline`, an
//! RPC-only daemon with the claim loop disabled — no card is run in this journey)
//! so the daemon's `hangar/notify_rules_list` / `hangar/notify_rule_set` RPCs are
//! live for the Settings screen's Notifications grid, and its automatic attention
//! ingest loop (`AttentionIngest::spawn`, `lib.rs`) is tailing the REAL
//! `$HOME/.agents-in-a-box/events.jsonl` (the same file the `Notification` hook
//! appends to).
//!
//! The recording script (`record-t5-notify-routing.sh`) drives the Settings →
//! Notifications grid interactively (flips the `ask_user_question` rule from the
//! seeded default web+os to phone+web), then — while the tape is mid-flight —
//! plants an `AskUserQuestion` transcript + appends a `Notification` hook line to
//! raise a real ASK through the SAME ingest path `tripwire_tcp_notify_routing_e2e.rs`
//! exercises, so the recorded attention card reflects the flipped rule live.
//!
//! Usage: `seed_t5_notify_routing_journey <HOME_DIR> <DAEMON_BIN>`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(
        args.next().expect("usage: seed_t5_notify_routing_journey <HOME> <DAEMON_BIN>"),
    );
    let daemon_bin = PathBuf::from(
        args.next().expect("usage: seed_t5_notify_routing_journey <HOME> <DAEMON_BIN>"),
    );

    let hangar_dir = home.join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(&home);
    seed_notify_prompt_dismissed(&home);
    seed_first_run_ack(&home);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        ainb_hangar_daemon::seed::seed_p4_fixture(store.pool())
            .await
            .expect("seed P4 fixture");
    });
    // store/pool dropped here so the seed connection closes before the daemon opens.

    let mut cmd = Command::new(&daemon_bin);
    cmd.env("HOME", &home)
        .env_remove("AINB_HANGAR_HOME")
        // RPC-only daemon: no card is run in this journey, but the attention
        // ingest + notify-rule RPCs are always live regardless of the claim loop.
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar.sock");
    wait_for(Duration::from_secs(15), || socket.exists());
    assert!(socket.exists(), "daemon never bound its socket under {}", hangar_dir.display());

    println!("HOME={}", home.display());
    println!("DAEMON_PID={}", child.id());
    // Intentionally do NOT wait on `child` — leave the daemon running.
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
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
