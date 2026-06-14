//! P4.9 — shared seed/spawn helpers for the per-screen TUI tripwires.
//!
//! Lives as `tests/tripwire_p4_common.rs` and is `#[path]`-included by each
//! `tripwire_p4_*` test (rather than a `mod` under `tests/`, which Cargo would
//! compile as its own test binary). It codifies the `tmux-ui-tripwire` skill's
//! HARD RULES:
//!
//! - exact-name `tmux kill-session` only (never `kill-server`/`pkill`/wildcard);
//! - `poll_capture` with a deadline + predicate (no bare `sleep` before capture);
//! - single-char nav keys sent WITHOUT `Enter`;
//! - POSITIVE marker paired with a NEGATIVE placeholder assertion (never a
//!   substring-OR on chrome strings);
//! - SKIP-not-fail when the environment can't support the test.
//!
//! ## What the pipeline looks like now (P4.10 closed the gaps)
//!
//! ```text
//! seed hangar.db ──▶ ainb-hangar-daemon (binds $HOME/.ainb/hangar.sock)
//!                            ▲ snapshot RPCs
//!  ainb tui (tmux) ──`g`──▶ HANGAR PluginScreen ──▶ hangar-tui plugin ──dial──┘
//! ```
//!
//! [`prepare_pipeline`] seeds an isolated `$HOME`'s `hangar.db` with the P4
//! fixture and spawns the daemon against it; [`TuiSession::spawn`] launches
//! `ainb tui` (plugins active) under the same `$HOME` and presses `g` to open the
//! Hangar screen. [`can_run_tripwire`] gates on tmux + both binaries + the staged
//! plugin, SKIPping (never failing) when any is missing.

#![allow(dead_code)]
// each tripwire uses a subset of these helpers.
// `Duration::from_secs(60)` reads fine as a poll budget; `from_mins` is unstable.
// Same rationale as `run_loop.rs`'s crate-level allow.
#![allow(clippy::duration_suboptimal_units)]
// Test-helper rustdoc: prose-heavy module/fn docs (multi-sentence first
// paragraphs, plain words that aren't code items) are intentional here.
#![allow(clippy::too_long_first_doc_paragraph, clippy::doc_markdown)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The expected on-screen marker proving the seeded hangar TUI actually rendered
/// (the issue-list landing screen shows the seeded `Refactor API` issue).
pub const READY_MARKER: &str = "Refactor API";

/// `true` when the `tmux` binary is usable.
pub fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Best-effort locate the built `ainb` binary the tripwire drives.
///
/// `CARGO_BIN_EXE_ainb` is only defined for tests of the `ainb` crate itself;
/// from the daemon crate we walk up to the workspace `target/<profile>/ainb`.
/// Returns `None` when it can't be found (→ the tripwire SKIPs).
pub fn ainb_bin() -> Option<PathBuf> {
    if let Some(p) = option_env!("CARGO_BIN_EXE_ainb") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    // .../target/<profile>/deps/<test-bin> → .../target/<profile>/ainb
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let candidate = profile_dir.join("ainb");
    candidate.exists().then_some(candidate)
}

/// The `ainb-hangar-daemon` binary (always defined for this crate's tests).
pub fn daemon_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_ainb-hangar-daemon"));
    p.exists().then_some(p)
}

/// The staged `hangar-tui` plugin binary the host discovers at
/// `<target>/dist/plugins/hangar-tui/hangar-tui`. `ainb tui` only renders the
/// Hangar screen when this is present + signed (`just stage-plugins`).
pub fn staged_plugin() -> Option<PathBuf> {
    plugin_root()
        .map(|r| r.join("hangar-tui").join("hangar-tui"))
        .filter(|p| p.exists())
}

/// The staged plugin root (`<workspace-root>/dist/plugins`), discovered from the
/// test binary location.
///
/// `build-plugins.sh` stages into `ainb-tui/dist/plugins/<id>/<id>` (the
/// workspace root, NOT under `target/`). From the test binary at
/// `<workspace-root>/target/<profile>/deps/<bin>` that is three levels up + `dist/plugins`.
pub fn plugin_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../target/<profile>/deps/<bin> → up to the dir holding `target/`.
    let target_dir = exe.parent()?.parent()?.parent()?; // deps → profile → target
    let workspace_root = target_dir.parent()?; // target → workspace root
    let p = workspace_root.join("dist").join("plugins");
    p.exists().then_some(p)
}

/// Whether the seeded TUI render pipeline is standable on this machine.
///
/// P4.10 closed every gap (plugin render dispatch + daemon snapshot RPCs + host
/// HANGAR screen + staged plugin), so the gate is now a **real probe**: tmux
/// present, both binaries built, and the plugin staged. There is no longer an
/// `AINB_HANGAR_TUI_E2E` opt-in env — when the pieces are present the tripwire
/// runs for real; when any is missing it SKIPs gracefully.
pub fn hangar_tui_ready() -> bool {
    ainb_bin().is_some() && daemon_bin().is_some() && staged_plugin().is_some()
}

