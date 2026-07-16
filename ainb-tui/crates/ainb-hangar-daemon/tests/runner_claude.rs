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
use ainb_hangar_daemon::runner::{
    Backend, Mode, ProviderInvocation, RunOutcome, Runner, RunnerConfig,
};
use ainb_hangar_store::service::fail::FailureReason;
use tempfile::TempDir;

/// A minimal invocation carrying only a brief — the shape every real dispatch
/// has. `ProviderInvocation` has no `Default` on purpose: a promptless provider
/// exits non-zero instead of running, so these tests must state a brief rather
/// than quietly assert against an argv no real run ever produces.
fn invocation() -> ProviderInvocation {
    ProviderInvocation {
        prompt: "do the thing".to_string(),
        model: None,
        cli_args: Vec::new(),
    }
}

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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        // Exercise the WIRED sandbox path: the stand-in script reads `$HOME`
        // (pointed at the tempdir, which is the task root and so an allowed read
        // root) and writes only its log under `logs/`, so a confined run still
        // succeeds — proving the env-allowlist + OS-sandbox layers compose.
        sandbox: true,
    };
    let runner = Runner::new(cfg);
    // SECRET_KEY is NOT in the 12-var allowlist; HOME is. The runner must build
    // the child env solely from the allowlist sourced from these overrides.
    let overrides = [
        ("SECRET_KEY".to_string(), "xyz".to_string()),
        ("HOME".to_string(), tmp.path().display().to_string()),
    ];

    let outcome = runner
        .run_claude(&env, overrides.iter().cloned(), &invocation())
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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

    assert_eq!(outcome.result().exit_code, Some(0));
    assert!(matches!(outcome, RunOutcome::Success(_)));
}

#[tokio::test]
async fn exec_captures_usage_from_result_line() {
    // e38.35: the runner parses the final `{"type":"result",...}` JSONL line's
    // `usage` (input/output tokens) + `total_cost_usd` so the usage dashboard can
    // roll them up. A run that reports usage lands a `Some(ProviderUsage)` on the
    // captured result; a run with no result-usage lands `None`.
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"abc-123"}'
echo '{"type":"result","subtype":"success","total_cost_usd":0.0231,"usage":{"input_tokens":1200,"output_tokens":340}}'
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");
    let usage = outcome.result().usage.clone().expect("usage captured from result line");
    assert_eq!(usage.input_tokens, 1200);
    assert_eq!(usage.output_tokens, 340);
    assert!((usage.cost_usd - 0.0231).abs() < 1e-9);
}

#[tokio::test]
async fn exec_no_result_usage_leaves_usage_none() {
    // A provider run whose result line carries no `usage` (or no result line at
    // all) leaves `usage` as `None` — the dashboard simply records nothing for it.
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = fake_claude_happy(tmp.path());
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");
    assert!(
        outcome.result().usage.is_none(),
        "a result line without usage leaves usage None"
    );
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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

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
async fn exec_exit0_with_structured_error_marks_failed() {
    // bead 48d: a provider that EXITS 0 but whose structured terminal reports an
    // error (claude's `error_max_turns`) must finalize `Failed`, NOT `Success` —
    // the exit code is not the completion signal. The reason is the one the
    // subtype maps to (`error_max_turns` → the retry-fresh IterationLimit), and
    // the session_id from the stream is still captured (48c).
    //
    // Mutation-proof: revert the finalize-on-structured-result change in
    // `run_provider::run_provider` and this goes RED — exit 0 falls back to
    // `Success` and the daemon marks the task `done` over an incomplete run.
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"sid-maxturns"}'
echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":2}'
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

    match outcome {
        RunOutcome::Failed { reason, result } => {
            assert_eq!(
                reason,
                FailureReason::IterationLimit,
                "error_max_turns must map to the retry-fresh IterationLimit reason"
            );
            assert_eq!(
                result.exit_code,
                Some(0),
                "the process exited 0 — the STRUCTURED error, not the exit code, drove the failure"
            );
            assert_eq!(
                result.session_id.as_deref(),
                Some("sid-maxturns"),
                "session_id must still be pinned from the stream-json system line (48c)"
            );
        }
        other => panic!("exit-0 structured error must be Failed(IterationLimit), got {other:?}"),
    }
}

#[tokio::test]
async fn exec_exit0_without_success_terminal_marks_failed() {
    // bead 48d (the other half): a claude headless run (structured `stream-json`)
    // that exits 0 having emitted NO terminal event — a truncated / renamed /
    // empty structured stream — must finalize `Failed`, never `Success`. Because
    // this run PROMISED a structured terminal yet produced none, the reason is the
    // DISTINCT ProviderContractDrift (the CLI shape drifted from the pinned
    // parser), not a generic AgentError — so an operator can tell a parser-update
    // need from a real agent give-up. The daemon must not mark a task `done` over
    // work that never reported completion.
    //
    // Integration mutation proof: reverting finalize_outcome's exit-0-no-terminal
    // arm to `Success` flips this to `done` (RED as `done over no work`);
    // dropping the `structured` split flips `reason` to AgentError (RED here).
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    // A `system` line (so session_id is pinned) but NO `result` terminal — the
    // stream ended before any outcome, yet the process exited 0.
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"sid-noterm"}'
exit 0"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

    match outcome {
        RunOutcome::Failed { reason, result } => {
            assert_eq!(reason, FailureReason::ProviderContractDrift);
            assert_eq!(
                result.exit_code,
                Some(0),
                "exit 0 with no success terminal is still a failure (48d)"
            );
        }
        other => {
            panic!(
                "exit-0 with no success terminal must be Failed(ProviderContractDrift), got {other:?}"
            )
        }
    }
}

