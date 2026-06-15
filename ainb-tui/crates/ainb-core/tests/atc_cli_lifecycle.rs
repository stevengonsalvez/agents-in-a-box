//! End-to-end lifecycle test for `ainb fleet atc`.
//!
//! Drives the real `ainb` binary (via `CARGO_BIN_EXE_ainb`) against a
//! tempdir-backed `AINB_HOME`, using `--no-spawn --no-heartbeat` so the test
//! touches no real launchd/systemd timer and spawns no Claude session. It
//! proves the provisioning lifecycle and its idempotency:
//!
//! * `setup` writes CLAUDE.md + meta.json + seeded state/task-log.
//! * `status --format json` reports the instance.
//! * `list --format json` includes it.
//! * `setup` again is idempotent and does not clobber accumulated state.json.
//! * `teardown --purge` removes the instance dir.
//! * `teardown` again on an absent instance is a clean no-op.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn run(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(ainb_bin())
        .env("AINB_HOME", home)
        // Point the heartbeat/spawn binary resolution back at this same binary.
        .env("AINB_BIN", ainb_bin())
        .args(args)
        .output()
        .expect("invoke ainb")
}

#[test]
fn atc_setup_status_list_teardown_lifecycle() {
    let home = std::env::temp_dir().join(format!("atc-cli-{}", std::process::id()));
    let atc_dir = home.join("atc").join("tower");

    // --- setup (no spawn, no heartbeat → no OS side effects) ---
    let out = run(
        &home,
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "setup",
            "tower",
            "--interval",
            "10",
            "--idle-pause",
            "45",
            "--no-spawn",
            "--no-heartbeat",
        ],
    );
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(atc_dir.join("CLAUDE.md").is_file(), "CLAUDE.md not written");
    assert!(atc_dir.join("meta.json").is_file(), "meta.json not written");
    assert!(
        atc_dir.join("state.json").is_file(),
        "state.json not seeded"
    );
    assert!(
        atc_dir.join("task-log.md").is_file(),
        "task-log.md not seeded"
    );

    // CLAUDE.md carries the custom cadence + the instance name.
    let policy = std::fs::read_to_string(atc_dir.join("CLAUDE.md")).unwrap();
    assert!(policy.contains("# ATC — Air Traffic Control (tower)"));
    assert!(policy.contains("every 10 minutes"));
    assert!(policy.contains("after 45 minutes"));

    // setup JSON reports the knobs and that no session/timer was created.
    let setup_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(setup_json["heartbeat_interval_min"], 10);
    assert_eq!(setup_json["idle_pause_min"], 45);
    assert_eq!(setup_json["heartbeat_enabled"], false);
    assert_eq!(setup_json["session_spawned"], false);

    // --- mutate state.json, then re-setup → idempotent, state preserved ---
    std::fs::write(atc_dir.join("state.json"), r#"{"sentinel":true}"#).unwrap();
    let out = run(
        &home,
        &[
            "fleet",
            "atc",
            "setup",
            "tower",
            "--no-spawn",
            "--no-heartbeat",
        ],
    );
    assert!(out.status.success(), "re-setup failed");
    let state = std::fs::read_to_string(atc_dir.join("state.json")).unwrap();
    assert!(
        state.contains("sentinel"),
        "re-setup clobbered accumulated state.json"
    );

    // --- status ---
    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "status", "tower"],
    );
    assert!(out.status.success(), "status failed");
    let status_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_json["name"], "tower");
    assert_eq!(status_json["tmux_session"], "tmux_tower");

    // --- list ---
    let out = run(&home, &["--format", "json", "fleet", "atc", "list"]);
    assert!(out.status.success(), "list failed");
    let list_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = list_json["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"tower"), "list missing tower: {names:?}");

    // --- teardown --purge → dir gone ---
    let out = run(&home, &["fleet", "atc", "teardown", "tower", "--purge"]);
    assert!(out.status.success(), "teardown failed");
    assert!(!atc_dir.exists(), "teardown --purge left the dir behind");

    // --- teardown again → clean no-op (idempotent) ---
    let out = run(&home, &["fleet", "atc", "teardown", "tower", "--purge"]);
    assert!(
        out.status.success(),
        "second teardown should be a clean no-op"
    );

    // status on the now-absent instance fails cleanly.
    let out = run(&home, &["fleet", "atc", "status", "tower"]);
    assert!(
        !out.status.success(),
        "status on a torn-down instance should fail"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The heartbeat verb is poll-mode: it shells `ainb fleet needs`, builds the
/// nudge, and (when the fleet is quiet long enough) idle-pauses. Here we point
/// `AINB_BIN` at a fake that returns an empty needs list, pre-age the
/// `last_active_ms` past the idle-pause window, and assert the heartbeat
/// reports `idle_paused: true` and stamps `last_heartbeat_ms`. No tmux/session
/// is involved (the session is not live → no send), so this is hermetic.
#[test]
fn atc_heartbeat_idle_pauses_when_fleet_quiet() {
    let home = std::env::temp_dir().join(format!("atc-hb-{}", std::process::id()));
    let atc_dir = home.join("atc").join("ctl");
    std::fs::create_dir_all(&atc_dir).unwrap();

    // A fake `ainb` that returns an empty needs JSON array for the
    // `--format json fleet needs --no-enrich` call the heartbeat makes.
    let fake = home.join("fake-ainb.sh");
    std::fs::write(&fake, "#!/bin/sh\necho '[]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake, perms).unwrap();
    }

    // Provision files only (no real timer, no spawn), then pre-age state so the
    // idle-pause window (default 60m) has elapsed.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "fleet",
            "atc",
            "setup",
            "ctl",
            "--no-spawn",
            "--no-heartbeat",
        ])
        .output()
        .expect("setup");
    assert!(out.status.success(), "setup failed");

    let long_ago = 0i64; // epoch → guaranteed older than any idle-pause window
    std::fs::write(
        atc_dir.join("state.json"),
        format!(r#"{{"last_active_ms":{long_ago}}}"#),
    )
    .unwrap();

    // Fire the heartbeat with the fake ainb as the needs source.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_BIN", &fake)
        .args(["--format", "json", "fleet", "atc", "heartbeat", "ctl"])
        .output()
        .expect("heartbeat");
    assert!(
        out.status.success(),
        "heartbeat failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hb: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hb["needs_count"], 0, "fake returned empty needs");
    assert_eq!(hb["idle_paused"], true, "should idle-pause on quiet fleet");
    assert_eq!(hb["delivered"], false, "paused → nothing delivered");

    // state.json was updated with a fresh last_heartbeat_ms.
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(atc_dir.join("state.json")).unwrap())
            .unwrap();
    assert!(
        state["last_heartbeat_ms"].as_i64().unwrap() > 0,
        "heartbeat did not stamp last_heartbeat_ms"
    );

    let _ = std::fs::remove_dir_all(&home);
}