/// The combined precondition: tmux present, binaries built, plugin staged.
/// Returns `false` (and the caller SKIPs) when any is missing.
pub fn can_run_tripwire() -> bool {
    tmux_available() && hangar_tui_ready()
}

/// A running daemon child + the isolated `$HOME` it serves. Kills the daemon on
/// drop (by its own pid only — never a wildcard).
pub struct Pipeline {
    home: tempfile::TempDir,
    daemon: Child,
}

impl Pipeline {
    /// The isolated `$HOME` the daemon + TUI share.
    pub fn home(&self) -> &Path {
        self.home.path()
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // Kill only this exact daemon child — never a process-name or wildcard kill.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Seed an isolated `$HOME`'s `hangar.db` with the P4 fixture and spawn the
/// daemon against it. The daemon resolves `~/.ainb` from `$HOME` (no
/// `AINB_HANGAR_HOME`), binding `$HOME/.ainb/hangar.sock` — the exact path the
/// plugin dials. Polls for the socket file before returning so the TUI's first
/// dial lands.
///
/// Panics only after [`can_run_tripwire`] has gated the caller.
pub fn prepare_pipeline() -> Pipeline {
    // RPC-only daemon (the default for the per-screen render tripwires): no claim
    // loop, so it never tries to spawn `claude`.
    prepare_pipeline_with(&[("HANGAR_DAEMON_DISABLE_CLAIM", "1")])
}

/// Seed an isolated `$HOME` with the P4 fixture and spawn the daemon against it
/// with the given `extra_env` overrides layered on top of the common ones.
///
/// Factors out the seed + spawn + socket-wait shared by [`prepare_pipeline`]
/// (RPC-only) and the claim-enabled health tripwire (which passes
/// `HANGAR_DAEMON_RUNTIME_ID` + `HANGAR_CLAUDE_PATH` so the daemon actually
/// claims + executes seeded tasks, populating the in-memory throughput ring the
/// daemon-health sparkline reads). `HOME` and `AINB_HANGAR_HOME` are always set
/// here; `extra_env` cannot override them.
///
/// Panics only after [`can_run_tripwire`] has gated the caller.
pub fn prepare_pipeline_with(extra_env: &[(&str, &str)]) -> Pipeline {
    prepare_pipeline_seeded(extra_env, |_| {})
}

/// Like [`prepare_pipeline`], plus one cron autopilot (`daily-triage`,
/// `0 9 * * *`) seeded into the fixture BEFORE the daemon spawns — for the
/// Autopilots-manager tripwire. The autopilot is written through the same
/// pre-daemon connection that seeds the issues (closed before the daemon opens
/// its own), NOT a second live connection racing the running daemon: that race
/// wedges the daemon's first issue snapshot on slow CI runners, leaving the
/// issue list empty until the whole tripwire times out.
pub fn prepare_pipeline_with_autopilot() -> Pipeline {
    prepare_pipeline_seeded(&[("HANGAR_DAEMON_DISABLE_CLAIM", "1")], seed_autopilot)
}

/// Shared body of [`prepare_pipeline_with`] and its variants: seed the isolated
/// `$HOME` fixture, run `pre_spawn_seed(home)` while no daemon is attached to the
/// database, then spawn the daemon. Splitting the pre-spawn seed out lets a
/// caller add fixture rows (e.g. an autopilot) through a connection that closes
/// before the daemon opens, instead of a second live connection.
fn prepare_pipeline_seeded(
    extra_env: &[(&str, &str)],
    pre_spawn_seed: impl FnOnce(&Path),
) -> Pipeline {
    let home = tempfile::tempdir().expect("isolated HOME tempdir");
    let hangar_dir = home.path().join(".ainb");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.ainb");

    // Onboarding skip: write a completed onboarding.toml so `ainb tui` lands on
    // the home screen rather than the wizard. Only the MAJOR version is gated, so
    // the daemon crate's CARGO_PKG_VERSION (same workspace major as `ainb`) is fine.
    seed_onboarding(home.path());

    // Seed the database (workspace + issues/agents/skills + running task) on the
    // `default` workspace the plugin subscribes to.
    seed_database(&hangar_dir);

    // P5.6: pre-ack the first-run danger-full-access warning so the per-screen
    // tripwires (issue list, settings, …) aren't blocked by the modal overlay.
    // The P5.6 tripwire that DOES want the modal calls `clear_first_run_ack`
    // first to undo this.
    seed_first_run_ack(home.path());

    // Pre-dismiss the notifyd first-run install prompt: a fresh $HOME has no
    // `~/.agents-in-a-box/install.json`, so `maybe_prompt_notify_install`
    // raises a ConfirmationDialog whose key handler swallows every key except
    // ←/→/Tab/Enter/Esc — including the `g` Hangar nav — deadlocking every TUI
    // tripwire at its full poll deadline (dialog ships since PR #194 /
    // 642dd6b4, which reached this branch via the main merge).
    seed_notify_prompt_dismissed(home.path());

    // Caller-provided fixture rows seeded while NO daemon is attached — their
    // connection closes before the daemon opens its own. Seeding here (not via a
    // second live connection after spawn) avoids a concurrency race that wedges
    // the daemon's first issue snapshot on slow CI runners.
    pre_spawn_seed(home.path());

    // Spawn the daemon under the same $HOME (binds $HOME/.ainb/hangar.sock).
    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let daemon = cmd.spawn().expect("spawn ainb-hangar-daemon");

    // Wait for the socket to appear (the daemon binds it during boot).
    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    Pipeline { home, daemon }
}

/// The plugin's `state.toml` path under an isolated `$HOME`:
/// `{home}/.ainb/hangar/state.toml` (the plugin resolves `$HOME/.ainb` when no
/// `$AINB_HANGAR_HOME` is set, as the TUI session is launched).
fn state_toml_path(home: &Path) -> PathBuf {
    home.join(".ainb").join("hangar").join("state.toml")
}

/// Pre-seed the `first_run` warning ack so the danger-full-access modal is
/// skipped (the per-screen tripwires don't want it). Preserves any foreign keys.
fn seed_first_run_ack(home: &Path) {
    let path = state_toml_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, "warnings_ack = [\"first_run\"]\n");
}

/// Remove the `first_run` ack so the next TUI launch shows the danger-full-access
/// modal. The P5.6 first-run tripwire calls this to undo
/// [`prepare_pipeline`]'s default ack seed.
pub fn clear_first_run_ack(home: &Path) {
    let _ = std::fs::remove_file(state_toml_path(home));
}

/// Pre-dismiss the notifyd first-run install prompt by writing an
/// `InstallRecord` with `prompt_dismissed = true` to
/// `{home}/.agents-in-a-box/install.json` (the exact shape
/// `ainb-plugin-notifyd/src/install.rs` deserializes). Without it the host
/// raises the "Get notified when a session needs you?" ConfirmationDialog on
/// every launch under a fresh `$HOME`, and that dialog intercepts all nav keys.
fn seed_notify_prompt_dismissed(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let _ = std::fs::create_dir_all(&base);
    let _ = std::fs::write(
        base.join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"claude_plugin_dir\":null,\
         \"codex_hooks_json\":null,\"plugin_version\":null,\"prompt_dismissed\":true}\n",
    );
}

/// Write a completed `onboarding.toml` under the isolated `$HOME` so the wizard
/// is skipped.
///
/// `needs_onboarding` re-triggers the wizard when the saved **major** version
/// differs from `ainb`'s own `CARGO_PKG_VERSION`. This crate's version (`0.x`)
/// is NOT `ainb`'s (`1.x`), so we must write `ainb`'s version — read from the
/// workspace `[workspace.package].version` in the root `Cargo.toml` rather than
/// `env!("CARGO_PKG_VERSION")` (which would be the daemon crate's `0.1.0` and
/// leave the wizard intercepting every keystroke).
fn seed_onboarding(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let version = workspace_version();
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{version}\"\nskipped_dependencies = []\ngit_directories = []\n"
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("write onboarding.toml");
}

/// Read `[workspace.package].version` from the workspace root `Cargo.toml`.
///
/// Resolved from the manifest dir at compile time, then walked up to the
/// workspace root. Falls back to `"1.0.0"` (the current major) if the file can't
/// be read — only the major is gated by `needs_onboarding`.
fn workspace_version() -> String {
    // .../ainb-tui/crates/ainb-hangar-daemon → up two to ainb-tui (workspace root).
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root_cargo = manifest_dir.parent().and_then(Path::parent).map(|p| p.join("Cargo.toml"));
    if let Some(path) = root_cargo {
        if let Ok(text) = std::fs::read_to_string(&path) {
            // Find the `[workspace.package]` section's `version = "x.y.z"`.
            let mut in_pkg = false;
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_pkg = t == "[workspace.package]";
                    continue;
                }
                if in_pkg {
                    if let Some(rest) = t.strip_prefix("version") {
                        if let Some(v) = rest.split('"').nth(1) {
                            return v.to_string();
                        }
                    }
                }
            }
        }
    }
    "1.0.0".to_string()
}

