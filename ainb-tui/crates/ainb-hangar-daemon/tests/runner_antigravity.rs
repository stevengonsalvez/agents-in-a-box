//! Runner unit and integration tests for antigravity provider.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::execenv::ExecEnv;
use ainb_hangar_daemon::runner::{
    Backend, Mode, ProviderInvocation, RunOutcome, Runner, RunnerConfig,
};
use tempfile::TempDir;

fn exec_env_in(root: &Path) -> ExecEnv {
    let env = ExecEnv {
        workdir: root.join("workdir"),
        output: root.join("output"),
        logs: root.join("logs"),
        gc_meta: root.join(".gc_meta.json"),
    };
    fs::create_dir_all(&env.workdir).expect("create workdir");
    fs::create_dir_all(&env.output).expect("create output");
    fs::create_dir_all(&env.logs).expect("create logs");
    env
}

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

fn cfg_with_antigravity(antigravity_path: PathBuf) -> RunnerConfig {
    RunnerConfig {
        claude_path: PathBuf::from("/nonexistent/claude"),
        codex_path: PathBuf::from("/nonexistent/codex"),
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        antigravity_path,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    }
}

fn invocation(prompt: &str) -> ProviderInvocation {
    ProviderInvocation {
        prompt: prompt.to_string(),
        model: None,
        cli_args: Vec::new(),
    }
}

#[tokio::test]
async fn antigravity_happy_path_writes_log_and_succeeds() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let script = write_script(
        tmp.path(),
        "fake-agy.sh",
        r#"echo '{"type":"system","session_id":"agy-session-1"}'
echo '{"type":"assistant","content":"hello from antigravity"}'
echo '{"type":"result","subtype":"success","is_error":false}'
exit 0"#,
    );
    let runner = Runner::new(cfg_with_antigravity(script));

    let outcome = runner
        .run_antigravity(&env, std::iter::empty(), &invocation("fix the bug"))
        .await
        .expect("run antigravity");

    assert!(matches!(outcome, RunOutcome::Success(_)));
    let result = outcome.result();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.session_id.as_deref(), Some("agy-session-1"));

    let log_file = env.logs.join("antigravity.jsonl");
    assert!(log_file.exists(), "antigravity.jsonl must be created");
    let content = fs::read_to_string(&log_file).expect("read log");
    assert!(content.contains("agy-session-1"));
}

#[test]
fn antigravity_command_spec_argv_structure() {
    let tmp = TempDir::new().expect("tmp");
    let runner = Runner::new(cfg_with_antigravity(tmp.path().join("fake-agy.sh")));

    let inv = ProviderInvocation {
        prompt: "review the changes".to_string(),
        model: Some("gemini-2.5-pro".to_string()),
        cli_args: vec!["--extra-arg".to_string(), "val".to_string()],
    };

    let (prog, headless_argv) = runner.provider_command(Backend::Antigravity, &inv, Mode::Headless);
    assert_eq!(prog, tmp.path().join("fake-agy.sh"));
    assert_eq!(
        headless_argv,
        vec![
            "-p",
            "--dangerously-skip-permissions",
            "--output-format",
            "stream-json",
            "--model",
            "gemini-2.5-pro",
            "--extra-arg",
            "val",
            "--",
            "review the changes"
        ]
    );

    let (_prog, interactive_argv) =
        runner.provider_command(Backend::Antigravity, &inv, Mode::Interactive);
    assert_eq!(
        interactive_argv,
        vec![
            "--dangerously-skip-permissions",
            "-i",
            "--model",
            "gemini-2.5-pro",
            "--extra-arg",
            "val",
            "--",
            "review the changes"
        ]
    );
}
