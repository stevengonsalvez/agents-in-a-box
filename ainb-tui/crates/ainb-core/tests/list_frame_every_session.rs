#![allow(missing_docs)]

// ABOUTME: `ainb list --frame` feeds the web dashboard and its attach lookup, so
// it must list every session whatever session filter the TUI persisted (#1180).
// The Shift+F filter is a fact about the TUI's renderer; a user who left it on
// "active only" must still see and attach a stopped session on the web.
//
// Runs the real binary against a scratch HOME holding a persisted
// `session_filter = "active_only"` and one stopped session.

use std::process::Command;

const STOPPED_ID: &str = "6f1d3d64-0f44-4a1e-9a9e-6f2f7e6a1b11";
const STOPPED_TMUX: &str = "ainb_list_frame_stopped_6f1d3d64";

#[test]
fn list_frame_carries_a_stopped_session_with_active_only_persisted() {
    let home = tempfile::tempdir().expect("scratch home");
    let store = home.path().join(".agents-in-a-box");
    let config = store.join("config");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[ui_preferences]\nsession_filter = \"active_only\"\n",
    )
    .expect("persist the TUI filter");
    // A session whose tmux is long gone: `list` reports it stopped.
    std::fs::write(
        store.join("sessions.json"),
        serde_json::json!({
            "sessions": {
                STOPPED_TMUX: {
                    "session_id": STOPPED_ID,
                    "tmux_session_name": STOPPED_TMUX,
                    "worktree_path": home.path().join("stopped-repo"),
                    "workspace_name": "stopped-repo",
                    "created_at": "2026-09-16T10:00:00Z",
                }
            }
        })
        .to_string(),
    )
    .expect("session store");

    let output = Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args(["list", "--frame"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("AINB_HOME", home.path())
        .env("AINB_HANGAR_HOME", &store)
        .output()
        .expect("run ainb list --frame");
    assert!(
        output.status.success(),
        "ainb list --frame failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list --frame prints JSON rows");
    let row = rows
        .as_array()
        .expect("an array of rows")
        .iter()
        .find(|row| row["session_id"] == STOPPED_ID)
        .unwrap_or_else(|| panic!("the stopped session is missing: {rows}"));
    assert_eq!(row["is_running"], false, "{row}");
    // The attach lookup matches on these two values.
    assert_eq!(row["tmux_session_name"], STOPPED_TMUX, "{row}");
}