/// Seed the P4 fixture into `{hangar_dir}/hangar.db` via a one-shot tokio runtime.
fn seed_database(hangar_dir: &Path) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open seed store");
        ainb_hangar_daemon::seed::seed_p4_fixture(store.pool())
            .await
            .expect("seed P4 fixture");
    });
}

/// Insert one extra task per non-`running` board column into an already-seeded
/// `{home}/.ainb/hangar.db` so the Kanban board (`K`) has a card in every
/// column.
///
/// The P4 fixture (`seed_p4_fixture`) lands a single `running` task (`task-1`)
/// against `issue-1`. To prove the four-column board renders cards across the
/// whole lifecycle, this adds one `queued`, one `done`, and one `failed` task
/// on the same workspace / runtime / agent the fixture seeded. Each gets a
/// distinct `id` so the board's `#<short_id>` card identifier is greppable. The
/// inserts carry **no** `issue_id` (`NULL`) so the partial-unique
/// `idx_one_pending_task_per_issue_agent` index never collides with the
/// fixture's `issue-1` task.
///
/// Must run after [`prepare_pipeline`] (which seeds the fixture) and against the
/// same isolated `$HOME`. Panics on any insert failure (the caller is already
/// gated by [`can_run_tripwire`]).
pub fn seed_kanban_spread(home: &Path) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("kanban-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open kanban-seed store");
        let pool = store.pool();
        // (id, status). `created_at` is the fixture's fixed epoch so card ages
        // are deterministic; the `running` task already comes from the fixture.
        // The ids end in a unique 6-char suffix (`kq0001` / `kd0002` / `kf0003`)
        // so the board's `#<short_id>` (last 6 chars) is a distinctive,
        // greppable token that can never alias a column header label
        // (`queued` / `done` / `failed`).
        let now: i64 = 1_700_000_000_000;
        for (id, status) in [
            ("task-kanban-kq0001", "queued"),
            ("task-kanban-kd0002", "done"),
            ("task-kanban-kf0003", "failed"),
        ] {
            sqlx::query(
                "INSERT INTO agent_task_queue \
                 (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
                 VALUES (?, ?, ?, ?, NULL, ?, ?)",
            )
            .bind(id)
            .bind(ainb_hangar_daemon::seed::WS_ID)
            .bind("runtime-1")
            .bind("agent-1")
            .bind(status)
            .bind(now)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("seed kanban {status} task: {e}"));
        }
    });
}

