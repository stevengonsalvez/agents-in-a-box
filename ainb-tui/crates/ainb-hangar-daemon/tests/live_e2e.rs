//! The first REAL, no-mock, end-to-end tripwire for the Hangar dispatch chain.
//!
//! Every other "e2e" test in this crate spawns a `fake-claude.sh` that emits a
//! canned `result` line and exits 0 — which proves ROUTING (the FSM walks
//! `queued → … → done`) but NEVER proves the daemon can invoke a real agent and
//! get real work out of it. A fake binary exits 0 by construction, so it cannot
//! catch the class of bug where the daemon spawns `claude` with a wrong argv /
//! null stdin and the process does nothing yet the task is marked `done`.
//!
//! This tripwire closes that gap. It drives the GENUINE user path through the
//! REAL CLI verbs against the REAL `claude` binary:
//!
//! ```text
//!  ainb hangar agent create ─▶ agent edit --model haiku
//!         │
//!         ▼
//!  ainb hangar issue create --assign <agent>  ──▶ queued task
//!         │                                            │
//!         ▼                              real ainb-hangar-daemon (this binary)
//!  claim ─▶ dispatch ─▶ `claude -p --model haiku -- <brief>` ─▶ done
//!                                            │
//!                              agent runs Bash: writes NONCE → file in cwd
//! ```
//!
//! # The testing law this enforces
//!
//! `exit 0` is NOT success — agent CLIs exit 0 on refusals, denials and empty
//! runs. So the success signal is NOT the task status: it is an OBSERVABLE SIDE
//! EFFECT the model cannot fabricate by printing text. The brief instructs the
//! agent to run a shell command that writes a specific NONCE (handed to it in
//! the brief, so it cannot be guessed) to a file in its workdir. The assertion
//! reads that file and checks the exact nonce FIRST; `task = done` is only
//! trusted as a cross-check because the artifact already proved real work.
//!
//! A task that reaches a terminal state WITHOUT the artifact fails loudly here —
//! that is precisely the bug class ("no-op task marked done", bead 48d) the
//! fake-claude suite could never surface.
//!
//! # Feature gate + skip contract
//!
//! Behind the `live-e2e` cargo feature, so a plain `cargo test` never runs it
//! (no spend, no provider dependency). It SKIPS CLEAN (returns without failing)
//! when the `ainb` binary is absent or `claude` is not on PATH / not
//! authenticated — a skip must never read as a pass, so each skip prints a loud
//! `SKIPPED:` line.
//!
//! # Sandbox
//!
//! Sandbox is OFF for this P0 (`HANGAR_DAEMON_DISABLE_SANDBOX=1`). Running the
//! real provider INSIDE the Seatbelt/Landlock FS sandbox is a later phase that
//! needs the credential-passing work; this tripwire deliberately exercises the
//! unconfined dispatch path only.
//!
//! # Mutation self-check
//!
//! Set `LIVE_E2E_BREAK_CLAUDE=1` to point the daemon at a no-op stand-in that
//! emits the pinned `system`+`result` JSONL and exits 0 WITHOUT writing the
//! nonce file — i.e. a provider that reaches `done` cleanly having done no real
//! work (bead 48d's exact bug class). The test then goes RED on the MISSING
//! NONCE while the task status is `done`, proving the artifact assertion — not
//! the FSM status — is the trusted signal.

#![cfg(feature = "live-e2e")]
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

/// The file the agent is told to write its nonce into, relative to its workdir.
const ARTIFACT_NAME: &str = "hangar_live_nonce.txt";

/// Terminal task statuses — the poll stops on any of these, THEN we assert the
/// artifact (so a `failed`/`cancelled` run is caught on the missing nonce, not a
/// poll timeout).
const TERMINAL: [&str; 3] = ["done", "failed", "cancelled"];

/// Wall-clock budget for a real haiku call to claim → dispatch → run → finalize.
/// A tiny "write this nonce" brief is seconds of model time; 180s absorbs a cold
/// auth handshake without ever hanging the suite.
const TASK_BUDGET: Duration = Duration::from_secs(180);

