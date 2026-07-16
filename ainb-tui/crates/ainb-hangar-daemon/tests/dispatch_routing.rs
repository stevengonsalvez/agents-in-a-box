//! e38.16 dispatch-routing test: each backend (`claude` / `codex` / `copilot`)
//! takes its own exec path.
//!
//! Pure + deterministic (no tmux, no DB, no real provider). It exercises the
//! exact routing the daemon's claim loop uses: resolve a [`Backend`] from a
//! provider wire name, then dispatch to the matching [`Runner`] method.
//!
//! The exec path actually taken is proven by **which BINARY was spawned** — each
//! fake provider appends its own name to a shared marker file. That is the
//! assertion that bites: the provider log file (`claude.jsonl` / `codex.jsonl` /
//! `copilot.jsonl`) is chosen by the `ProviderSpec`, NOT by the binary, so a route
//! that kept the right spec but spawned the WRONG program would still write the
//! expected log and pass a log-only assertion. The log-file assertions are kept as
//! a secondary signal (they catch a wrong SPEC); the marker catches a wrong BINARY.
//!
//! This is the fast both-legs companion to the tmux e2e tripwire
//! `tripwire_dispatch_routes_by_backend.rs` (which drives the genuine daemon
//! binary, but only its codex leg from one daemon instance).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_daemon::execenv::ExecEnv;
use ainb_hangar_daemon::runner::{
    Backend, Mode, ProviderInvocation, RunOutcome, Runner, RunnerConfig,
};
use tempfile::TempDir;

/// A minimal invocation carrying only a brief — the shape every real dispatch
/// has (`ProviderInvocation` has no `Default`: a promptless provider exits
/// non-zero instead of running).
fn invocation(prompt: &str) -> ProviderInvocation {
    ProviderInvocation {
        prompt: prompt.to_string(),
        model: None,
        cli_args: Vec::new(),
    }
}

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

/// A fake provider that IDENTIFIES ITSELF, then emits a system line + result and
/// exits 0.
///
/// The identity marker is the point: the per-provider log file
/// (`claude.jsonl`/`codex.jsonl`/`copilot.jsonl`) is chosen by the `ProviderSpec`,
/// NOT by the binary, so a log-file assertion alone proves only which SPEC ran. A
/// `run_copilot_in` that wrongly spawned `cfg.claude_path` while keeping
/// `copilot_spec` would still write `copilot.jsonl` and pass. Each fake therefore
/// appends its OWN name to a marker file, so a test can assert WHICH BINARY the
/// runner actually executed.
///
/// The marker path is baked into the script rather than passed as an env var:
/// the runner filters the child env deny-by-default, so an env-carried marker
/// would be stripped before the fake could read it.
fn fake_provider(dir: &Path, name: &str, marker: &Path) -> PathBuf {
    write_script(
        dir,
        name,
        &format!(
            r#"echo '{name}' >> '{}'
echo '{{"type":"system","session_id":"s"}}'
echo '{{"type":"result","content":"ok"}}'
exit 0"#,
            marker.display()
        ),
    )
}