/// Write an executable fake-`claude` under `dir` that drives a deterministic
/// **mix** of successful and failed runs, returning its path.
///
/// On each invocation the script reads + increments a counter file at
/// `$HOME/.fake-claude-count` (`$HOME` is the only stable, allowlisted env the
/// runner forwards — it is the daemon's isolated tempdir). The first
/// `fail_first` invocations emit a provider error and `exit 1`
/// ([`RunOutcome::Failed`] → `record_failed`, the sparkline's red band); every
/// later invocation emits a `system` + `result` line and `exit 0`
/// ([`RunOutcome::Success`] → `record_completed`, the green band). So a batch of
/// `n > fail_first` seeded tasks yields exactly `fail_first` failures + the rest
/// successes, populating the throughput ring with a known green/red shape.
pub fn fake_claude_mixed(dir: &Path, fail_first: u32) -> PathBuf {
    let path = dir.join("fake-claude-mixed.sh");
    let body = format!(
        "#!/bin/sh\n\
         COUNT_FILE=\"$HOME/.fake-claude-count\"\n\
         n=$(cat \"$COUNT_FILE\" 2>/dev/null || echo 0)\n\
         n=$((n + 1))\n\
         echo \"$n\" > \"$COUNT_FILE\"\n\
         if [ \"$n\" -le {fail_first} ]; then\n\
         \techo '{{\"type\":\"system\",\"session_id\":\"fail-'\"$n\"'\"}}'\n\
         \techo '{{\"type\":\"result\",\"content\":\"boom\"}}'\n\
         \texit 1\n\
         fi\n\
         echo '{{\"type\":\"system\",\"session_id\":\"ok-'\"$n\"'\"}}'\n\
         echo '{{\"type\":\"result\",\"content\":\"ok\"}}'\n\
         exit 0\n"
    );
    std::fs::write(&path, body).expect("write fake-claude-mixed");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// Mark the fixture's `task-1` (on `issue-1`) `done` with a `result.pr_url` so
/// the task-detail screen surfaces the PR badge (P9.2).
///
/// `issues_list` reads `result ->> 'pr_url'` from an issue's latest completed
/// task, so a completed `task-1` carrying `pr_url` makes `issue-1`'s wire row
/// (and thus the opened task detail) badge `pr_url`. Must run after
/// [`prepare_pipeline`] and against the same isolated `$HOME`.
pub fn seed_completed_task_with_pr(home: &Path, pr_url: &str) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("pr-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open pr-seed store");
        let result = format!("{{\"content\":\"done\",\"exit_code\":0,\"pr_url\":\"{pr_url}\"}}");
        sqlx::query(
            "UPDATE agent_task_queue \
             SET status = 'done', result = ?, finished_at = ? WHERE id = 'task-1'",
        )
        .bind(result)
        .bind(1_700_000_100_000i64)
        .execute(store.pool())
        .await
        .expect("seed completed task with pr");
    });
}

/// Enqueue `count` `queued` tasks (ids `seed-<prefix>-<i>`) on the seeded
/// `runtime-1` / `agent-1` / `default` workspace into `{home}/.ainb/hangar.db`,
/// so a claim-enabled daemon claims + executes them.
///
/// `created_at` is set to wall-clock "now" (not the fixture's 1970-relative
/// epoch) so the queued-TTL sweeper does not reap them before the claim loop
/// runs. Each task carries no `issue_id` (`NULL`) so the partial-unique pending
/// index never collides. Used by the daemon-health tripwire to drive real
/// completions into the throughput ring.
pub fn enqueue_tasks(home: &Path, prefix: &str, count: usize) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("enqueue runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open enqueue store");
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX);
        for i in 0..count {
            sqlx::query(
                "INSERT INTO agent_task_queue \
                 (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
                 VALUES (?, ?, ?, ?, NULL, 'queued', ?)",
            )
            .bind(format!("seed-{prefix}-{i}"))
            .bind(ainb_hangar_daemon::seed::WS_ID)
            .bind("runtime-1")
            .bind("agent-1")
            .bind(now_ms)
            .execute(store.pool())
            .await
            .unwrap_or_else(|e| panic!("enqueue seed task {prefix}-{i}: {e}"));
        }
    });
}