#[tokio::test]
async fn live_dispatch_writes_nonce_artifact() {
    // ---- Skip gates (a skip prints LOUD and returns clean — never a pass) ----
    let Some(ainb) = ainb_bin() else {
        eprintln!("SKIPPED: ainb binary not built (run `cargo build -p ainb --bin ainb`)");
        return;
    };
    let Some(claude) = real_claude() else {
        eprintln!("SKIPPED: no authenticated claude on PATH");
        return;
    };
    if !claude_alive(&claude) {
        eprintln!("SKIPPED: no authenticated claude on PATH");
        return;
    }

    // A unique nonce + agent name per run so parallel/rerun invocations never
    // collide and the nonce is unguessable text handed to the model via the brief.
    let tag = format!("{}-{}", std::process::id(), now_ms());
    let nonce = format!("HANGAR-LIVE-NONCE-{tag}");
    let agent_name = format!("live-e2e-{tag}");

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Create the agent via the real CLI. This is the daemon-less path: it
    //    bootstraps the default workspace + a claude runtime + the agent, and
    //    migrates the store db under AINB_HANGAR_HOME — all before the daemon runs.
    run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar",
            "agent",
            "create",
            "--name",
            &agent_name,
            "--provider",
            "claude",
        ],
    );

    // The DB now exists; open a WAL reader for the id lookup + terminal poll.
    let pool = open_pool(&db_path).await;
    let (agent_id, runtime_id) = agent_ids_by_name(&pool, &agent_name).await;

    // 2. Pin the cheapest live model. `claude-3-5-haiku-*` is retired; the
    //    `haiku` alias tracks the current cheap model. The runner threads this as
    //    `--model haiku` onto the real argv.
    run_ainb(
        &ainb,
        home.path(),
        &["hangar", "agent", "edit", &agent_id, "--model", "haiku"],
    );

    // 3. Spawn the REAL daemon binary as a plain child. It INHERITS this
    //    process's environment (so the spawned claude sees the real $HOME and is
    //    authenticated) and overrides only the hangar knobs. The mutation switch
    //    swaps claude for a no-op stand-in that reaches `done` WITHOUT writing the
    //    nonce, so the RED path is proven on the missing artifact (not task state).
    let effective_claude = if std::env::var_os("LIVE_E2E_BREAK_CLAUDE").is_some() {
        eprintln!(
            "MUTATION: LIVE_E2E_BREAK_CLAUDE set — daemon uses a no-op claude that reaches \
             `done` without writing the nonce"
        );
        write_noop_claude(home.path())
    } else {
        claude.clone()
    };
    let daemon = LiveDaemon::spawn(
        &home.path().join("daemon.log"),
        home.path(),
        &runtime_id,
        &effective_claude,
    );

    // 4. Enqueue via the real user path. `issue create --assign <agent>` inserts
    //    the issue AND a queued task for the agent's runtime, and echoes
    //    `queued task <id>`. The issue title BECOMES the provider prompt
    //    (run_loop::build_prompt = title [+ description]).
    let brief = format!(
        "Use the Bash tool to run exactly this one shell command and nothing else: \
         printf '%s' '{nonce}' > {ARTIFACT_NAME}  \
         Do not use the Write tool. Once the command has run, stop."
    );
    let stdout = run_ainb_capture(
        &ainb,
        home.path(),
        &[
            "hangar", "issue", "create", "--title", &brief, "--assign", &agent_id,
        ],
    );
    let task_id = parse_queued_task_id(&stdout);

    // 5. Poll for a terminal state, then STOP the daemon by its exact pid before
    //    asserting (no wildcard / pkill).
    let row = wait_for_terminal(&pool, &task_id, TASK_BUDGET, home.path()).await;
    let status: String = row.get("status");
    drop(daemon);

    // 6. THE side-effect assertion, FIRST. The nonce artifact must exist in the
    //    task's recorded workdir with the exact bytes. This is the only trusted
    //    success signal; a terminal task with no artifact is the exact bug class.
    let work_dir: Option<String> = row.get("work_dir");
    let work_dir = work_dir.unwrap_or_else(|| {
        panic!(
            "task {task_id} reached status={status} but recorded NO work_dir — \
             cannot even locate where the agent should have written. daemon log:\n{}",
            read_log(home.path())
        )
    });
    let artifact = Path::new(&work_dir).join(ARTIFACT_NAME);
    let got = std::fs::read_to_string(&artifact).unwrap_or_else(|e| {
        panic!(
            "NONCE ARTIFACT MISSING at {artifact:?} ({e}); task status={status}. \
             A task that reaches a terminal state WITHOUT the artifact is the exact \
             bug class this tripwire exists to catch (exit 0 / done != real work). \
             daemon log:\n{}",
            read_log(home.path())
        )
    });
    assert_eq!(
        got.trim(),
        nonce,
        "artifact exists but nonce mismatch — the agent wrote the WRONG bytes"
    );

    // 7. Only NOW is the task status trustworthy: the artifact proved real work,
    //    so `done` must be the terminal the FSM reached.
    assert_eq!(
        status,
        "done",
        "nonce artifact is present and correct, but the task status is {status:?}, not done — \
         a finalize-path inconsistency. daemon log:\n{}",
        read_log(home.path())
    );

    // 8. bead 48c: `--output-format stream-json` makes claude emit the structured
    //    terminal the runner pins session_id + usage from. Before it, `claude -p`
    //    printed ~5 bytes of plain text and BOTH were silently `None` despite the
    //    runner's session-pin doc promising otherwise. A real success must now
    //    populate them end-to-end — asserted on the DB row + the usage table, the
    //    live proof 48c is closed.
    let session_id: Option<String> = row.get("session_id");
    assert!(
        session_id.as_deref().is_some_and(|s| !s.is_empty()),
        "48c: a real claude success must persist a session_id (got {session_id:?}); it was None \
         while claude emitted plain text. daemon log:\n{}",
        read_log(home.path())
    );
    let usage = fetch_usage(&pool, &task_id).await.unwrap_or_else(|| {
        panic!(
            "48c: a real claude success must record token usage in task_usage for {task_id}; \
             usage was None while claude emitted plain text. daemon log:\n{}",
            read_log(home.path())
        )
    });
    assert!(
        usage.output_tokens > 0,
        "48c: recorded usage must carry real output tokens, got {usage:?}"
    );

    eprintln!(
        "LIVE E2E OK: task {task_id} done; nonce artifact verified at {artifact:?}; \
         48c session_id={session_id:?} usage={usage:?}"
    );
}

