//! Shared helpers for the P1.7 e2e tripwires.
//!
//! Lives under `tests/tripwire_support/mod.rs` (not
//! `tests/tripwire_support.rs`) so Cargo treats it as a shared module rather
//! than its own test binary. Each tripwire `mod tripwire_support;`s this in.
//! Helpers cover: seeding the minimal world, writing a fake-`claude` script, an
//! RAII tmux session that kills itself by exact name on drop, and a DB poll
//! helper with a deadline.

#![allow(dead_code)]
// not every tripwire uses every helper
// Helper docs mention capitalised domain nouns (AskUserQuestion); backticking
// each hurts readability. Mirrors `tripwire_p4_common.rs`.
#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};

/// The seeded ids a tripwire needs to enqueue and assert against.
pub struct SeededIds {
    /// `workspace.id`.
    pub workspace_id: String,
    /// `workspace.slug` (used to build the expected env-dir path).
    pub workspace_slug: String,
    /// `agent_runtime.id` (the daemon's `HANGAR_DAEMON_RUNTIME_ID`).
    pub runtime_id: String,
    /// `agent.id`.
    pub agent_id: String,
}

/// Seed the minimal workspace + user + runtime + agent graph a task FKs
/// require.
pub async fn seed_world(pool: &SqlitePool) -> SeededIds {
    let ids = SeededIds {
        workspace_id: "ws-trip".to_string(),
        workspace_slug: "trip".to_string(),
        runtime_id: "rt-trip".to_string(),
        agent_id: "agent-trip".to_string(),
    };
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(&ids.workspace_id)
        .bind(&ids.workspace_slug)
        .bind("Trip")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-trip")
        .bind("trip@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&ids.runtime_id)
    .bind(&ids.workspace_id)
    .bind("daemon-trip")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&ids.agent_id)
    .bind(&ids.workspace_id)
    .bind("Agent")
    .bind(&ids.runtime_id)
    .bind("workspace")
    .bind("user-trip")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent");
    ids
}

/// Seed a `codex` runtime + agent into an already-seeded world (e38.16 routing
/// tripwire). Returns the new `(runtime_id, agent_id)`.
///
/// The agent carries a `model` override (`gpt-5-codex`) and a `cli_args`
/// (`["--full-auto"]`) so the routing test can also assert the daemon threads
/// the migration-0015 config onto the codex argv.
pub async fn seed_codex_agent(pool: &SqlitePool, ids: &SeededIds) -> (String, String) {
    let runtime_id = "rt-codex".to_string();
    let agent_id = "agent-codex".to_string();
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&runtime_id)
    .bind(&ids.workspace_id)
    .bind("daemon-codex")
    .bind("codex")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert codex runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks, \
          model, cli_args) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&agent_id)
    .bind(&ids.workspace_id)
    .bind("CodexAgent")
    .bind(&runtime_id)
    .bind("workspace")
    .bind("user-trip")
    .bind(5_i64)
    .bind("gpt-5-codex")
    .bind(r#"["--full-auto"]"#)
    .execute(pool)
    .await
    .expect("insert codex agent");
    (runtime_id, agent_id)
}

/// Write an executable `fake-codex.sh` that mimics `codex exec`: it emits a
/// `system` line carrying `session_id`, echoes its own argv as a plain line (so
/// the routing test can assert the `exec` subcommand + `-m <model>` landed),
/// then a `result` line, and exits 0.
pub fn fake_codex_happy(dir: &Path, session_id: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\necho '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         echo \"ARGV=$*\"\n\
         echo '{{\"type\":\"result\",\"content\":\"codex-ok\"}}'\nexit 0\n"
    );
    write_executable(dir, "fake-codex.sh", &body)
}

/// Write a fake `claude` that BRANCHES on whether stdout is a TTY, so the SAME
/// binary can back BOTH a headless and an interactive dispatch inside one
/// daemon (the a54 concurrency tripwire):
///
///   * headless (stdout is a pipe — the captured runner path): emit the
///     `system`
///     + `result` JSONL the runner pins and exit 0 (a fast, clean completion).
///   * interactive (stdout is a tmux pty): BLOCK until `$HOME/<sentinel>`
///     appears (self-exits after ~60s so a wiring bug can never wedge the
///     session), then exit 0.
///
/// `[ -t 1 ]` is the reliable discriminator: the headless runner pipes the
/// provider's stdout, while the interactive tmux pane hands it a pty. Both
/// paths resolve the same `HANGAR_CLAUDE_PATH`, so one binary serves both
/// tasks.
pub fn fake_claude_tty_branch(dir: &Path, session_id: &str, sentinel: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\n\
         if [ -t 1 ]; then\n\
         \ti=0\n\
         \twhile [ ! -f \"$HOME/{sentinel}\" ] && [ \"$i\" -lt 300 ]; do sleep 0.2; i=$((i+1)); done\n\
         \texit 0\n\
         fi\n\
         echo '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\n\
         exit 0\n"
    );
    write_executable(dir, "fake-claude-tty-branch.sh", &body)
}

/// Write an executable `fake-claude.sh` that emits a `system` line carrying
/// `session_id`, then a `result` line, then exits 0.
pub fn fake_claude_happy(dir: &Path, session_id: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\necho '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\nexit 0\n"
    );
    write_executable(dir, "fake-claude.sh", &body)
}