/// The provider binaries the runner actually spawned, in order.
fn spawned(marker: &Path) -> Vec<String> {
    fs::read_to_string(marker)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Build a runner whose claude/codex/copilot paths are three DISTINCT fake
/// binaries, so a mis-route spawns the wrong one and the identity marker catches
/// it.
fn runner_with(claude: PathBuf, codex: PathBuf, copilot: PathBuf) -> Runner {
    Runner::new(RunnerConfig {
        claude_path: claude,
        codex_path: codex,
        copilot_path: copilot,
        max_runtime: Duration::from_secs(10),
        tail_lines: 50,
        sandbox: true,
    })
}

/// The three fake providers + a runner wired to them, plus the marker file each
/// fake records its identity in. Every route has a REAL distinct binary, so a
/// mis-route (right spec, wrong binary) is caught by the marker — not just by the
/// spec-chosen log file.
fn runner_with_all(dir: &Path) -> (Runner, PathBuf) {
    let marker = dir.join("spawned.txt");
    let runner = runner_with(
        fake_provider(dir, "fake-claude.sh", &marker),
        fake_provider(dir, "fake-codex.sh", &marker),
        fake_provider(dir, "fake-copilot.sh", &marker),
    );
    (runner, marker)
}

/// Mirror the daemon's `execute_claimed` routing: backend → exec method.
async fn dispatch(runner: &Runner, backend: Backend, env: &ExecEnv) -> RunOutcome {
    match backend {
        Backend::Claude => runner
            .run_claude(env, std::iter::empty(), &invocation("do the thing"))
            .await
            .expect("run claude"),
        Backend::Codex => runner
            .run_codex(env, std::iter::empty(), &invocation("do the thing"))
            .await
            .expect("run codex"),
        Backend::Copilot => runner
            .run_copilot(env, std::iter::empty(), &invocation("do the thing"))
            .await
            .expect("run copilot"),
    }
}

#[tokio::test]
async fn claude_backend_takes_claude_path() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let (runner, marker) = runner_with_all(tmp.path());

    // A claude runtime's provider wire name resolves to the claude backend.
    let backend = Backend::from_provider("claude");
    assert_eq!(backend, Backend::Claude);

    let outcome = dispatch(&runner, backend, &env).await;
    assert!(matches!(outcome, RunOutcome::Success(_)));

    // The CLAUDE BINARY ran (not merely: the claude spec's log file appeared).
    assert_eq!(
        spawned(&marker),
        vec!["fake-claude.sh"],
        "claude backend must spawn the claude binary"
    );
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
    let (runner, marker) = runner_with_all(tmp.path());

    // A codex runtime's provider wire name resolves to the codex backend.
    let backend = Backend::from_provider("codex");
    assert_eq!(backend, Backend::Codex);

    let outcome = dispatch(&runner, backend, &env).await;
    assert!(matches!(outcome, RunOutcome::Success(_)));

    // The CODEX BINARY ran — a spec-only assertion would not catch a route that
    // kept codex_spec but spawned the claude path.
    assert_eq!(
        spawned(&marker),
        vec!["fake-codex.sh"],
        "codex backend must spawn the codex binary"
    );

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

/// A `copilot`-backend agent takes the `run_copilot` exec path: it spawns the
/// COPILOT binary (through the same sandbox + env allowlist) and writes
/// `copilot.jsonl` — never claude's or codex's log. This exercises the real
/// `run_copilot_in` → `run_provider` path end-to-end against a stand-in binary;
/// without it the `Backend::Copilot` exec arm would be dead code no test runs.
#[tokio::test]
async fn copilot_backend_takes_copilot_path() {
    let tmp = TempDir::new().expect("tmp");
    let env = exec_env_in(tmp.path());
    let (runner, marker) = runner_with_all(tmp.path());

    // A copilot agent's provider wire name resolves to the copilot backend.
    let backend = Backend::from_provider("copilot");
    assert_eq!(backend, Backend::Copilot);

    let outcome = dispatch(&runner, backend, &env).await;
    assert!(matches!(outcome, RunOutcome::Success(_)));

    // The COPILOT BINARY ran. This is the assertion that actually bites: the log
    // file below is chosen by copilot_spec, so a run_copilot_in that wrongly used
    // cfg.claude_path would still write copilot.jsonl and pass without this.
    assert_eq!(
        spawned(&marker),
        vec!["fake-copilot.sh"],
        "copilot backend must spawn the COPILOT binary, not another provider's"
    );
    // The program the runner would exec is the configured copilot path.
    let (program, _argv) = runner.provider_command(
        Backend::Copilot,
        &invocation("do the thing"),
        Mode::Headless,
    );
    assert_eq!(
        program.file_name().unwrap(),
        "fake-copilot.sh",
        "provider_command must name the copilot binary"
    );

    // Positive proof the copilot exec path ran…
    assert!(
        env.logs.join("copilot.jsonl").exists(),
        "copilot backend must take run_copilot (writes copilot.jsonl)"
    );
    // …and negative proof it did not fall through to either other provider (the
    // silent claude fallback this branch removed).
    assert!(
        !env.logs.join("claude.jsonl").exists(),
        "copilot backend must NOT fall through to run_claude (no claude.jsonl)"
    );
    assert!(
        !env.logs.join("codex.jsonl").exists(),
        "copilot backend must NOT take the codex path (no codex.jsonl)"
    );
}

/// The copilot argv carries the flags a REAL non-interactive copilot run needs
/// (verified against GitHub Copilot CLI 1.0.68): `--allow-all-tools` is
/// documented "required for non-interactive mode", and `--model` is threaded from
/// the agent's config when set.
#[test]
fn copilot_command_carries_verified_non_interactive_flags() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _marker) = runner_with_all(tmp.path());

    let (_program, argv) = runner.provider_command(
        Backend::Copilot,
        &invocation("do the thing"),
        Mode::Headless,
    );
    assert!(
        argv.contains(&"--allow-all-tools".to_string()),
        "copilot needs --allow-all-tools for non-interactive mode: {argv:?}"
    );
    // The BRIEF must actually reach copilot. Asserted explicitly because a fixture
    // that merely SETS a prompt proves nothing: this test used to pass a prompt and
    // check only the flags, so deleting copilot_spec's prompt push left the whole
    // suite green while every real copilot run did nothing.
    assert!(
        argv.join(" ").contains("-p do the thing"),
        "copilot must receive the brief as the value of -p: {argv:?}"
    );

    // The agent's configured model is threaded (copilot DOES support --model).
    let invocation = ProviderInvocation {
        prompt: "do the thing".to_string(),
        model: Some("gpt-5.4".to_string()),
        cli_args: vec!["--add-dir".to_string(), "/tmp/x".to_string()],
    };
    let (_program, argv) = runner.provider_command(Backend::Copilot, &invocation, Mode::Headless);
    let joined = argv.join(" ");
    assert!(
        joined.contains("--model gpt-5.4"),
        "model must be threaded: {argv:?}"
    );
    assert!(
        joined.contains("--add-dir /tmp/x"),
        "cli_args must be appended: {argv:?}"
    );
}