/// The codex leg of the same tripwire: the REAL daemon dispatching the REAL
/// `codex` binary through the REAL CLI path, proven by an on-disk nonce artifact.
///
/// Identical law to [`live_dispatch_writes_nonce_artifact`], one provider over:
///
/// ```text
///  ainb hangar agent create --provider codex
///         │
///         ▼
///  ainb hangar issue create --assign <agent>  ──▶ queued task
///         │                                            │
///         ▼                              real ainb-hangar-daemon (this binary)
///  claim ─▶ dispatch ─▶ `codex exec --skip-git-repo-check -s danger-full-access
///                        -- <brief>` ─▶ done
///                                            │
///                              agent runs a shell command: writes NONCE → file
/// ```
///
/// # Two codex facts this leg pins that a fake could never surface
///
/// 1. **No model flag.** codex-cli 0.144.0 runs `codex exec` on its default model
///    with no `-m`, so — unlike the claude leg's `--model haiku` pin — no
///    `agent edit --model` step is needed (verified: a bare `codex exec -- …`
///    ran and exited 0). Threading a guessed model id would be the bug.
/// 2. **The sandbox flag is load-bearing.** `codex exec` DEFAULTS to a read-only
///    sandbox; before the `-s danger-full-access` fix (this branch), a codex task
///    ran a shell tool yet its write was silently dropped and the task still
///    exited 0 — a `done` with no artifact. This leg is the live proof of that
///    fix: the nonce lands only because the daemon now pins the sandbox policy.
///
/// # Mutation self-check
///
/// `LIVE_E2E_BREAK_CODEX=1` points the daemon at a no-op codex stand-in that
/// exits 0 WITHOUT writing the nonce — a provider that reaches `done` having done
/// no real work. The test then goes RED on the MISSING ARTIFACT (not on task
/// state), proving the artifact — not the FSM status — is the trusted signal.
#[tokio::test]
async fn live_dispatch_codex_writes_nonce_artifact() {
    // ---- Skip gates (a skip prints LOUD and returns clean — never a pass) ----
    let Some(ainb) = ainb_bin() else {
        eprintln!("SKIPPED: ainb binary not built (run `cargo build -p ainb --bin ainb`)");
        return;
    };
    let Some(codex) = real_codex() else {
        eprintln!("SKIPPED: no codex binary on PATH");
        return;
    };
    if !codex_alive(&codex) {
        eprintln!(
            "SKIPPED: codex on PATH is not authenticated / did not answer the liveness probe"
        );
        return;
    }

    let tag = format!("{}-{}", std::process::id(), now_ms());
    let nonce = format!("HANGAR-LIVE-CODEX-NONCE-{tag}");
    let agent_name = format!("live-e2e-codex-{tag}");

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // 1. Create a CODEX agent via the real CLI. No `agent edit --model` follows:
    //    codex runs on its default model with no `-m`, so pinning one is neither
    //    needed nor desirable here (see the fn doc).
    run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar",
            "agent",
            "create",
            "--name",
            &agent_name,
            "--provider",
            "codex",
        ],
    );

    let pool = open_pool(&db_path).await;
    let (agent_id, runtime_id) = agent_ids_by_name(&pool, &agent_name).await;

    // 2. Spawn the REAL daemon pointed at the real codex (or the mutation stand-in
    //    that reaches `done` WITHOUT writing the nonce).
    let effective_codex = if std::env::var_os("LIVE_E2E_BREAK_CODEX").is_some() {
        eprintln!(
            "MUTATION: LIVE_E2E_BREAK_CODEX set — daemon uses a no-op codex that reaches \
             `done` without writing the nonce"
        );
        write_noop_codex(home.path())
    } else {
        codex.clone()
    };
    let daemon = LiveDaemon::spawn_codex(
        &home.path().join("daemon.log"),
        home.path(),
        &runtime_id,
        &effective_codex,
    );

    // 3. Enqueue via the real user path. The brief tells codex to write the nonce
    //    with a shell command — exercising codex's REAL tool use, not a fabricated
    //    echo. The nonce is unguessable text handed to it here.
    let brief = format!(
        "Run exactly this one shell command and nothing else: \
         printf '%s' '{nonce}' > {ARTIFACT_NAME}  \
         Once the command has run, stop."
    );
    let stdout = run_ainb_capture(
        &ainb,
        home.path(),
        &[
            "hangar", "issue", "create", "--title", &brief, "--assign", &agent_id,
        ],
    );
    let task_id = parse_queued_task_id(&stdout);

    // 4. Poll to terminal, STOP the daemon by exact pid, THEN assert the artifact.
    let row = wait_for_terminal(&pool, &task_id, TASK_BUDGET, home.path()).await;
    let status: String = row.get("status");
    drop(daemon);

    // 5. THE side-effect assertion, FIRST. A codex task that reaches a terminal
    //    state WITHOUT the artifact fails here — exactly the read-only-sandbox bug
    //    class (done, exit 0, no write) this leg exists to catch.
    let work_dir: Option<String> = row.get("work_dir");
    let work_dir = work_dir.unwrap_or_else(|| {
        panic!(
            "codex task {task_id} reached status={status} but recorded NO work_dir. \
             daemon log:\n{}",
            read_log(home.path())
        )
    });
    let artifact = Path::new(&work_dir).join(ARTIFACT_NAME);
    let got = std::fs::read_to_string(&artifact).unwrap_or_else(|e| {
        panic!(
            "NONCE ARTIFACT MISSING at {artifact:?} ({e}); codex task status={status}. \
             A codex task that reaches a terminal state WITHOUT the artifact is the exact \
             bug class this tripwire exists to catch (exit 0 / done != real work — and, \
             specifically here, codex's default read-only exec sandbox dropping the write). \
             daemon log:\n{}",
            read_log(home.path())
        )
    });
    assert_eq!(
        got.trim(),
        nonce,
        "artifact exists but nonce mismatch — codex wrote the WRONG bytes"
    );

    // 6. Only NOW is the status trustworthy: the artifact proved real work.
    assert_eq!(
        status,
        "done",
        "nonce artifact is present and correct, but the codex task status is {status:?}, not \
         done — a finalize-path inconsistency. daemon log:\n{}",
        read_log(home.path())
    );

    eprintln!("LIVE E2E CODEX OK: task {task_id} done; nonce artifact verified at {artifact:?}");
}