/// Raise the seeded `agent-1`'s `max_concurrent_tasks` to `cap` in
/// `{home}/.ainb/hangar.db`.
///
/// The P4 fixture seeds `agent-1` at the schema default cap of **1** and leaves
/// its `task-1` in the `running` state — which fully consumes that single slot,
/// so a claim-enabled daemon would never claim any further queued task. The
/// daemon-health tripwire raises the cap (and/or clears `task-1`) so the seeded
/// queue actually drains, driving the throughput ring.
pub fn set_agent_concurrency(home: &Path, cap: u32) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("cap runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open cap store");
        sqlx::query("UPDATE agent SET max_concurrent_tasks = ? WHERE id = 'agent-1'")
            .bind(i64::from(cap))
            .execute(store.pool())
            .await
            .expect("raise agent concurrency");
        // Free the fixture's running slot so the cap math counts only the
        // seeded batch (the fixture's `task-1` is a render-only prop here).
        sqlx::query(
            "UPDATE agent_task_queue SET status = 'done', finished_at = created_at \
                     WHERE id = 'task-1'",
        )
        .execute(store.pool())
        .await
        .expect("clear fixture running task");
    });
}

/// Multiplier applied to tmux render/interaction-wait budgets, read once from
/// `HANGAR_TRIPWIRE_BUDGET_SCALE` (default `1`, floored at `1`).
///
/// Hosted CI runners render the TUI much slower than a dev box, and the full
/// serial tripwire suite compounds it, so the dev-tuned poll budgets time out
/// on CI even though the code is correct (observed: scattered ~60s render-wait
/// timeouts on the macOS runner while the same suite is green locally). CI sets
/// this >1 to widen every budget that routes through it; locally it stays `1`
/// so the suite is fast. This is a deliberate budget bump (no retry), keeping
/// single-run rigor: a real regression still fails at the scaled deadline.
#[must_use]
pub fn budget_scale() -> u64 {
    std::env::var("HANGAR_TRIPWIRE_BUDGET_SCALE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(1)
}

/// Poll `{home}/.ainb/hangar.db` until at least `want` tasks with id prefix
/// `seed-<prefix>-` have reached a terminal status (`done`/`failed`/`cancelled`),
/// or `deadline` passes. Returns the terminal count actually observed.
///
/// The daemon-health tripwire uses this to wait for the claim-enabled daemon to
/// finish executing the seeded tasks (driving the throughput ring) before
/// opening the `D` screen.
pub fn wait_for_terminal(home: &Path, prefix: &str, want: usize, deadline: Instant) -> usize {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("wait runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open wait store");
        let like = format!("seed-{prefix}-%");
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_task_queue \
                 WHERE id LIKE ? AND status IN ('done','failed','cancelled')",
            )
            .bind(&like)
            .fetch_one(store.pool())
            .await
            .expect("count terminal tasks");
            let n = usize::try_from(n).unwrap_or(0);
            if n >= want || Instant::now() >= deadline {
                return n;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
}

/// Seed one cron-scheduled autopilot (`daily-triage`, `0 9 * * *`, enabled)
/// into an already-seeded `{home}/.ainb/hangar.db` so the autopilot-manager
/// screen (`5`) has a live row to render.
///
/// Inserts via the P7.2 [`AutopilotRepo::create`] path — the same path the
/// daemon's create flow uses — so the row carries a properly-computed,
/// strictly-future `next_tick_at` and is returned by the `hangar/autopilots_list`
/// RPC the screen pulls. It targets the fixture's `default`-slug workspace
/// ([`WS_ID`]) and `agent-1`, so the workspace-scoped snapshot finds it.
///
/// # Why this is its own helper (not part of `seed_p4_fixture`)
///
/// The spawned daemon runs the **real** autopilot scheduler on a
/// [`SystemClock`](ainb_hangar_core::clock::SystemClock). Seeding the autopilot
/// here (in *this* tripwire's RPC-only pipeline) rather than in the shared
/// fixture keeps it out of every other tripwire — and `create` computes
/// `next_tick_at` strictly after *now* for `0 9 * * *` (next 09:00 UTC, up to a
/// day away), so the scheduler parks until that future instant and never fires
/// the autopilot inside the test window. The fixture's other screens (issue
/// list, Kanban, daemon health, the autopilot-fires scheduler tripwire — which
/// seeds its OWN autopilots) are therefore untouched.
///
/// Must run after [`prepare_pipeline`] (which seeds `agent-1` + the workspace)
/// and against the same isolated `$HOME`. Panics on any insert failure (the
/// caller is already gated by [`can_run_tripwire`]).
pub fn seed_autopilot(home: &Path) {
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::ids::{AgentId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("autopilot-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open autopilot-seed store");
        let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("non-empty ws id");
        let agent = AgentId::from_str("agent-1").expect("non-empty agent id");
        AutopilotRepo::create(
            store.pool(),
            &SystemClock,
            &NewAutopilot {
                workspace_id: ws,
                agent_id: agent,
                name: "daily-triage".into(),
                instructions: Some("triage new issues".into()),
                // Daily at 09:00 UTC — `create` parks the scheduler on the next
                // future 09:00, so it never fires inside the test window.
                cron_expr: "0 9 * * *".into(),
                max_concurrent_runs: 1,
                execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
                concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(
                ),
            },
        )
        .await
        .expect("seed daily-triage autopilot");
    });
}