/// Each provider's argv must be the shape that ACTUALLY runs headless — verified
/// against the real CLIs (claude 2.1.210, codex-cli 0.144.0, Copilot CLI 1.0.70):
///
/// * claude needs `-p` or it opens a session and exits 1 on the daemon's null stdin,
///   AND a permission flag or every tool call is denied while the process still
///   exits 0 — a silent no-op the daemon scores as `done`. This assertion formerly
///   pinned a bare `-p BRIEF`: that shape was only ever exercised against no-tool
///   briefs ("reply OK"), which is exactly why the gap went unseen. The flag is
///   `--dangerously-skip-permissions` by operator decision (blanket tool autonomy,
///   including `Bash`); the narrower `acceptEdits` measurably denies `Bash`.
/// * codex needs `--skip-git-repo-check` or it exits 1 ("Not inside a trusted
///   directory") in the daemon's non-repo in-tree workdir, and takes the prompt as a
///   TRAILING positional,
/// * copilot needs `-p` + `--allow-all-tools` ("required for non-interactive mode").
///
/// claude and codex now fence the brief behind `--`, because their briefs are
/// POSITIONALS and issue text can start with `-` (see
/// `dash_leading_brief_is_delivered_as_text_to_every_provider`). Copilot's is a
/// flag VALUE and must NOT be fenced. Verified to compose:
/// `codex exec --skip-git-repo-check -- "-fix the login bug"` parses and runs.
#[test]
fn every_provider_argv_is_the_verified_headless_shape() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _m) = runner_with_all(tmp.path());
    let inv = ProviderInvocation {
        prompt: "BRIEF".to_string(),
        model: None,
        cli_args: Vec::new(),
    };

    let (_p, claude) = runner.provider_command(Backend::Claude, &inv, Mode::Headless);
    assert_eq!(
        claude,
        vec![
            "-p",
            "--dangerously-skip-permissions",
            "--output-format",
            "stream-json",
            "--verbose",
            "--",
            "BRIEF"
        ],
        "claude: -p --dangerously-skip-permissions --output-format stream-json --verbose \
         -- <brief> (the stream-json flags — bead 48c — make claude emit the structured \
         terminal event the daemon finalizes on; stream-json requires --verbose under -p)"
    );

    let (_p, codex) = runner.provider_command(Backend::Codex, &inv, Mode::Headless);
    assert_eq!(
        codex,
        vec![
            "exec",
            "--skip-git-repo-check",
            "-s",
            "danger-full-access",
            "--json",
            "--",
            "BRIEF"
        ],
        "codex: exec --skip-git-repo-check -s danger-full-access --json -- <brief> \
         (--json — bead 48c — emits the structured turn.completed/turn.failed terminal; \
         prompt is a trailing positional)"
    );

    let (_p, copilot) = runner.provider_command(Backend::Copilot, &inv, Mode::Headless);
    assert_eq!(
        copilot,
        vec!["-p", "BRIEF", "--allow-all-tools"],
        "copilot: -p <brief> --allow-all-tools"
    );
}