/// bead 48d (the DB-row proof): the REAL daemon must NOT mark a task `done` when
/// the provider EXITS 0 but its structured stream reports non-success.
///
/// ```text
///  issue create ──▶ queued task
///        │                    │
///        ▼         real ainb-hangar-daemon (this binary)
///  claim ─▶ dispatch ─▶ stand-in claude: emits `result subtype=error_max_turns`
///                        then `exit 0`   ─▶ status=failed reason=iteration_limit
/// ```
///
/// # Why a stand-in, not the real CLI, for THIS leg
///
/// Empirically (verified against Claude Code 2.1.211 / codex-cli 0.144.0), both
/// real CLIs exit *non-zero* on their hard failures: claude `--max-turns 1`
/// exits 1, codex `turn.failed` exits 1. A refusal exits 0 but self-reports
/// `subtype:"success"` (a SEMANTIC problem, out of scope here). So the exact 48d
/// hole — a provider that EXITS 0 while its OWN terminal reports an error — cannot
/// be induced with the live CLI; a stand-in emitting claude's real
/// `error_max_turns` result and exiting 0 is the only way to exercise it. The
/// daemon under test is REAL, and the assertion is the DB ROW (status +
/// failure_reason), never a log line.
///
/// # Mutation self-check
///
/// Revert the finalize-on-structured-result change in `runner::run_provider`
/// (the `match terminal { … }` block) and this task reaches `done` — the exit-0
/// fallback scores it `Success` — turning the `failed` + `iteration_limit`
/// assertions RED. Restore → green. That flip IS the bug 48c/48d closes.
#[tokio::test]
async fn live_exit0_structured_error_finalizes_failed_not_done() {
    let Some(ainb) = ainb_bin() else {
        eprintln!("SKIPPED: ainb binary not built (run `cargo build -p ainb --bin ainb`)");
        return;
    };
    // No claude/PATH/auth gate: the provider is a deterministic stand-in and the
    // DAEMON is real, so this leg runs whenever the live-e2e feature + binaries
    // are built (no spend, no provider dependency).
    let tag = format!("{}-{}", std::process::id(), now_ms());
    let agent_name = format!("live-e2e-exit0err-{tag}");
    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar",
            "agent",
            "create",
            "--name",
            &agent_name,
            "--provider",
            "claude",
        ],
    );
    let pool = open_pool(&db_path).await;
    let (agent_id, runtime_id) = agent_ids_by_name(&pool, &agent_name).await;

    // A stand-in `claude` that emits claude's REAL max-turns terminal, then exits
    // 0 — the exact exit-0-structured-failure shape the live CLI never produces.
    let standin = write_exit0_structured_error_claude(home.path());
    let daemon = LiveDaemon::spawn(
        &home.path().join("daemon.log"),
        home.path(),
        &runtime_id,
        &standin,
    );

    let stdout = run_ainb_capture(
        &ainb,
        home.path(),
        &[
            "hangar",
            "issue",
            "create",
            "--title",
            "structured-error tripwire brief",
            "--assign",
            &agent_id,
        ],
    );
    let task_id = parse_queued_task_id(&stdout);

    let row = wait_for_terminal(&pool, &task_id, TASK_BUDGET, home.path()).await;
    let status: String = row.get("status");
    let reason: Option<String> = row.get("failure_reason");
    drop(daemon);

    assert_eq!(
        status,
        "failed",
        "the provider EXITED 0 but reported error_max_turns — the task must be `failed`, not \
         `done` over no work (bead 48d). daemon log:\n{}",
        read_log(home.path())
    );
    assert_eq!(
        reason.as_deref(),
        Some("iteration_limit"),
        "error_max_turns must persist the retry-fresh iteration_limit reason, got {reason:?}. \
         daemon log:\n{}",
        read_log(home.path())
    );

    eprintln!("LIVE E2E OK: exit-0 structured error → task {task_id} failed/iteration_limit");
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A real `ainb-hangar-daemon` child, killed by its EXACT pid on drop.
///
/// Unlike the tmux-hosted `DaemonSession`, this is a plain child that inherits
/// the test process's environment, so the claude the runner spawns resolves the
/// real `$HOME`/`~/.claude` credentials. Only the hangar knobs are overridden;
/// stdout+stderr are captured to a log file for post-mortem on failure.
struct LiveDaemon {
    child: std::process::Child,
}

