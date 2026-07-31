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
/// The remote-repo + source-branch leg (0042, plans/hangar-task-agent-model.md
/// P3): the REAL daemon clones a REMOTE (`file://` bare) repo, provisions the
/// run's worktree off a NON-default `feature/x` source branch, and the REAL
/// claude writes the nonce INTO that worktree.
///
/// The assertion ladder, side-effects first:
///   1. a sentinel file that exists ONLY on `feature/x` is present in the
///      recorded work_dir → the worktree is based on the chosen source, not the
///      remote's default HEAD (the exact 0042 claim);
///   2. the default branch's HEAD-only file is ABSENT → not merely a superset;
///   3. the nonce artifact → the agent genuinely worked in that tree;
///   4. only then task=done.
///
/// Mutation-proof: `LIVE_E2E_BREAK_SOURCE_BRANCH=1` drops `--source-branch`
/// from the enqueue, so the worktree lands on the remote's default branch and
/// assertion 1 goes RED — proving this leg detects a wrong-base dispatch.
#[tokio::test]
async fn live_dispatch_remote_repo_source_branch() {
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

    let tag = format!("{}-{}", std::process::id(), now_ms());
    let nonce = format!("HANGAR-LIVE-NONCE-{tag}");
    let agent_name = format!("live-e2e-branch-{tag}");
    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // Build the REMOTE: a bare repo whose default branch holds `head-only.txt`
    // and whose `feature/x` branch holds `feature-sentinel.txt` instead.
    let remote_dir = tempfile::tempdir().expect("tempdir remote");
    let remote_url = make_branchy_bare_remote(remote_dir.path());

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
    run_ainb(
        &ainb,
        home.path(),
        &["hangar", "agent", "edit", &agent_id, "--model", "haiku"],
    );

    let daemon = LiveDaemon::spawn(
        &home.path().join("daemon.log"),
        home.path(),
        &runtime_id,
        &claude,
    );

    // Enqueue with the REMOTE repo + the feature source branch. The CLI clones
    // the remote into the shared cache and persists the LOCAL path + the branch
    // onto the issue AND the task row (one tx with the enqueue).
    let brief = format!(
        "Use the Bash tool to run exactly this one shell command and nothing else: \
         printf '%s' '{nonce}' > {ARTIFACT_NAME}  \
         Do not use the Write tool. Once the command has run, stop."
    );
    let break_source = std::env::var_os("LIVE_E2E_BREAK_SOURCE_BRANCH").is_some();
    if break_source {
        eprintln!(
            "MUTATION: LIVE_E2E_BREAK_SOURCE_BRANCH set — enqueue omits --source-branch, so \
             the worktree lands on the remote's default branch and the sentinel assert goes RED"
        );
    }
    let mut args: Vec<&str> = vec![
        "hangar",
        "issue",
        "create",
        "--title",
        &brief,
        "--assign",
        &agent_id,
        "--repo",
        &remote_url,
    ];
    if !break_source {
        args.extend_from_slice(&["--source-branch", "feature/x"]);
    }
    let stdout = run_ainb_capture(&ainb, home.path(), &args);
    let task_id = parse_queued_task_id(&stdout);

    let row = wait_for_terminal(&pool, &task_id, TASK_BUDGET, home.path()).await;
    let status: String = row.get("status");
    drop(daemon);

    let work_dir: Option<String> = row.get("work_dir");
    let work_dir = work_dir.unwrap_or_else(|| {
        panic!(
            "task {task_id} reached status={status} but recorded NO work_dir. daemon log:\n{}",
            read_log(home.path())
        )
    });
    let wd = Path::new(&work_dir);

    // 1+2. THE source-branch assertion, before anything else: the worktree must
    // hold feature/x's tree, not the default branch's.
    assert!(
        wd.join("feature-sentinel.txt").exists(),
        "SOURCE-BRANCH VIOLATION: the worktree at {wd:?} lacks feature/x's sentinel — \
         the run was based on the WRONG branch (status={status}). daemon log:\n{}",
        read_log(home.path())
    );
    assert!(
        !wd.join("head-only.txt").exists(),
        "the worktree holds the default branch's HEAD-only file — it is NOT feature/x's tree"
    );

    // 3. The agent's own artifact in that same tree.
    let artifact = wd.join(ARTIFACT_NAME);
    let got = std::fs::read_to_string(&artifact).unwrap_or_else(|e| {
        panic!(
            "NONCE ARTIFACT MISSING at {artifact:?} ({e}); status={status}. daemon log:\n{}",
            read_log(home.path())
        )
    });
    assert_eq!(got.trim(), nonce, "artifact exists but nonce mismatch");

    // 4. Only now is `done` trustworthy.
    assert_eq!(
        status, "done",
        "sentinel + nonce present but status={status:?}, not done"
    );

    eprintln!(
        "LIVE E2E BRANCH OK: task {task_id} done; worktree on feature/x verified at {wd:?}; \
         nonce verified; remote clone + source-branch dispatch proven"
    );
}

