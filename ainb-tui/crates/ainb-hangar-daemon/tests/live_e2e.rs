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

    eprintln!("LIVE E2E OK: task {task_id} done; nonce artifact verified at {artifact:?}");
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
    fn spawn(log_path: &Path, home: &Path, runtime_id: &str, claude_path: &Path) -> Self {
        let log = std::fs::File::create(log_path).expect("create daemon log");
        let err = log.try_clone().expect("clone daemon log handle");
        let child = Command::new(daemon_bin())
            .env("AINB_HANGAR_HOME", home)
            .env("HANGAR_DAEMON_RUNTIME_ID", runtime_id)
            .env("HANGAR_CLAUDE_PATH", claude_path)
            .env("HANGAR_DAEMON_DISABLE_SANDBOX", "1")
            .env("HANGAR_DAEMON_POLL_MS", "200")
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat noop-claude").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod noop-claude");
    }
    path
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