/// A greppable marker embedded in the seeded log line's message, distinctive
/// enough that it can never collide with a real daemon log message.
pub const LOGS_TRIPWIRE_MARKER: &str = "LOGS_TRIPWIRE_MARKER_42";

/// Seed three known structured-log lines (one `INFO` carrying
/// [`LOGS_TRIPWIRE_MARKER`], one `WARN`, one `ERROR`) into the daemon's rolling
/// JSONL log file so the Logs screen (`L`) has deterministic, level-diverse
/// content to render.
///
/// The Logs screen reads the **newest** `daemon.*` file in
/// `{home}/.ainb/hangar/logs` by mtime ([`ainb_hangar_core::logs::read_tail`]).
/// The spawned daemon already writes its own `daemon.<utc-date>` file on boot;
/// this **appends** the marker lines to that same dated file (creating it if the
/// daemon hasn't flushed yet), in the exact P8.1 wire shape (top-level `level`,
/// event message + custom fields nested under `fields`). Appending — rather than
/// writing a second file — both keeps the daemon's own lines and guarantees the
/// markers live in the newest file (the append bumps its mtime).
///
/// Must run after [`prepare_pipeline`] (which sets the isolated `$HOME` + spawns
/// the daemon). Panics on an IO failure (the caller is gated by
/// [`can_run_tripwire`]).
pub fn seed_logs(home: &Path) {
    use std::io::Write as _;

    let log_dir = home.join(".ainb").join("hangar").join("logs");
    std::fs::create_dir_all(&log_dir).expect("create logs dir");

    // Match the daemon's daily-rotated filename: `daemon.<YYYY-MM-DD>` in UTC
    // (`tracing_appender::rolling` with `Rotation::DAILY`). Appending to the
    // same dated file keeps the daemon's `ready` line and lands our markers in
    // the newest file the screen reads.
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let file = log_dir.join(format!("daemon.{date}"));

    // The P8.1 wire shape: top-level timestamp/level/target, the event message
    // and every custom field nested under `fields`. One INFO (carrying the
    // marker), one WARN, one ERROR so every level chip has something to surface.
    let now = chrono::Utc::now().to_rfc3339();
    let lines = [
        format!(
            "{{\"timestamp\":\"{now}\",\"level\":\"INFO\",\"target\":\"ainb_hangar_daemon\",\
             \"fields\":{{\"message\":\"daemon ready {LOGS_TRIPWIRE_MARKER}\",\"task_id\":\"t-seed-1\"}}}}"
        ),
        format!(
            "{{\"timestamp\":\"{now}\",\"level\":\"WARN\",\"target\":\"ainb_hangar_daemon::run_loop\",\
             \"fields\":{{\"message\":\"claim slot retry\",\"attempts\":2}}}}"
        ),
        format!(
            "{{\"timestamp\":\"{now}\",\"level\":\"ERROR\",\"target\":\"ainb_hangar_daemon::runner\",\
             \"fields\":{{\"message\":\"provider error\",\"code\":7}}}}"
        ),
    ];

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .expect("open daemon log file for append");
    for line in &lines {
        writeln!(f, "{line}").expect("append seeded log line");
    }
    f.flush().expect("flush seeded log lines");
}

/// A uniquely-named tmux session that kills itself by **exact name** on drop.
pub struct TuiSession {
    name: String,
}

impl TuiSession {
    /// Spawn `ainb tui` in a detached tmux session under `home` (the same
    /// isolated `$HOME` the daemon serves), plugins ACTIVE so the host discovers
    /// and loads `hangar-tui`. The staged plugin root is passed via
    /// `AINB_PLUGIN_ROOT` so discovery finds it regardless of cwd. Presses `g`
    /// from the home screen to open the Hangar plugin screen.
    ///
    /// Panics only on a tmux spawn failure (the caller has gated on
    /// [`can_run_tripwire`]).
    pub fn spawn(bin: &Path, home: &Path) -> Self {
        Self::spawn_with_env(bin, home, &[])
    }