/// Write an executable `fake-claude.sh` that emits a `system` line carrying
/// `session_id`, then a `result` line whose `usage` block reports
/// `input_tokens`/`output_tokens` + `total_cost_usd` (e38.35), then exits 0.
///
/// The script name differs from [`fake_claude_happy`] so a test can stage both
/// in the same home without one clobbering the other.
pub fn fake_claude_with_usage(
    dir: &Path,
    session_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
) -> PathBuf {
    let body = format!(
        "#!/bin/sh\necho '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         echo '{{\"type\":\"result\",\"subtype\":\"success\",\"total_cost_usd\":{cost_usd},\
         \"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":{output_tokens}}}}}'\n\
         exit 0\n"
    );
    write_executable(dir, "fake-claude-usage.sh", &body)
}

/// Write an executable `gh` stand-in into a fresh `bin/` subdir of `dir` that,
/// on `gh pr create …`, prints `pr_url` on its own line then exits 0 (mimicking
/// the real `gh pr create` success output). Returns the **bin directory** to
/// prepend to `PATH` so the agent subprocess resolves this `gh`.
///
/// `gh` invoked with any other first arg (e.g. `pr list`) prints a table-ish
/// blob and exits 0 — never a bare PR URL — so the parser must not match it.
pub fn fake_gh_pr_create(dir: &Path, pr_url: &str) -> PathBuf {
    let bin = dir.join("fakebin");
    std::fs::create_dir_all(&bin).expect("mk fakebin dir");
    // `case` on the second positional ($2) — `gh pr create` vs `gh pr list`.
    let body = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"create\" ]; then\n\
         \techo '{pr_url}'\n\
         \texit 0\n\
         fi\n\
         echo 'gh: other verb'\n\
         exit 0\n"
    );
    write_executable(&bin, "gh", &body);
    bin
}

/// Write an executable `gh` stand-in that **fails** (exit 3) on `pr create`,
/// printing an error to stderr and NO PR URL — the failure-mode fixture.
/// Returns the bin directory to prepend to `PATH`.
pub fn fake_gh_pr_create_fails(dir: &Path) -> PathBuf {
    let bin = dir.join("fakebin");
    std::fs::create_dir_all(&bin).expect("mk fakebin dir");
    let body = "#!/bin/sh\n\
         echo 'gh: a pull request for branch already exists' 1>&2\n\
         exit 3\n";
    write_executable(&bin, "gh", body);
    bin
}

/// Write an executable `fake-claude.sh` that pins `session_id`, runs
/// `gh pr create …` (ignoring its exit so the *agent* completes regardless of
/// what `gh` did — the v1 "agent shells out to whatever it wants" contract),
/// echoes a result line, then exits 0. The captured `gh` stdout (a PR URL on
/// success) flows through the provider's stdout tail, which is where the daemon
/// scans for the PR URL.
pub fn fake_claude_runs_gh(dir: &Path, session_id: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\n\
         echo '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         gh pr create --title X --body Y || true\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\n\
         exit 0\n"
    );
    write_executable(dir, "fake-claude.sh", &body)
}

