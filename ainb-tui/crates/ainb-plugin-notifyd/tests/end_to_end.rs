//! End-to-end integration test for the notifyd pipeline.
//!
//! Spins up the real `ainb-notifyd` accept loop in a temporary
//! directory, fires the real `plugins/ainb-hooks/hooks/notify.sh`
//! bash script against the socket with both a Claude-shaped (stdin
//! JSON) payload and a Codex-shaped (argv JSON) payload, and asserts
//! that two rows with the correct `agent` + `raw_event` end up in
//! `notifications.db`.
//!
//! This is the wiring proof referenced in the spec's success
//! criteria #1: "real claude / codex session produces a row in
//! notifications.db".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ainb_plugin_notifyd::{Paths, RetentionPolicy, RunConfig, Store, run_daemon};

fn hook_script() -> PathBuf {
    // The test runs from `ainb-tui/crates/ainb-plugin-notifyd`.
    // The hook script lives at the monorepo root under
    // `plugins/ainb-hooks/hooks/notify.sh`.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let manifest = Path::new(&manifest);
    let monorepo = manifest.ancestors().nth(3).expect("walking up from manifest");
    monorepo.join("plugins/ainb-hooks/hooks/notify.sh")
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

fn fire_claude_hook(script: &Path, home: &Path, json: &str) {
    let out = Command::new("bash")
        .arg(script)
        .env("HOME", home)
        .env("AINB_AGENT", "claude")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(json.as_bytes()).unwrap();
            }
            child.wait_with_output()
        })
        .expect("running claude hook");
    assert!(
        out.status.success(),
        "claude hook failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fire_codex_hook(script: &Path, home: &Path, json: &str) {
    let out = Command::new("bash")
        .arg(script)
        .arg(json)
        .env("HOME", home)
        .env("AINB_AGENT", "codex")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("running codex hook");
    assert!(
        out.status.success(),
        "codex hook failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn hook_script_into_real_daemon_persists_both_agents() {
    // Skip if the bash script is missing — useful when running from
    // a CI matrix entry that hasn't checked out the plugin tree
    // (defensive, not expected).
    let script = hook_script();
    if !script.exists() {
        eprintln!(
            "skipping: hook script not found at {} (CARGO_MANIFEST_DIR={:?})",
            script.display(),
            std::env::var("CARGO_MANIFEST_DIR").ok(),
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let paths = Paths::under(home.join(".agents-in-a-box"));
    paths.ensure_base().unwrap();

    let config = RunConfig {
        paths: paths.clone(),
        retention: RetentionPolicy {
            retention_days: 0,
            max_rows: 0,
        },
        os_notifications: false,
        ingest_interval: std::time::Duration::from_millis(20),
    };
    let daemon = tokio::spawn(async move { run_daemon(config).await });

    wait_for_socket(&paths.socket).await;

    // Claude path: JSON on stdin, hook_event_name field, agent=claude.
    let claude_json = r#"{"hook_event_name":"Stop","session_id":"sess-claude-1","cwd":"/Users/example/proj","payload":{"k":"v"}}"#;
    fire_claude_hook(&script, &home, claude_json);

    // Codex path: JSON on argv[1], type field, agent=codex.
    let codex_json = r#"{"type":"agent-turn-complete","session_id":"sess-codex-1","cwd":"/Users/example/codex-proj"}"#;
    fire_codex_hook(&script, &home, codex_json);

    // Give the daemon a tick to drain both writes.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Open the same DB the daemon wrote.
    let store = Store::open(&paths.db).unwrap();
    let rows = store.list(false, None, None, 100).unwrap();
    assert_eq!(rows.len(), 2, "expected 2 rows, got {:?}", rows);

    let mut by_agent: std::collections::HashMap<String, _> = std::collections::HashMap::new();
    for row in &rows {
        by_agent.insert(row.agent.clone(), row.clone());
    }

    let claude_row = by_agent.get("claude").expect("claude row missing");
    assert_eq!(claude_row.raw_event, "Stop");
    assert_eq!(claude_row.session_id, "sess-claude-1");
    assert!(claude_row.project.contains("proj"));

    let codex_row = by_agent.get("codex").expect("codex row missing");
    assert_eq!(codex_row.raw_event, "agent-turn-complete");
    assert_eq!(codex_row.session_id, "sess-codex-1");

    daemon.abort();
}

#[tokio::test]
async fn hook_falls_back_when_daemon_down_then_daemon_replays() {
    let script = hook_script();
    if !script.exists() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let paths = Paths::under(home.join(".agents-in-a-box"));
    paths.ensure_base().unwrap();

    // Fire the hook BEFORE the daemon starts. The script should
    // detect no socket, fail the lazy spawn (because `ainb` is not on
    // PATH in a test context), and write to the fallback file.
    let claude_json = r#"{"hook_event_name":"Stop","session_id":"sess-fallback","cwd":"/Users/example/proj","payload":{}}"#;
    fire_claude_hook(&script, &home, claude_json);

    assert!(
        paths.fallback.exists(),
        "expected fallback file at {}",
        paths.fallback.display()
    );

    // Now start the daemon. It should replay the fallback and remove
    // the file.
    let config = RunConfig {
        paths: paths.clone(),
        retention: RetentionPolicy {
            retention_days: 0,
            max_rows: 0,
        },
        os_notifications: false,
        ingest_interval: std::time::Duration::from_millis(20),
    };
    let daemon = tokio::spawn(async move { run_daemon(config).await });
    wait_for_socket(&paths.socket).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let store = Store::open(&paths.db).unwrap();
    let rows = store.list(false, None, None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent, "claude");
    assert_eq!(rows[0].raw_event, "Stop");
    assert!(!paths.fallback.exists(), "fallback file should be cleared");

    daemon.abort();
}