    /// Like [`spawn`](Self::spawn) but layers `extra_env` (`KEY=value`) into the
    /// launched `ainb tui` command's environment.
    ///
    /// The host `ainb tui` process inherits these and passes them to the plugin
    /// subprocess it spawns. P9.2's tripwire uses this to set
    /// `HANGAR_OPENER_PROBE_FILE`, flipping the plugin's PR opener to a recording
    /// opener (so the `o` action writes to a probe file instead of launching a
    /// real browser).
    pub fn spawn_with_env(bin: &Path, home: &Path, extra_env: &[(&str, &str)]) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("hangar-p4-trip-{}-{nanos}", std::process::id());

        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", "180", "-y", "50"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed for {name}");

        // The staged plugin root: <workspace-root>/dist/plugins. Pass it via
        // AINB_PLUGIN_ROOT so discovery finds `hangar-tui` regardless of cwd
        // (the host probes <root>/<id>/<id>).
        let plugin_root = plugin_root()
            .map(|p| format!("AINB_PLUGIN_ROOT='{}' ", p.display()))
            .unwrap_or_default();

        let mut env_prefix = String::new();
        for (k, v) in extra_env {
            let _ = write!(env_prefix, "{k}='{v}' ");
        }

        let cmd = format!(
            "HOME='{}' {plugin_root}{env_prefix}exec '{}' tui",
            home.display(),
            bin.display()
        );
        Command::new("tmux")
            .args(["send-keys", "-t", &name, &cmd, "Enter"])
            .status()
            .expect("tmux send-keys launch");

        Self { name }
    }

    /// The exact session name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Send a single nav key WITHOUT `Enter` (per the skill's HARD RULE 3).
    pub fn send_key(&self, key: &str) {
        Command::new("tmux")
            .args(["send-keys", "-t", &self.name, key])
            .status()
            .expect("tmux send-keys");
    }

    /// Send the literal Enter key (for committing a selection / row open).
    pub fn send_enter(&self) {
        Command::new("tmux")
            .args(["send-keys", "-t", &self.name, "Enter"])
            .status()
            .expect("tmux send-keys Enter");
    }

    /// Type `text` as a single literal keystroke run (`send-keys -l`), so the
    /// whole string lands atomically rather than as separate per-char sends —
    /// which tmux can coalesce or drop on a busy pane (a dropped char would make
    /// a text-echo assertion flaky). `-l` tells tmux to treat every character
    /// verbatim (never as a key name), so an alphanumeric / punctuation title is
    /// typed exactly. No trailing Enter (the caller commits separately).
    pub fn type_literal(&self, text: &str) {
        Command::new("tmux")
            .args(["send-keys", "-t", &self.name, "-l", text])
            .status()
            .expect("tmux send-keys -l");
    }

    /// Send one raw SGR mouse escape sequence into the pane (63l.7).
    ///
    /// The TUI enables SGR mouse reporting (`EnableMouseCapture`, mode `1006`), so
    /// a pointer event reaches the app as `ESC [ < <btn> ; <col> ; <row> <m|M>`,
    /// where `M` is a button press / motion and `m` is a release. SGR coordinates
    /// are **1-based** (the top-left cell is `1;1`), so the caller passes 1-based
    /// `col`/`row`.
    ///
    /// `btn` is the SGR button code: `0` = left, `2` = right, plus the `32` motion
    /// bit for a drag (`0 | 32 = 32` left-drag), and `64` / `65` for wheel up /
    /// down. The raw bytes are sent verbatim via `send-keys -H` (hex), which never
    /// reinterprets them as tmux key names — the only reliable way to inject a
    /// control sequence with an embedded `ESC`.
    pub fn send_mouse_sgr(&self, btn: u16, col: u16, row: u16, press: bool) {
        let final_byte = if press { 'M' } else { 'm' };
        let seq = format!("\x1b[<{btn};{col};{row}{final_byte}");
        // Encode each byte as two hex digits — `send-keys -H` consumes a
        // whitespace-separated list of hex byte values.
        let hex: Vec<String> = seq.bytes().map(|b| format!("{b:02x}")).collect();
        let mut args = vec!["send-keys", "-H", "-t", &self.name];
        let hex_refs: Vec<&str> = hex.iter().map(String::as_str).collect();
        args.extend_from_slice(&hex_refs);
        Command::new("tmux").args(&args).status().expect("tmux send-keys -H mouse sgr");
    }

    /// Convenience: a left-button press at 1-based `(col, row)` (SGR button `0`).
    pub fn mouse_press(&self, col: u16, row: u16) {
        self.send_mouse_sgr(0, col, row, true);
    }

    /// Convenience: a left-button drag (motion with button held) to 1-based
    /// `(col, row)` — SGR button `32` (`0` left | `32` motion bit).
    pub fn mouse_drag(&self, col: u16, row: u16) {
        self.send_mouse_sgr(32, col, row, true);
    }

    /// Convenience: a left-button release at 1-based `(col, row)` (final byte `m`).
    pub fn mouse_release(&self, col: u16, row: u16) {
        self.send_mouse_sgr(0, col, row, false);
    }

    /// Convenience: a right-button press at 1-based `(col, row)` (SGR button `2`).
    pub fn mouse_right_press(&self, col: u16, row: u16) {
        self.send_mouse_sgr(2, col, row, true);
    }

    /// Capture the visible pane text (empty on error).
    pub fn capture(&self) -> String {
        Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &self.name])
            .output()
            .map_or_else(
                |_| String::new(),
                |o| String::from_utf8_lossy(&o.stdout).into_owned(),
            )
    }

    /// Capture the visible pane text **with SGR escape sequences** (`-e`), so the
    /// caller can grep for ANSI colour codes (the daemon-health sparkline's red
    /// failure band). Empty on error.
    pub fn capture_escaped(&self) -> String {
        Command::new("tmux")
            .args(["capture-pane", "-p", "-e", "-t", &self.name])
            .output()
            .map_or_else(
                |_| String::new(),
                |o| String::from_utf8_lossy(&o.stdout).into_owned(),
            )
    }

    /// Poll the pane until `pred` holds or `deadline` passes, returning the
    /// matching capture. No bare sleep: a 200ms inter-poll gap, deadline-bounded.
    pub fn poll_capture(&self, deadline: Instant, pred: impl Fn(&str) -> bool) -> Option<String> {
        loop {
            let cap = self.capture();
            if pred(&cap) {
                return Some(cap);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Wait up to 45s for the home screen, then press `g` to open the Hangar
    /// screen, then wait for the seeded issue-list landing screen.
    ///
    /// Returns the landing capture once `READY_MARKER` is visible, or `None` if
    /// the pipeline never rendered it within budget.
    pub fn open_hangar_and_wait_ready(&self) -> Option<String> {
        // Hosted CI runners render the TUI far slower than a dev box under the
        // full serial tripwire suite, so the dev-tuned render-wait budgets below
        // time out there (a runner-speed artifact, not a code fault). Scale them
        // by `HANGAR_TRIPWIRE_BUDGET_SCALE` (default 1 locally; CI sets >1).
        let scale = budget_scale();
        // 1. Wait for the home screen to be *interactive* — the home footer hint
        //    (`Enter select | Tab content | ↑↓ navigate`) only paints once the
        //    home screen owns the keyboard. Matching on a sidebar label alone
        //    races the initial tmux/session discovery and drops the `g` keystroke.
        let home = self.poll_capture(Instant::now() + Duration::from_secs(45 * scale), |c| {
            c.contains("Tab content") && c.contains("navigate")
        })?;
        // Negative: we are NOT already on the hangar issue list before pressing g.
        assert!(
            !home.contains(READY_MARKER),
            "issue list rendered before `g`:\n{home}"
        );

        // 2. Open the Hangar plugin screen (single-char nav, no Enter). The home
        //    screen's first frames race the initial tmux/session discovery, so a
        //    single `g` can be dropped. Re-send `g` every ~1.5s until the Hangar
        //    plugin chrome (its tab strip) replaces the host home screen, bounded
        //    by a deadline — then wait for the snapshot rows to fill in.
        let deadline = Instant::now() + Duration::from_secs(60 * scale);
        loop {
            self.send_key("g");
            if let Some(c) = self.poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
                hangar_chrome_visible(c)
            }) {
                // On the Hangar screen now. If rows already rendered, done.
                if c.contains(READY_MARKER) {
                    return Some(c);
                }
                break;
            }
            if Instant::now() >= deadline {
                return None;
            }
        }

        // 3. On the Hangar screen — wait for the seeded issue-list landing rows
        //    (they arrive after the plugin's snapshot fetch completes over the
        //    daemon socket).
        self.poll_capture(deadline, |c| c.contains(READY_MARKER))
    }

    /// From the Hangar issue-list landing screen, switch to a top-level tab by
    /// its single-char nav key (no `Enter`, per HARD RULE 3), retrying the key
    /// until `pred` holds on the captured pane or `deadline` passes.
    ///
    /// The first frames after a tab switch can race the plugin's snapshot fetch,
    /// so (like [`open_hangar_and_wait_ready`](Self::open_hangar_and_wait_ready))
    /// the key is re-sent every ~1.5s until the target screen's marker appears.
    /// Returns the matching capture, or `None` on timeout.
    pub fn switch_tab_until(
        &self,
        key: &str,
        deadline: Instant,
        pred: impl Fn(&str) -> bool,
    ) -> Option<String> {
        loop {
            self.send_key(key);
            if let Some(c) = self.poll_capture(Instant::now() + Duration::from_millis(1500), &pred)
            {
                return Some(c);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Convenience: spawn `ainb tui`, open the Hangar screen, and return the
    /// landing capture. Panics if the seeded screen never rendered.
    pub fn launch_to_hangar(bin: &Path, home: &Path) -> (Self, String) {
        let sess = Self::spawn(bin, home);
        let landing = sess
            .open_hangar_and_wait_ready()
            .unwrap_or_else(|| panic!("hangar issue list never rendered:\n{}", sess.capture()));
        (sess, landing)
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        // Exact-name kill only — never wildcard or kill-server.
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

/// `true` when the Hangar plugin chrome is on screen (its tab strip). Used to
/// confirm the `g` navigation switched to the plugin screen even before the
/// snapshot rows arrive — distinct from the host home screen.
pub fn hangar_chrome_visible(capture: &str) -> bool {
    capture.contains("Issues") && capture.contains("Skills") && capture.contains("Settings")
}

/// Print the canonical SKIP line and return — keeps every tripwire's skip path
/// identical and greppable.
pub fn skip(reason: &str) {
    eprintln!(
        "SKIP: {reason} (need tmux + built `ainb`/`ainb-hangar-daemon` + staged hangar-tui plugin; \
         run `just stage-plugins` + `cargo build -p ainb`)"
    );
}