impl LiveDaemon {
    /// Spawn the claude leg's daemon: overrides `HANGAR_CLAUDE_PATH`.
    fn spawn(log_path: &Path, home: &Path, runtime_id: &str, claude_path: &Path) -> Self {
        Self::spawn_with(
            log_path,
            home,
            runtime_id,
            &[("HANGAR_CLAUDE_PATH", claude_path)],
        )
    }

    /// Spawn the codex leg's daemon: overrides `HANGAR_CODEX_PATH` so the runner's
    /// `codex exec` path resolves the real codex binary (or the mutation stand-in).
    fn spawn_codex(log_path: &Path, home: &Path, runtime_id: &str, codex_path: &Path) -> Self {
        Self::spawn_with(
            log_path,
            home,
            runtime_id,
            &[("HANGAR_CODEX_PATH", codex_path)],
        )
    }

    /// The shared spawn core: one plain child that inherits this process's
    /// environment (so the spawned provider sees the real `$HOME` credentials),
    /// overriding only the hangar knobs plus the given provider-path env pairs.
    fn spawn_with(
        log_path: &Path,
        home: &Path,
        runtime_id: &str,
        provider_env: &[(&str, &Path)],
    ) -> Self {
        let log = std::fs::File::create(log_path).expect("create daemon log");
        let err = log.try_clone().expect("clone daemon log handle");
        let mut cmd = Command::new(daemon_bin());
        cmd.env("AINB_HANGAR_HOME", home)
            .env("HANGAR_DAEMON_RUNTIME_ID", runtime_id)
            .env("HANGAR_DAEMON_DISABLE_SANDBOX", "1")
            .env("HANGAR_DAEMON_POLL_MS", "200");
        for (key, path) in provider_env {
            cmd.env(key, path);
        }
        let child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(err))
            .spawn()
            .expect("spawn ainb-hangar-daemon");
        Self { child }
    }
}