/// The AskUserQuestion transcript line the hook-firing stubs point the
/// attention ingest at — a single `tool_use` block, the strongest ASK
/// classifier signal (mirrors the `attention_ingest` unit fixture). Written to
/// `$HOME/ask.jsonl` by [`plant_ask_transcript`]; the stub passes that path as
/// the hook's `transcript_path`, so the daemon's ingest classifies the raised
/// event as ASK.
const ASK_TRANSCRIPT_LINE: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Ship it?","options":[{"label":"yes"},{"label":"no"}]}]}}]},"timestamp":"2026-01-01T00:00:00Z"}"#;

/// Plant the AskUserQuestion transcript at `<home>/ask.jsonl` — the exact path
/// the hook-firing stubs reference as `"$HOME/ask.jsonl"` (the stub's `$HOME`
/// is this isolated tempdir). The daemon's attention ingest reads it to
/// classify the ASK.
pub fn plant_ask_transcript(home: &Path) {
    std::fs::write(home.join("ask.jsonl"), format!("{ASK_TRANSCRIPT_LINE}\n"))
        .expect("plant ASK transcript");
}

/// Locate the built `ainb` binary the hook-firing stubs shell out to for
/// `ainb fleet atc hook`. `CARGO_BIN_EXE_ainb` is only defined for the `ainb`
/// crate's own tests; from the daemon crate we walk up to
/// `target/<profile>/ainb`. `None` when it can't be found (→ the tripwire SKIPs
/// — the hook cannot be fired without it).
pub fn ainb_bin() -> Option<PathBuf> {
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

/// Write a fake-`claude` (HEADLESS) that fires the lifecycle hook the way
/// `notify.sh` does before finishing: it pipes a `{"transcript_path": …}`
/// payload into `ainb fleet atc hook --event Notification`, reading
/// `AINB_PARENT_SESSION` from its INHERITED env so the hook's fleet-membership
/// gate resolves (the fix under test). The hook then appends to `events.jsonl`,
/// the daemon's attention ingest classifies it (ASK, via the planted
/// transcript), and raises a row for `hook_session`. The stub also emits the
/// `system` + `result` JSONL lines the headless runner pins, then exits 0.
/// `ainb` is the absolute binary path.
pub fn fake_claude_fires_hook(dir: &Path, ainb: &Path, hook_session: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\n\
         echo '{{\"type\":\"system\",\"session_id\":\"{hook_session}\"}}'\n\
         printf '%s' '{{\"transcript_path\":\"'\"$HOME\"'/ask.jsonl\"}}' \
         | '{ainb}' fleet atc hook --event Notification --session-id '{hook_session}' --cwd \"$PWD\" >/dev/null 2>&1\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\n\
         exit 0\n",
        ainb = ainb.display(),
    );
    write_executable(dir, "fake-claude-hook.sh", &body)
}

/// Write a fake-`claude` (INTERACTIVE) that fires the same lifecycle hook, then
/// BLOCKS until the tripwire touches `$HOME/interactive-go` (self-exits after
/// ~10s so a wiring bug can never wedge a session), then exits 0. Blocking lets
/// the tripwire observe the live `tmux_hangar-<task_id>` session + its registry
/// entry before releasing the agent to finish (mirrors the jgb interactive
/// sibling's block-observe-release shape). `ainb` is the absolute binary path.
pub fn fake_claude_interactive_fires_hook(dir: &Path, ainb: &Path, hook_session: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\n\
         printf '%s' '{{\"transcript_path\":\"'\"$HOME\"'/ask.jsonl\"}}' \
         | '{ainb}' fleet atc hook --event Notification --session-id '{hook_session}' --cwd \"$PWD\" >/dev/null 2>&1\n\
         i=0\n\
         while [ ! -f \"$HOME/interactive-go\" ] && [ \"$i\" -lt 100 ]; do sleep 0.1; i=$((i+1)); done\n\
         exit 0\n",
        ainb = ainb.display(),
    );
    write_executable(dir, "fake-claude-interactive-hook.sh", &body)
}

