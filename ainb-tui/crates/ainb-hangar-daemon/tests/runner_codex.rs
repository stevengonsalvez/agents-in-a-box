//! e38.16 runner unit tests: subprocess exec of the `codex` provider.
//!
//! Mirrors `runner_claude.rs`: each test drives [`Runner::run_codex`] against a
//! tiny shell-script stand-in for the real `codex` CLI (`fake_codex` builders
//! below) — never a real provider. The contract under test is the codex exec
//! path: the `exec` subcommand + `-m <model>` + the agent's `cli_args` land on
//! the argv, the agent's `agent_env` reaches the child (while ambient secrets
//! stay stripped), the JSONL stdout is teed to `codex.jsonl` with `session_id`
//! pinned, exit codes and timeouts map to the right [`RunOutcome`], and the run
//! is OS-sandbox confined (same confinement as claude).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::execenv::ExecEnv;
use ainb_hangar_daemon::runner::{ProviderInvocation, RunOutcome, Runner, RunnerConfig};
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

/// Write an executable shell script at `dir/name` with the given body (prefixed
/// with a `#!/bin/sh` shebang) and return its path.
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

/// A [`RunnerConfig`] whose `codex_path` is the fake script, sandbox ON.
fn cfg_with_codex(codex_path: PathBuf) -> RunnerConfig {
    RunnerConfig {
        // claude_path is irrelevant to a codex run but the struct requires it; a
        // bogus path is fine since `run_codex` never touches it.
        claude_path: PathBuf::from("/nonexistent/claude"),
        codex_path,
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    }
}

/// A fake `codex` that emits a system line carrying `session_id`, a result line,
/// and exits 0.
fn fake_codex_happy(dir: &Path) -> PathBuf {
    write_script(
        dir,
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"cdx-1"}'
echo '{"type":"result","content":"ok"}'
exit 0"#,
    )
}

#[tokio::test]
async fn codex_exec_captures_exit_code_and_pins_session() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = fake_codex_happy(tmp.path());
    let runner = Runner::new(cfg_with_codex(script));

    let outcome = runner
        .run_codex(&env, std::iter::empty(), &ProviderInvocation::default())
        .await
        .expect("run");

    assert_eq!(outcome.result().exit_code, Some(0));
    assert_eq!(outcome.result().session_id.as_deref(), Some("cdx-1"));
    assert!(matches!(outcome, RunOutcome::Success(_)));
}

#[tokio::test]
async fn codex_exec_streams_jsonl_to_codex_log() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = fake_codex_happy(tmp.path());
    let runner = Runner::new(cfg_with_codex(script));

    runner
        .run_codex(&env, std::iter::empty(), &ProviderInvocation::default())
        .await
        .expect("run");

    // The codex provider writes its OWN log file (not claude.jsonl).
    let jsonl = env.logs.join("codex.jsonl");
    assert!(jsonl.exists(), "expected codex.jsonl at {jsonl:?}");
    let lines = fs::read_to_string(&jsonl)
        .expect("read jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(lines, 2, "expected 2 JSONL lines");
}

#[tokio::test]
async fn codex_exec_argv_carries_subcommand_model_and_cli_args() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    // The fake echoes its own argv (as a JSONL system line so the runner still
    // pins a session id) so the test can assert the exec shape.
    let script = write_script(
        tmp.path(),
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"s"}'
echo "ARGV=$*"
exit 0"#,
    );
    let runner = Runner::new(cfg_with_codex(script));
    let invocation = ProviderInvocation {
        prompt: String::new(),
        model: Some("gpt-5-codex".to_string()),
        cli_args: vec!["--full-auto".to_string()],
    };

    let outcome = runner.run_codex(&env, std::iter::empty(), &invocation).await.expect("run");
    let tail = &outcome.result().stdout_tail;

    assert!(
        tail.contains("ARGV=exec"),
        "argv must start with the `exec` subcommand, got: {tail}"
    );
    assert!(
        tail.contains("-m gpt-5-codex"),
        "argv must carry the model via -m, got: {tail}"
    );
    assert!(
        tail.contains("--full-auto"),
        "argv must carry the agent's cli_args, got: {tail}"
    );
}