impl Drop for LiveDaemon {
    fn drop(&mut self) {
        // Exact-pid kill only — never a process-name or wildcard kill.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate the freshly-built `ainb-hangar-daemon` binary under test.
fn daemon_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("ainb-hangar-daemon")
}

/// Locate the built `ainb` binary. `CARGO_BIN_EXE_ainb` is only defined for the
/// `ainb` crate's own tests; from here we walk `target/<profile>/ainb`.
fn ainb_bin() -> Option<PathBuf> {
    if let Some(p) = option_env!("CARGO_BIN_EXE_ainb") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    // .../target/<profile>/deps/<test-bin> → .../target/<profile>/ainb
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.parent()?.join("ainb");
    candidate.exists().then_some(candidate)
}

/// The real `claude` binary on PATH, if any.
fn real_claude() -> Option<PathBuf> {
    which::which("claude").ok()
}

/// The real `codex` binary on PATH, if any.
fn real_codex() -> Option<PathBuf> {
    which::which("codex").ok()
}

/// Write an executable no-op stand-in for `claude` used ONLY by the mutation
/// self-check. It emits the exact `system`+`result` JSONL the headless runner
/// pins (so the run finalizes cleanly to `done`) but writes NO nonce file — a
/// provider that reaches `done` having done no real work. The live artifact
/// assertion must catch this; if it did not, the whole tripwire would be
/// theatre. Returns the script path to hand the daemon as `HANGAR_CLAUDE_PATH`.
fn write_noop_claude(dir: &Path) -> PathBuf {
    let path = dir.join("noop-claude.sh");
    let body = "#!/bin/sh\n\
         echo '{\"type\":\"system\",\"session_id\":\"noop-mutation\"}'\n\
         echo '{\"type\":\"result\",\"content\":\"ok\"}'\n\
         exit 0\n";
    std::fs::write(&path, body).expect("write noop-claude");
    make_executable(&path);
    path
}

/// The codex counterpart of [`write_noop_claude`]: an executable no-op stand-in
/// for `codex` used ONLY by the codex mutation self-check. It emits codex's
/// structured `turn.completed` success terminal (bead 48c: the daemon now
/// finalizes a codex run on that event, not its exit code) but writes NO nonce
/// file — a provider that reaches `done` having done no real work. The live
/// artifact assertion must catch it. Returns the script path to hand the daemon
/// as `HANGAR_CODEX_PATH`.
fn write_noop_codex(dir: &Path) -> PathBuf {
    let path = dir.join("noop-codex.sh");
    let body = "#!/bin/sh\n\
         echo '{\"type\":\"thread.started\",\"thread_id\":\"noop-codex\"}'\n\
         echo '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n\
         exit 0\n";
    std::fs::write(&path, body).expect("write noop-codex");
    make_executable(&path);
    path
}

/// A stand-in `claude` for [`live_exit0_structured_error_finalizes_failed_not_done`]:
/// it emits claude's REAL `error_max_turns` terminal (bead 48d) and then exits 0
/// — the exit-0-structured-failure shape the live CLI never produces (real claude
/// exits 1 on max-turns). The daemon must finalize this `failed`/`iteration_limit`,
/// never `done`. Returns the script path to hand the daemon as `HANGAR_CLAUDE_PATH`.
fn write_exit0_structured_error_claude(dir: &Path) -> PathBuf {
    let path = dir.join("exit0-error-claude.sh");
    let body = "#!/bin/sh\n\
         echo '{\"type\":\"system\",\"session_id\":\"exit0-err\"}'\n\
         echo '{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"num_turns\":2}'\n\
         exit 0\n";
    std::fs::write(&path, body).expect("write exit0-error claude");
    make_executable(&path);
    path
}

/// A `task_usage` row's token/cost tallies, for the 48c capture assertion.
#[derive(Debug)]
struct UsageRow {
    #[allow(dead_code)]
    input_tokens: i64,
    output_tokens: i64,
    #[allow(dead_code)]
    cost_usd: f64,
}

/// Fetch the recorded usage for a task from `task_usage`, or `None` if the run
/// reported none (the pre-48c state for claude).
async fn fetch_usage(pool: &SqlitePool, task_id: &str) -> Option<UsageRow> {
    sqlx::query("SELECT input_tokens, output_tokens, cost_usd FROM task_usage WHERE task_id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .expect("query task_usage")
        .map(|r| UsageRow {
            input_tokens: r.get("input_tokens"),
            output_tokens: r.get("output_tokens"),
            cost_usd: r.get("cost_usd"),
        })
}

/// Mark a stand-in script executable (0o755) on unix so the daemon can spawn it.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("stat stand-in").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod stand-in");
    }
}

