//! P1.7 runner unit tests: subprocess exec of the `claude` provider.
//!
//! Each test drives [`Runner::run_claude`] against a tiny shell-script stand-in
//! for the real `claude` CLI (`fake_claude` builders below). The harness never
//! invokes a real provider — the contract under test is the runner's
//! orchestration: env allowlisting, exit-code capture, JSONL tee + `session_id`
//! extraction, and timeout-kill. The full daemon e2e lives in
//! `tripwire_task_happy_path_claude_provider.rs`.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::execenv::ExecEnv;
use ainb_hangar_daemon::runner::{RunOutcome, Runner, RunnerConfig};
use ainb_hangar_store::service::fail::FailureReason;
use tempfile::TempDir;

/// Build a per-task [`ExecEnv`] rooted in `dir` (workdir/output/logs created).
fn exec_env_in(dir: &Path) -> ExecEnv {
    let workdir = dir.join("workdir");
    let output = dir.join("output");
    let logs = dir.join("logs");
    for d in [&workdir, &output, &logs] {
        fs::create_dir_all(d).expect("mkdir env dir");
    }
    ExecEnv {
        workdir,
        output,
        logs,
        gc_meta: dir.join(".gc_meta.json"),
    }
}

/// Write an executable shell script at `dir/name` with the given body and return
/// its path. The body is prefixed with a `#!/bin/sh` shebang.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// A fake `claude` that emits a system line carrying `session_id` then a result
/// line, and exits 0.
fn fake_claude_happy(dir: &Path) -> PathBuf {
    write_script(
        dir,
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"abc-123"}'
echo '{"type":"result","content":"ok"}'
exit 0"#,
    )
}

#[tokio::test]
async fn exec_uses_allowlisted_env_only() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    // A fake claude that dumps whichever of these vars it received.
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo "{\"type\":\"system\",\"session_id\":\"s\"}"
echo "SECRET_KEY=${SECRET_KEY:-<absent>}"
echo "HOME=${HOME:-<absent>}"
exit 0"#,
    );
    let cfg = RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
    };
    let runner = Runner::new(cfg);
    // SECRET_KEY is NOT in the 12-var allowlist; HOME is. The runner must build
    // the child env solely from the allowlist sourced from these overrides.
    let overrides = [
        ("SECRET_KEY".to_string(), "xyz".to_string()),
        ("HOME".to_string(), tmp.path().display().to_string()),
    ];

    let outcome = runner
        .run_claude(&env, overrides.iter().cloned())
        .await
        .expect("run");
    let result = outcome.result();

    assert!(
        result.stdout_tail.contains("SECRET_KEY=<absent>"),
        "SECRET_KEY must be stripped from the child env, got: {}",
        result.stdout_tail
    );
    assert!(
        result.stdout_tail.contains(&format!("HOME={}", tmp.path().display())),
        "HOME (allowlisted) must pass through, got: {}",
        result.stdout_tail
    );
}

#[tokio::test]
async fn exec_captures_exit_code() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = fake_claude_happy(tmp.path());
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
    });

    let outcome = runner.run_claude(&env, std::iter::empty()).await.expect("run");

    assert_eq!(outcome.result().exit_code, Some(0));
    assert!(matches!(outcome, RunOutcome::Success(_)));
}

#[tokio::test]
async fn exec_nonzero_exit_marks_agent_error() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"s"}'
exit 1"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
    });

    let outcome = runner.run_claude(&env, std::iter::empty()).await.expect("run");

    match outcome {
        RunOutcome::Failed { reason, result } => {
            assert_eq!(reason, FailureReason::AgentError);
            assert_eq!(result.exit_code, Some(1));
        }
        RunOutcome::Success(_) => panic!("expected Failed(AgentError), got Success"),
    }
}

#[tokio::test]
async fn exec_parses_session_id_from_first_system_message() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"first-sid"}'
echo '{"type":"system","session_id":"second-sid"}'
echo '{"type":"result","content":"ok"}'
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
    });

    let outcome = runner.run_claude(&env, std::iter::empty()).await.expect("run");

    assert_eq!(
        outcome.result().session_id.as_deref(),
        Some("first-sid"),
        "runner must pin the FIRST system message's session_id"
    );
}

#[tokio::test]
async fn exec_streams_jsonl_to_logs_file() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"s"}'
echo '{"type":"assistant","content":"hi"}'
echo '{"type":"result","content":"ok"}'
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
    });

    runner.run_claude(&env, std::iter::empty()).await.expect("run");

    let jsonl = env.logs.join("claude.jsonl");
    assert!(jsonl.exists(), "expected claude.jsonl at {jsonl:?}");
    let lines: Vec<_> = fs::read_to_string(&jsonl)
        .expect("read jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 3, "expected 3 JSONL lines, got {lines:?}");
}

#[tokio::test]
async fn exec_timeout_kills_subprocess() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    // Sleeps 5s — well past the 100ms deadline.
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"s"}'
sleep 5
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        max_runtime: Duration::from_millis(100),
        tail_lines: 50,
    });

    let started = std::time::Instant::now();
    let outcome = runner.run_claude(&env, std::iter::empty()).await.expect("run");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "runner should kill the subprocess well before its 5s sleep, took {elapsed:?}"
    );
    match outcome {
        RunOutcome::Failed { reason, .. } => assert_eq!(reason, FailureReason::Timeout),
        RunOutcome::Success(_) => panic!("expected Failed(Timeout), got Success"),
    }
}
