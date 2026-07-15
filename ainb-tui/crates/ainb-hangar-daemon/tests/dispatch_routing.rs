//! e38.16 dispatch-routing test: a `claude`-backend agent takes the
//! `run_claude` exec path and a `codex`-backend agent takes the `run_codex`
//! exec path.
//!
//! Pure + deterministic (no tmux, no DB, no real provider). It exercises the
//! exact routing the daemon's claim loop uses: resolve a [`Backend`] from a
//! provider wire name, then dispatch to the matching [`Runner`] method. The
//! exec path actually taken is proven by **which provider log file the run
//! wrote** — `run_claude` writes `claude.jsonl`, `run_codex` writes
//! `codex.jsonl`. A regression that ignored the backend (always `run_claude`)
//! would write `claude.jsonl` for the codex case and the codex assertions would
//! fail.
//!
//! This is the fast both-legs companion to the tmux e2e tripwire
//! `tripwire_dispatch_routes_by_backend.rs` (which drives the genuine daemon
//! binary, but only its codex leg from one daemon instance).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::execenv::ExecEnv;
use ainb_hangar_daemon::runner::{Backend, ProviderInvocation, RunOutcome, Runner, RunnerConfig};
use tempfile::TempDir;

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

/// A fake provider that just emits a system line + result and exits 0.
fn fake_provider(dir: &Path, name: &str) -> PathBuf {
    write_script(
        dir,
        name,
        r#"echo '{"type":"system","session_id":"s"}'
echo '{"type":"result","content":"ok"}'
exit 0"#,
    )
}

/// Build a runner whose claude/codex paths are two DISTINCT fake binaries, so a
/// mis-route would spawn the wrong one (and the log-file assertion would catch
/// it regardless).
fn runner_with(claude: PathBuf, codex: PathBuf) -> Runner {
    Runner::new(RunnerConfig {
        claude_path: claude,
        codex_path: codex,
        // Not exercised by these two routes; a bogus path proves a mis-route to
        // copilot would fail loudly rather than silently succeed.
        copilot_path: PathBuf::from("/nonexistent/copilot"),
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    })
}

/// Mirror the daemon's `execute_claimed` routing: backend → exec method.
async fn dispatch(runner: &Runner, backend: Backend, env: &ExecEnv) -> RunOutcome {
    match backend {
        Backend::Claude => runner.run_claude(env, std::iter::empty()).await.expect("run claude"),
        Backend::Codex => runner
            .run_codex(env, std::iter::empty(), &ProviderInvocation::default())
            .await
            .expect("run codex"),
        Backend::Copilot => runner
            .run_copilot(env, std::iter::empty(), &ProviderInvocation::default())
            .await
            .expect("run copilot"),
    }
}

#[tokio::test]
async fn claude_backend_takes_claude_path() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let claude = fake_provider(tmp.path(), "fake-claude.sh");
    let codex = fake_provider(tmp.path(), "fake-codex.sh");
    let runner = runner_with(claude, codex);

    // A claude runtime's provider wire name resolves to the claude backend.
    let backend = Backend::from_provider("claude");
    assert_eq!(backend, Backend::Claude);

    let outcome = dispatch(&runner, backend, &env).await;
    assert!(matches!(outcome, RunOutcome::Success(_)));

    // run_claude writes claude.jsonl and never codex.jsonl.
    assert!(
        env.logs.join("claude.jsonl").exists(),
        "claude backend must take run_claude (writes claude.jsonl)"
    );
    assert!(
        !env.logs.join("codex.jsonl").exists(),
        "claude backend must NOT take the codex path"
    );
}

#[tokio::test]
async fn codex_backend_takes_codex_path() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let claude = fake_provider(tmp.path(), "fake-claude.sh");
    let codex = fake_provider(tmp.path(), "fake-codex.sh");
    let runner = runner_with(claude, codex);

    // A codex runtime's provider wire name resolves to the codex backend.
    let backend = Backend::from_provider("codex");
    assert_eq!(backend, Backend::Codex);

    let outcome = dispatch(&runner, backend, &env).await;
    assert!(matches!(outcome, RunOutcome::Success(_)));

    // run_codex writes codex.jsonl and never claude.jsonl — the positive proof
    // the new path is taken, plus the negative proof it did NOT fall through to
    // the pre-e38.16 unconditional run_claude.
    assert!(
        env.logs.join("codex.jsonl").exists(),
        "codex backend must take run_codex (writes codex.jsonl)"
    );
    assert!(
        !env.logs.join("claude.jsonl").exists(),
        "codex backend must NOT fall through to run_claude (no claude.jsonl)"
    );
}

#[test]
fn wired_providers_route_and_unknown_defaults_to_claude() {
    // Every WIRED provider routes to its own exec path — copilot included; it no
    // longer silently falls back to claude.
    assert_eq!(Backend::from_provider("codex"), Backend::Codex);
    assert_eq!(Backend::from_provider("copilot"), Backend::Copilot);
    assert_eq!(Backend::from_provider("claude"), Backend::Claude);
    // A genuinely not-wired / misconfigured provider must still dispatch (to the
    // safe default) rather than strand the task.
    assert_eq!(Backend::from_provider("gemini"), Backend::Claude);
    assert_eq!(Backend::from_provider(""), Backend::Claude);
    // Case-insensitive on the wired names.
    assert_eq!(Backend::from_provider("CODEX"), Backend::Codex);
    assert_eq!(Backend::from_provider("Copilot"), Backend::Copilot);
}