/// Liveness + auth probe: `claude -p "reply PONG"` under a short timeout. An
/// unauthenticated / missing claude fails fast, so the test SKIPs rather than
/// burning the full task budget on a doomed dispatch.
fn claude_alive(claude: &Path) -> bool {
    let Ok(out) = Command::new(claude)
        .args([
            "-p",
            "--model",
            "haiku",
            "--",
            "reply with the single word PONG",
        ])
        .output()
    else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).to_uppercase().contains("PONG")
}

/// Codex liveness + auth probe: `codex exec --skip-git-repo-check -- "reply
/// PONG"`, bounded by a hard wall-clock timeout. codex's non-interactive `exec`
/// never prompts (null stdin), so an unauthenticated codex errors instead of
/// hanging — but the probe is still bounded so a wedged CLI can never stall the
/// suite. Runs the child on a helper thread and joins with `recv_timeout`; a
/// timeout, spawn error, non-zero exit, or a reply without `PONG` all read as
/// "not alive" and drive a LOUD skip, never a pass. `exec`'s output is drained
/// via `output()` so a chatty codex cannot deadlock on a full stdout pipe.
fn codex_alive(codex: &Path) -> bool {
    let codex = codex.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(&codex)
            .args([
                "exec",
                "--skip-git-repo-check",
                "--",
                "reply with the single word PONG",
            ])
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(Ok(out)) => {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout).to_uppercase().contains("PONG")
        }
        _ => false,
    }
}