#[tokio::test]
async fn exec_extempfail_75_marks_runtime_offline() {
    // EX_TEMPFAIL (75) is the provider's "transient runtime failure, retry later"
    // signal — it must classify as the infra/retryable RuntimeOffline reason
    // (which the daemon's retry chain re-dispatches), NOT the terminal AgentError
    // that every other non-zero exit maps to.
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-claude.sh",
        r#"echo '{"type":"system","session_id":"s"}'
exit 75"#,
    );
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

    match outcome {
        RunOutcome::Failed { reason, result } => {
            assert_eq!(reason, FailureReason::RuntimeOffline);
            assert_eq!(result.exit_code, Some(75));
        }
        RunOutcome::Success(_) => panic!("expected Failed(RuntimeOffline), got Success"),
        RunOutcome::Cancelled(_) => panic!("expected Failed(RuntimeOffline), got Cancelled"),
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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

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
        // e38.16: the codex path is irrelevant to a claude run; a placeholder is
        // fine since `run_claude` never spawns it.
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_millis(100),
        tail_lines: 50,
        sandbox: true,
    });

    let started = std::time::Instant::now();
    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");
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

/// A fake `claude` that models the REAL permission semantics of Claude Code
/// 2.1.210 under `-p`, as measured with a clean `$HOME`:
///
/// * WITHOUT `--dangerously-skip-permissions`, tool calls are DENIED. The process
///   still prints a `result` line with `subtype:"success"` and **exits 0** — it
///   just never writes the file.
/// * WITH the flag, the write lands.
///
/// The exit code is 0 in BOTH branches on purpose: that is precisely what made
/// the bug silent, since [`RunOutcome::Success`] is keyed on exit code. A test
/// that asserted only the outcome would pass either way — so the assertion that
/// matters is the file on disk.
fn fake_claude_permission_aware(dir: &Path) -> PathBuf {
    write_script(
        dir,
        "fake-claude-perm.sh",
        r#"skip=""
for a in "$@"; do
  if [ "$a" = "--dangerously-skip-permissions" ]; then skip="1"; fi
done
echo '{"type":"system","session_id":"perm-1"}'
if [ -n "$skip" ]; then
  printf 'TOOLPROOF_OK' > proof.txt
  echo '{"type":"result","subtype":"success","is_error":false,"result":"DONE"}'
else
  echo '{"type":"result","subtype":"success","is_error":false,"permission_denials":[{"tool_name":"Write"}],"result":"I do not have permission to write that file."}'
fi
exit 0"#,
    )
}

/// REGRESSION (silent no-op): a tool-using claude run must actually produce its
/// side effect, not merely report success.
///
/// Mutation-proof: drop `--dangerously-skip-permissions` from `claude_spec` and
/// this test goes RED on the `proof.txt` assertion — while the `RunOutcome`
/// assertion below stays GREEN, reproducing the exact false-green class that let
/// this ship (every prior headless proof used a no-tool brief).
#[tokio::test]
async fn exec_tool_using_task_actually_writes_its_file() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = fake_claude_permission_aware(tmp.path());
    let runner = Runner::new(RunnerConfig {
        claude_path: script,
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    });

    let outcome = runner.run_claude(&env, std::iter::empty(), &invocation()).await.expect("run");

    // The daemon scores this run a success...
    assert!(
        matches!(outcome, RunOutcome::Success(_)),
        "expected Success, got {outcome:?}"
    );
    // ...so the ONLY assertion that can catch a silent no-op is the side effect.
    let proof = env.workdir.join("proof.txt");
    assert!(
        proof.is_file(),
        "tool-using task reported success but wrote NO file: the daemon would \
         mark this task `done` over an untouched workdir"
    );
    assert_eq!(
        fs::read_to_string(&proof).expect("read proof"),
        "TOOLPROOF_OK",
        "tool ran but produced the wrong content"
    );
}

/// The claude argv must carry a permission flag, or a tool-using task silently
/// no-ops (denied tool, exit 0, task marked `done`).
///
/// Pins `--dangerously-skip-permissions`: the operator's explicit choice of
/// blanket tool autonomy, taken because the narrower `acceptEdits` measurably
/// denies `Bash` and would leave a build/test task half-done yet reported as
/// success. The escape hatch (`cli_args` appended after) is pinned too, since a
/// task overriding the posture depends on ordering.
#[test]
fn claude_argv_carries_skip_permissions_and_cli_args_can_override() {
    let runner = Runner::new(RunnerConfig {
        claude_path: PathBuf::from("/nonexistent/claude"),
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(1),
        tail_lines: 1,
        sandbox: true,
    });
    let (_prog, argv) = runner.provider_command(Backend::Claude, &invocation(), Mode::Headless);
    assert!(
        argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "claude argv must carry --dangerously-skip-permissions or a tool-using task \
         silently no-ops, got {argv:?}"
    );

    // A task's own cli_args are appended AFTER the daemon's flag, so an override
    // wins. If this ordering ever flips, the escape hatch dies silently.
    let overridden = ProviderInvocation {
        cli_args: vec!["--permission-mode".to_string(), "plan".to_string()],
        ..invocation()
    };
    let (_p, argv2) = runner.provider_command(Backend::Claude, &overridden, Mode::Headless);
    let skip_at = argv2
        .iter()
        .position(|a| a == "--dangerously-skip-permissions")
        .expect("flag present");
    let override_at =
        argv2.iter().position(|a| a == "--permission-mode").expect("cli_args appended");
    assert!(
        override_at > skip_at,
        "cli_args must come AFTER the daemon flag so a task can override it, got {argv2:?}"
    );
}
