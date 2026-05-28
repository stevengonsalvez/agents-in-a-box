//! Subprocess-level tests for
//! [`ainb_plugin_witr::exec::exec_witr_json`].
//!
//! Pairs four stub-witr behaviours with the four side-effecting
//! [`ExecResult`] variants:
//!
//! - `exec_with_stub_returns_ok_snapshot` — stub prints valid JSON.
//! - `exec_with_stub_timeout` — stub sleeps past `SCAN_TIMEOUT`.
//! - `exec_with_stub_non_zero_exit` — stub exits 1 with stderr.
//! - `exec_with_stub_invalid_json` — stub prints garbage.
//!
//! The pure validation surface is covered by unit tests inside
//! `src/exec.rs`. This file only exercises the actual subprocess
//! exec + JSON-decode path. Stubs are shell scripts marked
//! executable; gated on `cfg(unix)` per
//! [[reference_rust_unix_only_apis]].
//!
//! No `ENV_LOCK` here (unlike `detect_subprocess.rs`) — these tests
//! pass `witr_path` directly to `exec_witr_json`, so no `PATH`
//! mutation is needed.
//!
//! Timeout-reap guarantee: when [`exec_witr_json`] hits its
//! deadline, the `tokio::process::Child` is dropped, which kills
//! the OS process. If anyone ever replaces `tokio::process::Command`
//! with `std::process::Command` this contract evaporates and the
//! sleep stub leaks. The 8-second sleep budget below is sized so a
//! regression of that guarantee shows up as a CI hang, not as
//! permanent zombies.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_plugin_witr::exec::{ExecResult, exec_witr_json};

fn stage_stub(dir: &Path, body: &str) -> PathBuf {
    let bin = dir.join("witr");
    fs::write(&bin, body).expect("write stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).expect("stat stub").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod stub");
    bin
}

/// Pin the exact JSON we stub witr to emit so the parse-success
/// assertions stay independent of any sample reformatting. Staged
/// to disk (not embedded in a shell-quoted string) so a `'` in any
/// future sample edit can't trip POSIX shell quoting rules.
const SAMPLE_JSON_ONELINE: &str = r#"{"Target":{"Type":"name","Value":"nginx"},"ResolvedTarget":"nginx","Process":{"PID":1234,"PPID":800,"Command":"nginx"},"RestartCount":0,"Ancestry":[],"Source":{"Type":"systemd","Name":"nginx.service"},"Warnings":[]}"#;

#[tokio::test]
async fn exec_with_stub_returns_ok_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Stage the JSON in a sibling file so the stub doesn't have to
    // embed it in shell-quoted form.
    let json_file = dir.path().join("snapshot.json");
    fs::write(&json_file, SAMPLE_JSON_ONELINE).expect("write json fixture");

    let json_path = json_file.to_str().expect("utf8 json path");
    let stub = stage_stub(
        dir.path(),
        &format!(
            "#!/bin/sh\n# exec_witr_json passes `--json -- <target>`. We accept either argv\n# shape so the stub keeps working if the call site drops the `--`.\nif [ \"$1\" = \"--json\" ]; then\n  cat \"{json_path}\"\n  exit 0\nfi\nprintf 'stub-witr: unexpected args: %s\\n' \"$*\" >&2\nexit 1\n",
        ),
    );

    let result = exec_witr_json(&stub, "nginx").await;
    match result {
        ExecResult::Ok(snap) => {
            // `snap` is `Box<WitrSnapshot>` — `Deref` makes field
            // access transparent.
            assert_eq!(snap.target.kind, "name");
            assert_eq!(snap.target.value, "nginx");
            assert_eq!(snap.process.pid, 1234);
            assert_eq!(snap.source.kind, "systemd");
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[tokio::test]
async fn exec_with_stub_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    // /bin/sleep so the script doesn't depend on $PATH — this test
    // hands witr_path directly, no PATH mutation needed.
    // Sleep just past the 5s SCAN_TIMEOUT, not 30s — bounds CI cost
    // if tokio's Child-drop reap ever regresses. 8s is enough margin
    // to prove the timeout fired before the stub would have exited
    // naturally.
    let stub = stage_stub(dir.path(), "#!/bin/sh\n/bin/sleep 8\nexit 0\n");

    let result = exec_witr_json(&stub, "nginx").await;
    assert!(
        matches!(result, ExecResult::Timeout),
        "expected Timeout, got {result:?}",
    );
}

#[tokio::test]
async fn exec_with_stub_non_zero_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stage_stub(
        dir.path(),
        "#!/bin/sh\nprintf 'target not found\\n' >&2\nexit 1\n",
    );

    let result = exec_witr_json(&stub, "nonexistent-process").await;
    match result {
        ExecResult::NonZero { code, stderr } => {
            assert_eq!(code, Some(1));
            assert!(
                stderr.contains("target not found"),
                "stderr should be surfaced: {stderr:?}",
            );
        }
        other => panic!("expected NonZero, got {other:?}"),
    }
}

#[tokio::test]
async fn exec_with_stub_invalid_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stage_stub(
        dir.path(),
        "#!/bin/sh\nprintf 'this is not json at all'\nexit 0\n",
    );

    let result = exec_witr_json(&stub, "x").await;
    match result {
        ExecResult::ParseError {
            error,
            raw_stdout_excerpt,
        } => {
            assert!(!error.is_empty());
            assert!(raw_stdout_excerpt.contains("not json"));
        }
        other => panic!("expected ParseError, got {other:?}"),
    }
}

#[tokio::test]
async fn exec_rejects_invalid_target_without_spawning() {
    // No stub on disk — if validation didn't short-circuit, we'd
    // hit a spawn-failed I/O error, which is a different variant
    // than what the validator emits. The variant we assert here
    // (SpawnFailed carrying "invalid target") proves the validator
    // ran first.
    let nonexistent = Path::new("/nonexistent/witr-binary-does-not-exist");
    let result = exec_witr_json(nonexistent, "").await;
    match result {
        ExecResult::SpawnFailed(msg) => {
            assert!(
                msg.contains("invalid target") || msg.contains("empty"),
                "expected validation message, got {msg}",
            );
        }
        other => panic!("expected SpawnFailed with validation msg, got {other:?}"),
    }
}