/// Run an `ainb` subcommand under the isolated hangar home; panic on non-zero.
fn run_ainb(ainb: &Path, home: &Path, args: &[&str]) {
    let out = Command::new(ainb)
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .output()
        .unwrap_or_else(|e| panic!("spawn ainb {args:?}: {e}"));
    assert!(
        out.status.success(),
        "ainb {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Run an `ainb` subcommand and return its stdout; panic on non-zero.
fn run_ainb_capture(ainb: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(ainb)
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .output()
        .unwrap_or_else(|e| panic!("spawn ainb {args:?}: {e}"));
    assert!(
        out.status.success(),
        "ainb {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Parse the `queued task <id>` line `issue create --assign` prints.
fn parse_queued_task_id(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("queued task "))
        .map(str::trim)
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("no `queued task <id>` line in issue-create output:\n{stdout}"))
}

/// Look up an agent's `(id, runtime_id)` by its unique name.
async fn agent_ids_by_name(pool: &SqlitePool, name: &str) -> (String, String) {
    let row = sqlx::query("SELECT id, runtime_id FROM agent WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("look up agent {name}: {e}"));
    (row.get("id"), row.get("runtime_id"))
}

/// Poll the task row until its status is terminal, or the budget elapses.
/// Dumps the daemon log into the panic on timeout so a CI failure is diagnosable.
async fn wait_for_terminal(
    pool: &SqlitePool,
    task_id: &str,
    budget: Duration,
    home: &Path,
) -> SqliteRow {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    loop {
        if let Some(row) = fetch_row(pool, task_id).await {
            let status: String = row.get("status");
            if TERMINAL.contains(&status.as_str()) {
                return row;
            }
            last = status;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} never reached a terminal status within {budget:?} (last={last:?}). \
             daemon log:\n{}",
            read_log(home)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Fetch the full task row, or `None` if absent.
async fn fetch_row(pool: &SqlitePool, task_id: &str) -> Option<SqliteRow> {
    sqlx::query("SELECT * FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .expect("query task row")
}

/// Open a `SQLite` WAL pool at `db_path` (the daemon is the writer; this is a reader).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}

/// Best-effort read of the captured daemon log for failure post-mortems.
fn read_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("daemon.log"))
        .unwrap_or_else(|e| format!("(no daemon log: {e})"))
}

/// Current wall-clock epoch milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