/// Build a bare "remote" with a default branch carrying `head-only.txt` and a
/// `feature/x` branch carrying `feature-sentinel.txt` instead, returning its
/// `file://` URL (so the CLI treats it as a REMOTE and exercises ensure_clone).
fn make_branchy_bare_remote(dir: &Path) -> String {
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("mk work");
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(&work).args(args).output().expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "t@e.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(work.join("README.md"), "base").expect("write");
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "base"]);
    // feature/x forks BEFORE the default branch gains its HEAD-only file.
    git(&["branch", "feature/x"]);
    std::fs::write(work.join("head-only.txt"), "on-default").expect("write");
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "default work"]);
    git(&["switch", "--quiet", "feature/x"]);
    std::fs::write(work.join("feature-sentinel.txt"), "on-feature").expect("write");
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "feature work"]);
    git(&["switch", "--quiet", "-"]);

    let bare = dir.join("remote.git");
    let out = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ])
        .output()
        .expect("bare clone");
    assert!(
        out.status.success(),
        "bare clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    format!("file://{}", bare.display())
}

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

// ---------------------------------------------------------------------------
// LIVE PIPELINE PROOF: one card, four role-gated stages, three REAL agents.
// ---------------------------------------------------------------------------

/// THE LIVE END-TO-END PROOF of the role-gated pull pipeline (goal criterion 1).
///
/// One issue traverses the pipeline across THREE DIFFERENT agents driving TWO
/// REAL provider CLIs, and the DB is asserted at every transition:
///
/// ```text
///  Triage        Implement       Review          QA            Done
///  triager       implementer     reviewer        tester        (terminal)
///  pipe-triage   pipe-triage     pipe-review     pipe-qa
///  [claude]      [claude]        [codex]         [claude]
///        \___________/                 excl. prior    excl. prior
///         same agent may                    \______________/
///         implement what it                  a checker is never
///         triaged                            the agent it checks
/// ```
///
/// The assertions, sampled on EVERY poll rather than once at the end, are:
///   * never more than ONE `running` task on the card at any instant,
///   * the Review stage's `agent_id` differs from the Implement stage's,
///   * the `parent_task_id` chain is unbroken across stages,
///   * `board_card.column_id` advances ONE column at a time, never skipping.
///
/// # Why fakes are not permitted here
///
/// A scripted stub proves ROUTING and never invocation shape. That is exactly
/// how a broken headless path survived undetected in this repo for weeks: the
/// fake exits 0 by construction, so `queued -> done` walked perfectly while the
/// real CLI was never correctly invoked. So the success signal is not the task
/// status. It is a per-stage NONCE ARTIFACT the model cannot fabricate without
/// actually running a tool, written into that stage's OWN worktree, plus a
/// recorded `task_usage` row proving real tokens were burned.
#[tokio::test]
async fn live_pipeline_walks_four_stages_across_three_real_agents() {
    let Some(ainb) = ainb_bin() else {
        eprintln!("SKIPPED: ainb binary not built (run `cargo build -p ainb --bin ainb`)");
        return;
    };
    let Some(claude) = real_claude() else {
        eprintln!("SKIPPED: no claude on PATH");
        return;
    };
    let Some(codex) = real_codex() else {
        eprintln!("SKIPPED: no codex on PATH");
        return;
    };
    if !claude_alive(&claude) {
        eprintln!("SKIPPED: no authenticated claude on PATH");
        return;
    }
    if !codex_alive(&codex) {
        eprintln!("SKIPPED: no authenticated codex on PATH");
        return;
    }

    let tag = format!("{}-{}", std::process::id(), now_ms());
    let nonce = format!("HANGAR-PIPELINE-NONCE-{tag}");
    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");

    // A scratch git repo so EVERY stage provisions its OWN worktree. Distinct
    // work_dirs are what make the per-stage artifact assertion meaningful: a
    // single shared cwd would let stage 1's file satisfy stage 4's check.
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    init_scratch_repo(&repo);

    // 1. Three agents on TWO providers, created through the real CLI.
    let a_tri = format!("pipe-triage-{tag}");
    let a_rev = format!("pipe-review-{tag}");
    let a_qa = format!("pipe-qa-{tag}");
    for (name, provider) in [(&a_tri, "claude"), (&a_rev, "codex"), (&a_qa, "claude")] {
        run_ainb(
            &ainb,
            home.path(),
            &[
                "hangar",
                "agent",
                "create",
                "--name",
                name,
                "--provider",
                provider,
            ],
        );
    }

    let pool = open_pool(&db_path).await;
    let (id_tri, rt_claude) = agent_ids_by_name(&pool, &a_tri).await;
    let (id_rev, rt_codex) = agent_ids_by_name(&pool, &a_rev).await;
    let (id_qa, _) = agent_ids_by_name(&pool, &a_qa).await;
    // A runtime is a DAEMON, not a provider binding: every agent binds to the one
    // `default` runtime whatever `--provider` said, and the per-agent provider
    // lives on `agent.provider`. So ONE daemon serves all three agents, and it is
    // handed BOTH real provider binaries.
    assert_eq!(
        rt_claude, rt_codex,
        "all agents share the single default runtime"
    );
    let providers: Vec<String> =
        sqlx::query_scalar("SELECT provider FROM agent WHERE id IN (?1, ?2, ?3) ORDER BY id")
            .bind(&id_tri)
            .bind(&id_rev)
            .bind(&id_qa)
            .fetch_all(&pool)
            .await
            .expect("read agent providers");
    assert!(
        providers.iter().any(|p| p == "codex") && providers.iter().any(|p| p == "claude"),
        "the roster must mix real providers, saw {providers:?}"
    );

    // Cheapest live claude model on both claude agents.
    for id in [&id_tri, &id_qa] {
        run_ainb(
            &ainb,
            home.path(),
            &["hangar", "agent", "edit", id, "--model", "haiku"],
        );
    }

    // 2. The pipeline, through the real CLI verb.
    run_ainb(&ainb, home.path(), &["hangar", "pipeline", "init"]);

    // 3. A squad whose members carry the ROLES that gate each stage. This is the
    //    only thing that decides who may take which stage.
    let squad = format!("pipe-squad-{tag}");
    let squad_id = parse_parenthesised_id(&run_ainb_capture(
        &ainb,
        home.path(),
        &[
            "hangar",
            "squad",
            "create",
            &squad,
            "--leader",
            &format!("agent:{id_tri}"),
        ],
    ));
    for (agent_id, roles) in [
        (&id_tri, "triager,implementer"),
        (&id_rev, "reviewer"),
        (&id_qa, "tester"),
    ] {
        run_ainb(
            &ainb,
            home.path(),
            &[
                "hangar",
                "squad",
                "add-member",
                &squad_id,
                "--member",
                &format!("agent:{agent_id}"),
                "--role",
                roles,
            ],
        );
    }

    // 4. Two daemons, one per runtime, each pointed at its REAL provider binary.
    let daemon = LiveDaemon::spawn_with(
        &home.path().join("daemon.log"),
        home.path(),
        &rt_claude,
        &[
            ("HANGAR_CLAUDE_PATH", claude.as_path()),
            ("HANGAR_CODEX_PATH", codex.as_path()),
        ],
    );

    // 5. The issue. Its title BECOMES the provider prompt at every stage, so the
    //    brief asks for the one observable side effect the model cannot fake by
    //    printing text.
    let brief = format!(
        "Use the Bash tool to run exactly this one shell command and nothing else: \
         printf '%s' '{nonce}' > {ARTIFACT_NAME}  \
         Do not use the Write tool. Once the command has run, stop."
    );
    let issue_out = run_ainb_capture(
        &ainb,
        home.path(),
        &[
            "hangar",
            "issue",
            "create",
            "--title",
            &brief,
            "--repo",
            repo.to_str().expect("repo path"),
        ],
    );
    let issue_id = parse_created_id(&issue_out);

    // No task exists yet: the card has not been dispatched.
    assert_eq!(
        count_tasks(&pool, &issue_id).await,
        0,
        "issue create without --assign must not enqueue a run"
    );

    // 6. Dispatch to the squad. Under pull this enqueues the card into Triage and
    //    ONE eligible agent takes it, rather than starting one run per member.
    run_ainb(
        &ainb,
        home.path(),
        &[
            "hangar", "squad", "assign", &squad_id, "--issue", &issue_id, "--fanout",
        ],
    );

    // 7. WALK THE PIPELINE, asserting continuously.
    let observed = walk_pipeline(&pool, &issue_id, home.path()).await;
    drop(daemon);

    // ---- Assertions -----------------------------------------------------
    println!("\n=== LIVE PIPELINE PROOF: {issue_id} ===");
    for s in &observed.stages {
        println!(
            "stage {:<10} agent={:<28} kind={:<7} task={} parent={}",
            s.column,
            s.agent_id,
            s.agent_kind,
            s.task_id,
            s.parent_task_id.as_deref().unwrap_or("(none)")
        );
    }

    assert!(
        observed.max_concurrent_running <= 1,
        "MORE THAN ONE RUNNING TASK on the card at once (saw {}), which is the \
         broadcast defect this pipeline removes",
        observed.max_concurrent_running
    );

    assert!(
        observed.stages.len() >= 4,
        "expected the card to traverse 4 role-gated stages, saw {}: {:?}",
        observed.stages.len(),
        observed.stages
    );

    let by_role = |role: &str| {
        observed
            .stages
            .iter()
            .find(|s| s.services_role == role)
            .unwrap_or_else(|| panic!("no stage serviced role `{role}`: {:?}", observed.stages))
            .clone()
    };
    let implement = by_role("implementer");
    let review = by_role("reviewer");
    let qa = by_role("tester");

    assert_ne!(
        review.agent_id, implement.agent_id,
        "THE REVIEWER IS THE IMPLEMENTER. The prior-agent exclusion did not bite."
    );
    assert_ne!(
        qa.agent_id, implement.agent_id,
        "QA must not be the implementer"
    );
    assert_ne!(qa.agent_id, review.agent_id, "QA must not be the reviewer");

    let distinct: std::collections::HashSet<_> =
        observed.stages.iter().map(|s| s.agent_id.clone()).collect();
    assert!(
        distinct.len() >= 3,
        "expected at least THREE different agents across the pipeline, saw {}: {distinct:?}",
        distinct.len()
    );

    // Two REAL provider CLIs actually drove the work.
    let kinds: std::collections::HashSet<_> =
        observed.stages.iter().map(|s| s.agent_kind.clone()).collect();
    assert!(
        kinds.contains("claude") && kinds.contains("codex"),
        "expected both real provider CLIs to have run, saw {kinds:?}"
    );

    // The parent_task_id chain is unbroken: stage N+1 points at stage N.
    for pair in observed.stages.windows(2) {
        assert_eq!(
            pair[1].parent_task_id.as_deref(),
            Some(pair[0].task_id.as_str()),
            "BROKEN HANDOFF CHAIN: stage `{}` must chain to stage `{}`",
            pair[1].column,
            pair[0].column
        );
    }
    assert_eq!(
        observed.stages[0].parent_task_id, None,
        "the first stage has no predecessor to chain to"
    );

    // The card advanced ONE column at a time, never skipping a stage.
    assert_eq!(
        observed.column_ords,
        (1..=observed.column_ords.len() as i64).collect::<Vec<_>>(),
        "the card must step one column at a time, saw ords {:?}",
        observed.column_ords
    );

    // Every stage did REAL work: its own worktree holds the exact nonce, and the
    // run recorded real token usage.
    for s in &observed.stages {
        let work_dir = s
            .work_dir
            .as_deref()
            .unwrap_or_else(|| panic!("stage `{}` recorded no work_dir", s.column));
        let artifact = Path::new(work_dir).join(ARTIFACT_NAME);
        let got = std::fs::read_to_string(&artifact).unwrap_or_else(|e| {
            panic!(
                "NONCE ARTIFACT MISSING for stage `{}` at {artifact:?} ({e}). \
                 The task reached a terminal state without doing real work. \
                 daemon log:\n{}",
                s.column,
                std::fs::read_to_string(home.path().join("daemon.log")).unwrap_or_default(),
            )
        });
        assert_eq!(
            got.trim(),
            nonce,
            "stage `{}` wrote the wrong nonce",
            s.column
        );

        let usage = fetch_usage(&pool, &s.task_id).await;
        assert!(
            usage.is_some_and(|u| u.input_tokens > 0 || u.output_tokens > 0),
            "stage `{}` recorded no token usage, so no real inference happened",
            s.column
        );
    }

    dump_sqlite_evidence(&pool, &issue_id, &observed).await;
    println!("=== ALL PIPELINE ASSERTIONS PASSED ===\n");
}

