//! Shared helpers for the P1.7 e2e tripwires.
//!
//! Lives under `tests/tripwire_support/mod.rs` (not `tests/tripwire_support.rs`)
//! so Cargo treats it as a shared module rather than its own test binary. Each
//! tripwire `mod tripwire_support;`s this in. Helpers cover: seeding the minimal
//! world, writing a fake-`claude` script, an RAII tmux session that kills itself
//! by exact name on drop, and a DB poll helper with a deadline.

#![allow(dead_code)] // not every tripwire uses every helper

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

/// Seed the minimal workspace + user + runtime + agent graph a task FKs require.
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

/// Write an executable `fake-claude.sh` that emits a `system` line carrying
/// `session_id`, then a `result` line, then exits 0.
pub fn fake_claude_happy(dir: &Path, session_id: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\necho '{{\"type\":\"system\",\"session_id\":\"{session_id}\"}}'\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\nexit 0\n"
    );
    write_executable(dir, "fake-claude.sh", &body)
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
