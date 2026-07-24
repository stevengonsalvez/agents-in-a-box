//! CLI tripwire: `ainb fleet ask` exposes an exact structured request and
//! `ainb fleet answer` refuses a stale version before any delivery attempt.

use std::path::PathBuf;
use std::process::Command;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

#[test]
fn fleet_cli_lists_structured_question_and_rejects_stale_answer() {
    static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _env_guard = ENV_LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let hangar_home = dir.path().join("hangar");
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);
    let cwd = dir.path().join("project");
    hangar.apply_hook(
        "cli-ask-start",
        "cli-ask-session",
        &cwd,
        "SessionStart",
        serde_json::json!({"source": "hook"}),
        4_000_000_000_001,
    );
    hangar.apply_hook(
        "cli-ask-question",
        "cli-ask-session",
        &cwd,
        "AskUserQuestion",
        serde_json::json!({
            "payload": {
                "tool_use_id": "toolu-cli-ask",
                "tool_input": {
                    "questions": [{
                        "id": "release",
                        "question": "Ship the CLI control?",
                        "options": [{"label": "yes"}, {"label": "no"}]
                    }]
                }
            }
        }),
        4_000_000_000_002,
    );

    let listed = Command::new(ainb_bin())
        .args(["fleet", "ask", "--format", "json"])
        .output()
        .expect("run fleet ask");
    assert!(listed.status.success(), "fleet ask failed: {listed:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("ask JSON");
    let session = &listed["questions"][0];
    assert_eq!(session["session_key"], "claude:cli-ask-session");
    assert_eq!(session["questions"][0]["id"], "release");
    assert_eq!(
        session["questions"][0]["options"],
        serde_json::json!(["yes", "no"])
    );
    assert_eq!(session["answerable"], true);

    let fingerprint = session["request_fingerprint"].as_str().expect("fingerprint");
    let stale = Command::new(ainb_bin())
        .args([
            "fleet",
            "answer",
            "claude:cli-ask-session",
            "--version",
            "999",
            "--fingerprint",
            fingerprint,
            "--answers",
            r#"[{"question_id":"release","selected_options":["yes"]}]"#,
        ])
        .output()
        .expect("run stale fleet answer");
    assert!(
        !stale.status.success(),
        "stale answer unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale session version"),
        "stale answer error must identify version: {stale:?}"
    );
}