/// Print the RAW sqlite evidence behind each success criterion, so the proof can
/// be read directly off the database rather than taken on the assertions' word.
async fn dump_sqlite_evidence(pool: &SqlitePool, issue_id: &str, walk: &PipelineWalk) {
    // The PEAK, tracked live. A post-hoc count would read 0 (everything is
    // terminal by now) and would prove nothing about what happened mid-flight,
    // which is exactly where a double-dispatch would appear.
    println!("\n--- [1] PEAK simultaneous runs on the card (must be <= 1) ---");
    println!(
        "MAX over `SELECT COUNT(*) FROM agent_task_queue WHERE issue_id=? AND status='running'`,\n\
         sampled every 250ms across the whole walk => {}",
        walk.max_concurrent_running
    );
    println!(
        "columns the card occupied, in order (ords) => {:?}",
        walk.column_ords
    );

    println!("\n--- [2] the stage chain: agent, provider, parent_task_id ---");
    let rows = sqlx::query(
        "SELECT t.id, t.agent_id, a.name AS agent_name, t.agent_kind, t.status, \
                t.generation, COALESCE(t.parent_task_id,'(none)') AS parent \
           FROM agent_task_queue t JOIN agent a ON a.id = t.agent_id \
          WHERE t.issue_id = ?1 ORDER BY t.created_at, t.id",
    )
    .bind(issue_id)
    .fetch_all(pool)
    .await
    .expect("read chain");
    println!(
        "{:<28} {:<22} {:<7} {:<6} {:>3}  {}",
        "task_id", "agent_name", "kind", "status", "gen", "parent_task_id"
    );
    for r in &rows {
        println!(
            "{:<28} {:<22} {:<7} {:<6} {:>3}  {}",
            r.get::<String, _>("id"),
            r.get::<String, _>("agent_name"),
            r.get::<String, _>("agent_kind"),
            r.get::<String, _>("status"),
            r.get::<i64, _>("generation"),
            r.get::<String, _>("parent"),
        );
    }

    println!("\n--- [3] distinct agents that ran the card ---");
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT agent_id) FROM agent_task_queue WHERE issue_id = ?1",
    )
    .bind(issue_id)
    .fetch_one(pool)
    .await
    .expect("count distinct");
    println!("SELECT COUNT(DISTINCT agent_id) => {n}");

    println!("\n--- [4] where the card finished ---");
    let fin = sqlx::query(
        "SELECT col.name AS name, col.ord AS ord, \
                COALESCE(col.services_role,'(none)') AS role \
           FROM board_card bc JOIN board_column col ON col.id = bc.column_id \
          WHERE bc.issue_id = ?1",
    )
    .bind(issue_id)
    .fetch_one(pool)
    .await
    .expect("read final column");
    println!(
        "board_card.column => {} (ord {}, services_role {})",
        fin.get::<String, _>("name"),
        fin.get::<i64, _>("ord"),
        fin.get::<String, _>("role"),
    );

    println!("\n--- [5] real token spend per stage (a stub cannot fake this) ---");
    for r in &rows {
        let id: String = r.get("id");
        let u = fetch_usage(pool, &id).await;
        println!(
            "{id}  in={:<7} out={:<7}",
            u.as_ref().map_or(-1, |u| u.input_tokens),
            u.as_ref().map_or(-1, |u| u.output_tokens),
        );
    }
}