/// The INTERACTIVE argv must never be the headless one.
///
/// `Mode::Interactive` backs an attachable tmux pane the operator drives, so the
/// flags that mean "do it and exit" are exactly wrong there — but they are the
/// same providers and the same builders, so one shared argv silently served both
/// contracts and shipped a print-and-exit process into a live pane. Shapes
/// verified against the real binaries (claude 2.1.210 / codex-cli 0.144.0 /
/// Copilot CLI 1.0.68).
#[test]
fn interactive_argv_seeds_a_real_session_never_print_and_exit() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _marker) = runner_with_all(tmp.path());
    let inv = invocation("do the thing");

    // claude: `-p/--print` is "Print response and exit" — fatal for a pane the
    // user attaches to. The brief still arrives, as the same trailing positional.
    let (_p, argv) = runner.provider_command(Backend::Claude, &inv, Mode::Interactive);
    assert!(
        !argv.contains(&"-p".to_string()),
        "interactive claude must NOT print-and-exit: {argv:?}"
    );
    assert!(
        argv.ends_with(&["--".to_string(), "do the thing".to_string()]),
        "interactive claude must still carry the brief: {argv:?}"
    );
    // The headless counterpart is the one that DOES print and exit.
    let (_p, argv) = runner.provider_command(Backend::Claude, &inv, Mode::Headless);
    assert!(
        argv.contains(&"-p".to_string()),
        "headless claude must print-and-exit: {argv:?}"
    );

    // codex: `exec` is the non-interactive subcommand; the interactive TUI is the
    // bare top-level command with the brief as its opening positional.
    let (_p, argv) = runner.provider_command(Backend::Codex, &inv, Mode::Interactive);
    assert!(
        !argv.contains(&"exec".to_string()),
        "interactive codex must NOT use the exec subcommand: {argv:?}"
    );
    assert!(
        argv.ends_with(&["--".to_string(), "do the thing".to_string()]),
        "interactive codex must still carry the brief: {argv:?}"
    );
    let (_p, argv) = runner.provider_command(Backend::Codex, &inv, Mode::Headless);
    assert_eq!(argv.first().map(String::as_str), Some("exec"));

    // copilot: `-p` "exits after completion"; `-i` "Start interactive mode and
    // automatically execute this prompt".
    let (_p, argv) = runner.provider_command(Backend::Copilot, &inv, Mode::Interactive);
    assert!(
        !argv.contains(&"-p".to_string()),
        "interactive copilot must NOT use the exits-after-completion flag: {argv:?}"
    );
    assert!(
        argv.join(" ").contains("-i do the thing"),
        "interactive copilot must seed the session with the brief via -i: {argv:?}"
    );
}