/// Poll the `attention` table until a row for `session_id` appears (returning
/// its `kind`) or `budget` elapses (returning `None`). The daemon's ingest
/// ticks every few seconds, so a bounded poll absorbs the append→ingest lag.
pub async fn wait_for_attention_kind(
    pool: &SqlitePool,
    session_id: &str,
    budget: Duration,
) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        let kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM attention WHERE session_id = ? LIMIT 1")
                .bind(session_id)
                .fetch_optional(pool)
                .await
                .expect("query attention row");
        if kind.is_some() {
            return kind;
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Read the raw `sessions.json` the daemon's session registry writes (and
/// `ainb list` reads) under `<home>/.agents-in-a-box/sessions.json`. `None`
/// when absent. The tripwire asserts a daemon-spawned interactive session is
/// keyed here so `ainb list` / fleet discover can see it.
pub fn read_sessions_json(home: &Path) -> Option<serde_json::Value> {
    let path = home.join(".agents-in-a-box").join("sessions.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Multiplier applied to poll budgets, read from `HANGAR_TRIPWIRE_BUDGET_SCALE`
/// (default `1`, floored at `1`).
///
/// The same knob the P4 tmux harness uses. A hosted runner running the full
/// serial suite is far slower than a dev box, and a tripwire that hardcodes a
/// dev-tuned deadline goes red on runner speed rather than on a regression —
/// which is a worse failure than no test, because it trains everyone to ignore
/// the gate. Scaling keeps single-run rigor: a real regression still fails at
/// the scaled deadline.
#[must_use]
pub fn budget_scale() -> u32 {
    std::env::var("HANGAR_TRIPWIRE_BUDGET_SCALE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1)
}

/// Read the daemon's structured JSONL logs under
/// `<home>/.agents-in-a-box/hangar/logs/` and return the tail of every `daemon.*`
/// file (newest file last, last 200 lines each). The daemon logs at `info` by
/// default (finalize outcomes, attention-ingest classify/insert), so folding this
/// into a FAILING tripwire's panic makes a CI-only failure diagnosable without a
/// local Linux repro. Empty-safe when the dir/files are absent.
#[must_use]
pub fn dump_daemon_logs(home: &Path) -> String {
    let logs = home.join(".agents-in-a-box").join("hangar").join("logs");
    let Ok(entries) = std::fs::read_dir(&logs) else {
        return format!("(no daemon logs at {})", logs.display());
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("daemon."))
        })
        .collect();
    files.sort();
    let mut out = String::new();
    for f in &files {
        let Ok(s) = std::fs::read_to_string(f) else {
            continue;
        };
        let lines: Vec<&str> = s.lines().collect();
        let start = lines.len().saturating_sub(200);
        let _ = writeln!(
            out,
            "--- {} (last {} lines) ---",
            f.display(),
            lines.len() - start
        );
        for line in &lines[start..] {
            let _ = writeln!(out, "{line}");
        }
    }
    if out.is_empty() {
        out = format!(
            "(daemon log dir {} present but no daemon.* content)",
            logs.display()
        );
    }
    // The hook's durable event log — decisive for the attention tripwire: it
    // distinguishes "the hook never appended" (file absent/empty → membership gate
    // or env fault) from "appended but the ingest never raised a row" (classify /
    // insert fault). The ingest cursor shows whether the tail advanced past it.
    let hangar = home.join(".agents-in-a-box");
    match std::fs::read_to_string(hangar.join("events.jsonl")) {
        Ok(s) => {
            let _ = writeln!(out, "--- events.jsonl ({} bytes) ---\n{s}", s.len());
        }
        Err(e) => {
            let _ = writeln!(out, "--- events.jsonl: unreadable ({e}) ---");
        }
    }
    out
}

/// Write `body` to `dir/name` and mark it executable (0755).
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// Current wall-clock time as epoch milliseconds (matches the daemon's
/// `SystemClock`), so seeded `created_at` / `dispatched_at` are "now" relative
/// to the live daemon and the sweepers behave as in production.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Path to the built `ainb-hangar-daemon` binary under test.
pub fn daemon_bin() -> PathBuf {
    // assert_cmd resolves the freshly-built binary for the crate under test.
    assert_cmd::cargo::cargo_bin("ainb-hangar-daemon")
}

/// Whether the `tmux` binary is on `PATH`.
pub fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// An RAII tmux session running the daemon; killed by **exact name** on drop.
///
/// Per the `tmux_protection` global rule this never uses a bulk/wildcard kill —
/// only `tmux kill-session -t <exact-name>` for the session it created.
pub struct DaemonSession {
    name: String,
}

impl DaemonSession {
    /// Spawn `bin` (with `env` overrides) inside a fresh, uniquely-named tmux
    /// session. The session name embeds the pid + a nanosecond timestamp so
    /// parallel test binaries never collide.
    pub fn spawn(bin: &Path, _home: &Path, env: &[(&str, &str)]) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("hangar-trip-{}-{nanos}", std::process::id());

        // Build the shell command: export each env var, then exec the daemon.
        // The daemon is the genuine binary; tmux keeps it alive across the poll.
        let mut cmd = String::new();
        for (k, v) in env {
            let _ = write!(cmd, "export {k}='{v}'; ");
        }
        let _ = write!(cmd, "exec '{}'", bin.display());

        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "sh", "-c", &cmd])
            .status()
            .expect("spawn tmux session");
        assert!(status.success(), "tmux new-session failed for {name}");

        Self { name }
    }

    /// The exact tmux session name (for `capture-pane` assertions).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Capture the session's visible pane text (best-effort; empty on error).
    pub fn capture_pane(&self) -> String {
        Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &self.name])
            .output()
            .map_or_else(
                |_| String::new(),
                |o| String::from_utf8_lossy(&o.stdout).into_owned(),
            )
    }
}