/// One observed pipeline stage: the task that owned it plus where it sat.
#[derive(Debug, Clone)]
struct StageObservation {
    column: String,
    services_role: String,
    task_id: String,
    agent_id: String,
    agent_kind: String,
    parent_task_id: Option<String>,
    work_dir: Option<String>,
}

/// What a full pipeline walk observed.
#[derive(Debug, Default)]
struct PipelineWalk {
    stages: Vec<StageObservation>,
    /// The HIGHEST number of simultaneously `running` tasks ever seen on the
    /// card. Sampled every poll, so a transient double-dispatch cannot hide
    /// between two end-state reads.
    max_concurrent_running: i64,
    /// The `ord` of every column the card was observed in, in order.
    column_ords: Vec<i64>,
}

/// Total wall-clock budget for the whole four-stage walk (four real provider
/// runs across two CLIs).
const PIPELINE_BUDGET: Duration = Duration::from_secs(900);

/// Drive and observe the card until it reaches a terminal (non-role-gated)
/// column or the budget expires.
///
/// Polls fast (250ms) so the "never two running at once" sample is dense enough
/// to catch a transient double-dispatch, which a start/end comparison would miss
/// entirely.
async fn walk_pipeline(pool: &SqlitePool, issue_id: &str, home: &Path) -> PipelineWalk {
    let deadline = std::time::Instant::now() + PIPELINE_BUDGET;
    let mut walk = PipelineWalk::default();
    let mut seen_tasks: Vec<String> = Vec::new();
    let mut last_ord: Option<i64> = None;

    while std::time::Instant::now() < deadline {
        // (a) One-owner sample, every poll.
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_queue \
              WHERE issue_id = ?1 AND status = 'running'",
        )
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("count running");
        walk.max_concurrent_running = walk.max_concurrent_running.max(running);

        // (b) Where the card sits now.
        let pos = sqlx::query(
            "SELECT col.name AS name, col.ord AS ord, col.services_role AS role \
               FROM board_card bc JOIN board_column col ON col.id = bc.column_id \
              WHERE bc.issue_id = ?1",
        )
        .bind(issue_id)
        .fetch_optional(pool)
        .await
        .expect("read card position");

        if let Some(p) = &pos {
            let ord: i64 = p.get("ord");
            if last_ord != Some(ord) {
                walk.column_ords.push(ord);
                last_ord = Some(ord);
            }
        }

        // (c) Record any newly-created task, with the stage it serves.
        let rows = sqlx::query(
            "SELECT t.id AS id, t.agent_id AS agent_id, t.agent_kind AS agent_kind, \
                    t.parent_task_id AS parent_task_id, t.work_dir AS work_dir, \
                    t.status AS status, t.created_at AS created_at \
               FROM agent_task_queue t \
              WHERE t.issue_id = ?1 ORDER BY t.created_at, t.id",
        )
        .bind(issue_id)
        .fetch_all(pool)
        .await
        .expect("read tasks");

        for r in &rows {
            let id: String = r.get("id");
            if seen_tasks.contains(&id) {
                // Refresh work_dir, which is only stamped once the run starts.
                if let Some(s) = walk.stages.iter_mut().find(|s| s.task_id == id) {
                    if s.work_dir.is_none() {
                        s.work_dir = r.get("work_dir");
                    }
                }
                continue;
            }
            seen_tasks.push(id.clone());
            // The stage a task serves is the column the card was in when it was
            // pulled, which is the card's position right now for the newest task.
            let (column, role) = pos.as_ref().map_or_else(
                || ("(unknown)".to_string(), String::new()),
                |p| {
                    (
                        p.get::<String, _>("name"),
                        p.get::<Option<String>, _>("role").unwrap_or_default(),
                    )
                },
            );
            walk.stages.push(StageObservation {
                column,
                services_role: role,
                task_id: id,
                agent_id: r.get("agent_id"),
                agent_kind: r.get("agent_kind"),
                parent_task_id: r.get("parent_task_id"),
                work_dir: r.get("work_dir"),
            });
        }

        // (d) Done when the card reaches a column with no role gate AND nothing
        //     is active: the pipeline has run to its terminal stage.
        let gated = pos.as_ref().and_then(|p| p.get::<Option<String>, _>("role")).is_some();
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_queue \
              WHERE issue_id = ?1 AND status IN ('queued','dispatched','running')",
        )
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("count active");
        if !gated && active == 0 && !walk.stages.is_empty() {
            return walk;
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    panic!(
        "pipeline did not reach a terminal column within {PIPELINE_BUDGET:?}. \
         observed stages: {:?}\ndaemon log:\n{}",
        walk.stages,
        std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default(),
    );
}

/// Count every task row on an issue.
async fn count_tasks(pool: &SqlitePool, issue_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?1")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("count tasks")
}

/// Pull the created entity id out of a `... <id>` CLI creation line. The hangar
/// creation verbs all echo the new id as the last whitespace-separated token of
/// their first output line.
fn parse_created_id(stdout: &str) -> String {
    stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| l.split_whitespace().last())
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("no created id in output:\n{stdout}"))
}

/// Pull an id out of a `created squad <name> (<id>) led by ...` line, whose id
/// is parenthesised rather than trailing.
fn parse_parenthesised_id(stdout: &str) -> String {
    stdout
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(id, _)| id.trim().to_string())
        .unwrap_or_else(|| panic!("no parenthesised id in output:\n{stdout}"))
}

/// A scratch git repo with one commit, so each stage can provision its own
/// worktree from it.
fn init_scratch_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "--initial-branch=main"]);
    git(&["config", "user.email", "pipeline@example.test"]);
    git(&["config", "user.name", "Pipeline Proof"]);
    // Signing is disabled for this throwaway repo: a headless commit against a
    // locked keychain would hang on pinentry rather than fail.
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "pipeline proof scratch repo\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "seed"]);
}