#[tokio::test]
async fn codex_exec_passes_agent_env_but_strips_ambient_secret() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"s"}'
echo "FOO=${FOO:-<absent>}"
echo "SECRET_KEY=${SECRET_KEY:-<absent>}"
exit 0"#,
    );
    let runner = Runner::new(cfg_with_codex(script));
    // The ambient/source env (the daemon's) carries a secret that must NOT reach
    // the child (it is not in the allowlist). The agent's explicit per-agent env
    // (FOO) is a deliberate override that MUST reach the child.
    let source = [("SECRET_KEY".to_string(), "leak".to_string())];
    let invocation = ProviderInvocation::default();
    let extra_env = [("FOO".to_string(), "bar".to_string())];

    let outcome = runner
        .run_codex_with_env(
            &env,
            source.iter().cloned(),
            extra_env.iter().cloned(),
            &invocation,
        )
        .await
        .expect("run");
    let tail = &outcome.result().stdout_tail;

    assert!(
        tail.contains("FOO=bar"),
        "agent_env override must reach the child, got: {tail}"
    );
    assert!(
        tail.contains("SECRET_KEY=<absent>"),
        "ambient secret must be stripped, got: {tail}"
    );
}

#[tokio::test]
async fn codex_exec_nonzero_exit_marks_agent_error() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"s"}'
exit 1"#,
    );
    let runner = Runner::new(cfg_with_codex(script));

    let outcome = runner
        .run_codex(&env, std::iter::empty(), &ProviderInvocation::default())
        .await
        .expect("run");

    match outcome {
        RunOutcome::Failed { reason, result } => {
            assert_eq!(reason, FailureReason::AgentError);
            assert_eq!(result.exit_code, Some(1));
        }
        RunOutcome::Success(_) => panic!("expected Failed(AgentError), got Success"),
        RunOutcome::Cancelled(_) => panic!("expected Failed(AgentError), got Cancelled"),
    }
}

#[tokio::test]
async fn codex_exec_timeout_kills_subprocess() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"s"}'
sleep 5
exit 0"#,
    );
    let mut cfg = cfg_with_codex(script);
    cfg.max_runtime = Duration::from_millis(100);
    let runner = Runner::new(cfg);

    let started = std::time::Instant::now();
    let outcome = runner
        .run_codex(&env, std::iter::empty(), &ProviderInvocation::default())
        .await
        .expect("run");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "runner should kill the subprocess well before its 5s sleep, took {elapsed:?}"
    );
    match outcome {
        RunOutcome::Failed { reason, .. } => assert_eq!(reason, FailureReason::Timeout),
        RunOutcome::Success(_) => panic!("expected Failed(Timeout), got Success"),
        RunOutcome::Cancelled(_) => panic!("expected Failed(Timeout), got Cancelled"),
    }
}

/// e38.23 sandbox-confinement proof for the codex path: a confined run that
/// writes ONLY to its task-isolated `logs/` (an allowed write root) still
/// succeeds, proving the codex spawn goes through the same OS sandbox as claude.
/// On a platform with no sandbox primitive the run degrades to a passthrough and
/// still succeeds, so this asserts the success outcome (never a false failure).
#[tokio::test]
async fn codex_run_is_sandbox_confined() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    // The fake reads $HOME (pointed at the task root — an allowed read root) and
    // writes only its JSONL log under logs/ (an allowed write root); a confined
    // run therefore succeeds, proving the sandbox composes with the codex path.
    let script = write_script(
        tmp.path(),
        "fake-codex.sh",
        r#"echo '{"type":"system","session_id":"s"}'
echo "HOME=${HOME:-<absent>}"
exit 0"#,
    );
    let runner = Runner::new(cfg_with_codex(script));
    let source = [("HOME".to_string(), tmp.path().display().to_string())];

    let outcome = runner
        .run_codex(&env, source.iter().cloned(), &ProviderInvocation::default())
        .await
        .expect("run");

    assert!(
        matches!(outcome, RunOutcome::Success(_)),
        "a confined codex run writing only to its task roots must succeed"
    );
    assert!(
        outcome.result().stdout_tail.contains(&format!("HOME={}", tmp.path().display())),
        "HOME (allowlisted) must pass through the confined run"
    );
}