impl Drop for DaemonSession {
    fn drop(&mut self) {
        // Exact-name kill only — never a wildcard or kill-server.
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

/// A daemon spawned as a plain child under a fully ISOLATED home, killed by its
/// own pid on drop (never a wildcard).
///
/// Unlike [`DaemonSession`] (tmux-hosted, so it inherits the operator's ambient
/// env), this pins `HOME` and REMOVES `AINB_HOME` / `AINB_HANGAR_HOME`, so
/// `events.jsonl`, `sessions.json`, and `hangar.db` all resolve under
/// `<home>/.agents-in-a-box` regardless of what the operator's shell has set —
/// the isolation the ccc attention + registry tripwires require to avoid
/// touching the real fleet home.
pub struct DaemonProcess {
    child: std::process::Child,
}

impl DaemonProcess {
    /// Spawn the daemon binary under `home` with `extra_env` layered on, stdio
    /// nulled. `HOME` is pinned and the two home-override vars are removed so
    /// the child cannot escape the isolated tree.
    pub fn spawn(home: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(daemon_bin());
        cmd.env("HOME", home)
            .env_remove("AINB_HOME")
            .env_remove("AINB_HANGAR_HOME")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn ainb-hangar-daemon");
        Self { child }
    }

    /// This daemon child's PID — for sending it a graceful `SIGINT` in a
    /// shutdown-reap test (so its `Ctrl-C` reap path runs, rather than the
    /// `SIGKILL` that `Drop` uses).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        // Kill only this exact daemon child — never a process-name or wildcard kill.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Whether a tmux session with the EXACT name `name` is live (`tmux
/// has-session`).
#[must_use]
pub fn tmux_session_live(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .is_ok_and(|s| s.success())
}

/// Kill a tmux session by its EXACT name (never a wildcard / kill-server) — the
/// interactive tripwire's defensive teardown of the `tmux_hangar-<task_id>`
/// session the daemon spawned.
pub fn tmux_kill_session(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).status();
}

/// Poll the task row until its `status` equals `want` or `budget` elapses,
/// returning the matching row. Panics with the last-seen row on timeout.
pub async fn wait_for_db(
    pool: &SqlitePool,
    task_id: &str,
    want: &str,
    budget: Duration,
) -> SqliteRow {
    let deadline = Instant::now() + budget;
    let mut last: Option<(String, Option<String>)> = None;
    loop {
        if let Some(row) = fetch_row(pool, task_id).await {
            let status: String = row.get("status");
            if status == want {
                return row;
            }
            let reason: Option<String> = row.get("failure_reason");
            last = Some((status, reason));
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} did not reach status {want:?} within {budget:?}; last={last:?}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Poll the task row until any field matches the given predicate, or timeout.
pub async fn wait_until<F>(
    pool: &SqlitePool,
    task_id: &str,
    budget: Duration,
    mut pred: F,
) -> SqliteRow
where
    F: FnMut(&SqliteRow) -> bool,
{
    let deadline = Instant::now() + budget;
    loop {
        if let Some(row) = fetch_row(pool, task_id).await {
            if pred(&row) {
                return row;
            }
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} did not satisfy predicate within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
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