/// A brief is arbitrary issue text, so it can start with `-` (an issue titled
/// `- fix the login bug` is ordinary bullet prose). It must reach the provider as
/// TEXT, never as flags.
///
/// Verified against the real binaries: `codex exec "-fix the login bug"` →
/// "unexpected argument '-f' found"; `claude -p "-reply …"` silently absorbs the
/// leading `-r` as its own `--resume` and then fails on the remainder. Both parse
/// correctly once the brief sits after `--`. Copilot is the exception BY DESIGN —
/// its brief is a flag VALUE, which takes a dash-leading string verbatim, and a
/// `--` there would be consumed AS the value and break it.
#[test]
fn dash_leading_brief_is_delivered_as_text_to_every_provider() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _marker) = runner_with_all(tmp.path());
    let brief = "-fix the login bug";
    let inv = invocation(brief);

    for mode in [Mode::Headless, Mode::Interactive] {
        // claude + codex: the brief is a positional, so it MUST be fenced off by
        // the end-of-options separator immediately before it.
        for backend in [Backend::Claude, Backend::Codex] {
            let (_p, argv) = runner.provider_command(backend, &inv, mode);
            assert!(
                argv.ends_with(&["--".to_string(), brief.to_string()]),
                "{backend:?}/{mode:?} must fence a dash-leading brief behind `--`: {argv:?}"
            );
        }

        // copilot: the brief rides a value-taking flag and must NOT be fenced —
        // `copilot -p -- "-fix…"` makes `--` the prompt and rejects the brief.
        let (_p, argv) = runner.provider_command(Backend::Copilot, &inv, mode);
        assert!(
            !argv.contains(&"--".to_string()),
            "copilot must NOT get an `--` separator (it would become the prompt value): {argv:?}"
        );
        let flag = if mode == Mode::Headless { "-p" } else { "-i" };
        assert_eq!(
            argv.iter().position(|a| a == brief),
            argv.iter().position(|a| a == flag).map(|i| i + 1),
            "copilot/{mode:?} must pass the dash-leading brief as {flag}'s value: {argv:?}"
        );
    }
}

/// A brief that happens to spell a subcommand is still a brief.
///
/// Verified: `codex exec review` hijacks codex's `review` subcommand and dies
/// ("Specify --uncommitted, --base, --commit, or provide custom review
/// instructions"); `codex exec -- review` treats it as the prompt.
#[test]
fn brief_that_names_a_subcommand_does_not_hijack_it() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _marker) = runner_with_all(tmp.path());
    let (_p, argv) = runner.provider_command(Backend::Codex, &invocation("review"), Mode::Headless);
    assert_eq!(
        argv,
        vec![
            "exec".to_string(),
            "--skip-git-repo-check".to_string(),
            "-s".to_string(),
            "danger-full-access".to_string(),
            "--json".to_string(),
            "--".to_string(),
            "review".to_string()
        ],
        "a `review` brief must be codex's PROMPT, not its review subcommand: {argv:?}"
    );
}

/// A value-taking flag in the agent's `cli_args` must not swallow the brief.
///
/// `cli_args` are appended verbatim before the prompt, so a trailing flag that
/// expects a value would eat the brief as that value. The `--` separator ends
/// option parsing first, so the brief stays a positional.
#[test]
fn value_taking_cli_arg_cannot_swallow_the_brief() {
    let tmp = TempDir::new().expect("tmp");
    let (runner, _marker) = runner_with_all(tmp.path());
    let inv = ProviderInvocation {
        prompt: "do the thing".to_string(),
        model: None,
        // A flag whose value the agent forgot to supply.
        cli_args: vec!["--add-dir".to_string()],
    };
    let (_p, argv) = runner.provider_command(Backend::Codex, &inv, Mode::Headless);
    assert_eq!(
        argv,
        vec![
            "exec".to_string(),
            "--skip-git-repo-check".to_string(),
            "-s".to_string(),
            "danger-full-access".to_string(),
            "--json".to_string(),
            "--add-dir".to_string(),
            "--".to_string(),
            "do the thing".to_string()
        ],
        "`--` must separate cli_args from the brief: {argv:?}"
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
