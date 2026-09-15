#![allow(missing_docs)]

// ABOUTME: Pins the web dashboard's session source to `ainb list --frame`
// (issue #1056). The stub `ainb` answers `list` with the operator's own rows,
// label included, and `list --frame` with the redacted rows, so a data source
// that falls back to plain `list` puts the label on `/api/snapshot` and fails.
//
// Its own test binary: it points `AINB_BIN`, `HOME` and `AINB_HANGAR_HOME` at a
// scratch directory, which no other test in this process may race.

use ainb_web::data::{AinbCliSource, DataSource};
use std::os::unix::fs::PermissionsExt;

const CANARY: &str = "ghp_ProofCanary0123456789abcdefghijklmnopq";

fn stub_ainb(dir: &std::path::Path) -> std::path::PathBuf {
    let row = r#""session_id":"95312768-43d4-4a9e-af9a-337e0c57a95d","tmux_session_name":"tmux_repo-95312768","workspace_name":"repo","worktree_name":"repo","created_at":"2026-09-15T00:31:48Z","is_running":true,"claude_active":false"#;
    let script = format!(
        "#!/bin/sh\n\
         case \" $* \" in\n\
         *\" list --frame \"*) echo '[{{{row}}}]' ;;\n\
         *\" list \"*) echo '[{{{row},\"display_name\":\"deploy {CANARY}\"}}]' ;;\n\
         *) echo null ;;\n\
         esac\n"
    );
    let path = dir.join("ainb");
    std::fs::write(&path, script).expect("write the stub ainb");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[tokio::test]
async fn the_web_snapshot_reads_sessions_from_list_frame_not_the_operator_list() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let bin = stub_ainb(scratch.path());
    std::env::set_var("AINB_BIN", &bin);
    // No daemon here: needs degrade to empty instead of dialling a real one.
    std::env::set_var("HOME", scratch.path());
    std::env::set_var("AINB_HANGAR_HOME", scratch.path());

    let snapshot = AinbCliSource::new().snapshot().await.expect("snapshot");
    let body = serde_json::to_string(&snapshot).expect("snapshot serialises");

    assert_eq!(
        snapshot.sessions.as_array().map(Vec::len),
        Some(1),
        "{body}"
    );
    assert!(body.contains("tmux_repo-95312768"), "{body}");
    assert!(
        !body.contains("display_name"),
        "the session source is not list --frame: {body}"
    );
    assert!(!body.contains(CANARY), "{body}");
}
