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
//! seed hangar.db ──▶ ainb-hangar-daemon (binds $HOME/.agents-in-a-box/hangar.sock)
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

/// `true` when the `git` binary is usable — the F5 worktree tripwires seed a real
/// repo + provision worktrees through it.
pub fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
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

/// The staged plugin root (`<workspace-root>/dist/plugins`).
///
/// `build-plugins.sh` stages into `ainb-tui/dist/plugins/<id>/<id>` — a path
/// relative to the *repo workspace root* (`cd "$(dirname "$0")/.."`), NOT under
/// any `target/` dir. So the anchor MUST be the repo, not the build output dir.
///
/// Primary anchor: `CARGO_MANIFEST_DIR` (compile-time; `<root>/crates/
/// ainb-hangar-daemon`). Its grandparent is the `ainb-tui` workspace root that
/// holds `dist/`. This is stable regardless of `CARGO_TARGET_DIR`.
///
/// Why not the test-binary location: under a shared/overridden `CARGO_TARGET_DIR`
/// (e.g. `~/.cache/ccc-shared-target`), `current_exe().parent×3` is that cache
/// dir and its parent is `~/.cache` — so the old logic resolved
/// `~/.cache/dist/plugins`, a path `build-plugins.sh` never writes to. The
/// tripwire then silently loaded whatever ancient binary happened to sit there
/// (a stale plugin with the pre-fix default hooks), passing or failing on dead
/// code instead of the freshly staged plugin. The exe-derived path is kept only
/// as a fallback for layouts where the repo-relative dist is absent.
pub fn plugin_root() -> Option<PathBuf> {
    // Primary: repo-relative dist, anchored on the compile-time manifest dir so
    // it always points at the tree `build-plugins.sh` stages into.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // <root>/crates/ainb-hangar-daemon
    if let Some(root) = manifest_dir.parent().and_then(Path::parent) {
        // crates → ainb-tui workspace root
        let p = root.join("dist").join("plugins");
        if p.exists() {
            return Some(p);
        }
    }
    // Fallback: the historical build-output-relative path, for layouts that stage
    // beside `target/` rather than in the repo.
    let exe = std::env::current_exe().ok()?;
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
/// daemon against it. The daemon resolves `~/.agents-in-a-box` from `$HOME` (no
/// `AINB_HANGAR_HOME`), binding `$HOME/.agents-in-a-box/hangar.sock` — the exact path the
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

/// Like [`prepare_pipeline`], plus two open `attention` rows (P2 control-center
/// tripwire): a NEWER `waiting` row and an OLDER `ask_user_question` row. Seeded
/// through the same pre-daemon connection [`prepare_pipeline_with_autopilot`]
/// uses, so the daemon's first `attention/subscribe` snapshot already carries
/// both — no live-push race.
///
/// The ASK row's `created_at` is deliberately EARLIER than the WAIT row's, so a
/// pane assertion that the ASK card renders above the WAIT card actually proves
/// the D9 urgency-rank shuffle ([`sort_cards`] in `control_center.rs`), not a
/// recency coincidence — a purely-recency sort would put the newer WAIT row
/// first.
pub fn prepare_pipeline_with_attention() -> Pipeline {
    prepare_pipeline_seeded(&[("HANGAR_DAEMON_DISABLE_CLAIM", "1")], seed_attention_pair)
}

/// The seeded ASK row's id — the tripwire answers this one.
pub const ATTENTION_ASK_ID: &str = "att-ask-1";
/// The seeded WAIT row's id.
pub const ATTENTION_WAIT_ID: &str = "att-wait-1";
/// The seeded ASK row's question text (rendered in the detail pane).
pub const ATTENTION_ASK_QUESTION: &str = "Ship to which env?";
/// The seeded ASK row's first option label (rendered next to the ① glyph).
pub const ATTENTION_ASK_OPTION_1: &str = "staging";
/// The seeded ASK row's second option label (rendered next to the ② glyph).
pub const ATTENTION_ASK_OPTION_2: &str = "prod";
/// The deliverable ASK's third option label (rendered next to the ③ glyph) —
/// the answer-flip tripwire ([`prepare_pipeline_answer_flip`]) seeds three
/// options so the recording asserts the full ①②③ span; it answers option 2.
pub const ATTENTION_ASK_OPTION_3: &str = "canary";
/// The session id the deliverable ASK is raised from. The fake `ainb list`
/// stub ([`write_fake_ainb`]) binds this id to a live tmux target so the answer
/// router's C1 resolve finds an exact-id match and the last-mile send delivers.
pub const ATTENTION_ASK_SESSION: &str = "s-deploy";

/// Seed one NEWER `waiting` row + one OLDER `ask_user_question` row into an
/// about-to-be-opened `hangar.db`, both scoped to no workspace (a hand-started
/// fleet session) so the fleet-wide `attention/subscribe` snapshot returns them
/// regardless of the seeded P4 fixture's workspace.
fn seed_attention_pair(home: &Path) {
    use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};

    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("attention-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open attention-seed store");
        let pool = store.pool();

        // Newer WAIT row (idle-at-prompt marker).
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: ATTENTION_WAIT_ID.to_string(),
                session_id: "s-idle".to_string(),
                cwd: "/work/idle".to_string(),
                workspace_id: None,
                kind: AttentionKind::Waiting,
                payload: serde_json::json!({
                    "kind": "WAIT",
                    "context": { "marker": "WAITING: still idle" }
                })
                .to_string(),
                degraded: false,
                created_at: 1_700_000_000_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .expect("seed waiting attention row");

        // Older ASK row (earlier `created_at` than the WAIT row above) — proves
        // the urgency rank (not recency) puts it on top.
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: ATTENTION_ASK_ID.to_string(),
                session_id: "s-deploy".to_string(),
                cwd: "/work/deploy".to_string(),
                workspace_id: None,
                kind: AttentionKind::AskUserQuestion,
                payload: serde_json::json!({
                    "kind": "ASK",
                    "context": {
                        "question": ATTENTION_ASK_QUESTION,
                        "options": [
                            { "label": ATTENTION_ASK_OPTION_1 },
                            { "label": ATTENTION_ASK_OPTION_2 },
                        ]
                    }
                })
                .to_string(),
                degraded: false,
                created_at: 1_699_999_000_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .expect("seed ask attention row");
    });
}

/// A plain-shell tmux session that receives the daemon's last-mile answer
/// delivery, killed by **exact name** on drop (never a wildcard / kill-server).
///
/// The answer-flip tripwire ([`prepare_pipeline_answer_flip`]) needs a live,
/// tmux-deliverable session bound to the seeded ASK's raising id so the answer
/// router's verified send actually lands the picked option somewhere — a plain
/// 80×24 shell pane accepts the `send-keys` exactly as the vhs harness
/// (`record-control-center.sh`) relied on.
pub struct DeliveryTarget {
    name: String,
}

impl DeliveryTarget {
    /// Stand up a detached 80×24 plain-shell tmux session. Panics only on a tmux
    /// spawn failure (the caller is gated by [`can_run_tripwire`]).
    #[must_use]
    pub fn spawn() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("hangar-answer-target-{}-{nanos}", std::process::id());
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", "80", "-y", "24"])
            .status()
            .expect("tmux new-session (delivery target)");
        assert!(status.success(), "tmux new-session failed for {name}");
        Self { name }
    }

    /// The exact session name — bound into the fake `ainb list` stub so the
    /// daemon resolves the ASK's target to this pane.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Capture the target pane's visible text (empty on error) — the tripwire
    /// asserts the delivered option landed here.
    #[must_use]
    pub fn capture(&self) -> String {
        Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &self.name])
            .output()
            .map_or_else(
                |_| String::new(),
                |o| String::from_utf8_lossy(&o.stdout).into_owned(),
            )
    }
}

impl Drop for DeliveryTarget {
    fn drop(&mut self) {
        // Exact-name kill only — never wildcard or kill-server.
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

/// Seed an isolated `$HOME` with the P4 fixture + a WAIT row + a THREE-option
/// **deliverable** ASK (raised from [`ATTENTION_ASK_SESSION`]), write a fake
/// `ainb list --format json` stub binding that session to `target_tmux`, and
/// spawn the daemon with `AINB_BIN=<fake>` + `AINB_FLEET_TRANSPORT=tmux-only`
/// so the answer router's C1 target-resolve + verified send actually deliver the
/// picked option into `target_tmux`.
///
/// This is the real-delivery sibling of [`prepare_pipeline_with_attention`]:
/// that one seeds a render/shuffle ASK but never makes it deliverable, so
/// pressing a digit cannot flip the row. This one closes the full loop
/// (answer RPC → C1 resolve → tmux delivery → `open`→`answered` → board refresh)
/// — the path `agents-in-a-box-43e` flagged as untested, first driven only by
/// the `docs/hangar/assets/record-control-center.sh` vhs harness.
///
/// Panics only after [`can_run_tripwire`] has gated the caller.
pub fn prepare_pipeline_answer_flip(target_tmux: &str) -> Pipeline {
    let home = tempfile::tempdir().expect("isolated HOME tempdir");
    let hangar_dir = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(home.path());
    seed_database(&hangar_dir);
    seed_first_run_ack(home.path());
    seed_notify_prompt_dismissed(home.path());
    seed_deliverable_ask(home.path());

    // The fake `ainb list` stub the daemon shells out to for session discovery.
    let fake = write_fake_ainb(home.path(), target_tmux);

    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        // Point discovery at the stub + force tmux-only delivery so the seeded
        // ASK's target (`s-deploy`) resolves to the real tmux pane the tripwire
        // stood up (mirrors `seed_control_center.rs`).
        .env("AINB_BIN", &fake)
        .env("AINB_FLEET_TRANSPORT", "tmux-only")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let daemon = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    Pipeline { home, daemon }
}

/// The board the card-run tripwire seeds (Todo manual + Done↦done auto-move),
/// with NO cards — the TUI creates the card interactively.
pub const BOARD_RUN_BOARD: &str = "board-run-1";
/// The card-run board's manual `Todo` column id (the create lands the card here).
pub const BOARD_RUN_TODO_COL: &str = "board-run-todo";
/// The card-run board's `done`-mapped auto-move `Done` column id.
pub const BOARD_RUN_DONE_COL: &str = "board-run-done";
/// The assignee profile the card-run tripwire seeds — its slug matches the P4
/// fixture agent's NAME (`claude-agent`, D16) so the create resolves the assignee
/// to a runnable agent + runtime.
pub const BOARD_RUN_PROFILE: &str = "claude-agent";

/// Seed the P4 fixture + a board (`Todo` manual, `Done`↦`done` auto-move) + a
/// `claude-agent` profile, write a happy fake-claude, and spawn a CLAIM-ENABLED
/// daemon so a card run enqueued from the TUI actually executes to `done` and the
/// D8 auto-move hook slides the card into `Done`. NO cards are pre-seeded — the
/// tripwire creates the card interactively, which is the whole point.
///
/// Panics only after [`can_run_tripwire`] has gated the caller.
pub fn prepare_pipeline_board_run() -> Pipeline {
    // A happy fake-claude the claim loop runs the card task through to `done`.
    prepare_pipeline_board_run_with(
        "#!/bin/sh\necho '{\"type\":\"system\",\"session_id\":\"board-run-sess\"}'\n\
         echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n",
    )
}

/// The sentinel file the interactive stub agent blocks on (under the isolated
/// `$HOME`) — the tripwire touches it to release the agent AFTER it has observed
/// the live tmux session, making the "session is real" assertion race-free.
pub const INTERACTIVE_RELEASE_SENTINEL: &str = "interactive-go";

/// Like [`prepare_pipeline_board_run`], but the fake agent BLOCKS until the
/// tripwire touches `$HOME/interactive-go`, then exits 0 (ccc / D6 interactive
/// tripwire). This lets the tripwire observe the REAL `tmux_hangar-<task_id>`
/// session while it is live before releasing the agent to finish (→ done →
/// auto-move). The stub self-exits after ~10s if never released, so a wiring bug
/// can never wedge a session.
///
/// Panics only after [`can_run_tripwire`] has gated the caller.
pub fn prepare_pipeline_board_run_interactive() -> Pipeline {
    prepare_pipeline_board_run_with(
        "#!/bin/sh\ni=0\nwhile [ ! -f \"$HOME/interactive-go\" ] && [ \"$i\" -lt 100 ]; do \
         sleep 0.1; i=$((i+1)); done\nexit 0\n",
    )
}

/// Shared body of [`prepare_pipeline_board_run`] and its interactive sibling:
/// seed the board-run fixture + profile and spawn a CLAIM-ENABLED daemon whose
/// `HANGAR_CLAUDE_PATH` is a fake agent with `fake_agent_body`.
fn prepare_pipeline_board_run_with(fake_agent_body: &str) -> Pipeline {
    prepare_pipeline_board_run_seeded(fake_agent_body, |_| {}, |_| Vec::new())
}

/// [`prepare_pipeline_board_run_with`], plus a `pre_spawn(home)` hook run while no
/// daemon is attached (to add repo/scan-cache fixture rows) and an `extra_env`
/// closure — computed against the isolated `$HOME` so it can point env vars at
/// files it wrote there (e.g. a stub `gh` under `HANGAR_GH_PATH`). The F5 worktree
/// tripwires use this to seed a real git repo + widen the daemon env.
fn prepare_pipeline_board_run_seeded(
    fake_agent_body: &str,
    pre_spawn: impl FnOnce(&Path),
    extra_env: impl FnOnce(&Path) -> Vec<(String, String)>,
) -> Pipeline {
    let home = tempfile::tempdir().expect("isolated HOME tempdir");
    let hangar_dir = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

    seed_onboarding(home.path());
    seed_database(&hangar_dir);
    seed_first_run_ack(home.path());
    seed_notify_prompt_dismissed(home.path());
    seed_board_and_profile(&hangar_dir);
    // Free the fixture's claim slot BEFORE the claim-enabled daemon starts.
    //
    // The P4 fixture seeds `agent-1` at the schema default cap of 1 and leaves
    // its `task-1` in `running`, which fully consumes that one slot — so the
    // card this pipeline's tripwire enqueues could never be claimed. It only
    // ever worked by accident: the fixture used to be stamped in 2023, so the
    // stale-running sweeper failed `task-1` on its first tick and freed the
    // slot. With the fixture on a live clock that accident is gone, so the slot
    // is now freed deliberately (`task-1` is a render-only prop here).
    set_agent_concurrency(home.path(), 10);
    pre_spawn(home.path());

    let fake_claude = write_executable(home.path(), "fake-claude.sh", fake_agent_body);
    let extra_env = extra_env(home.path());

    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let mut cmd = Command::new(bin);
    cmd.env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        // Claim-enabled on the fixture's runtime, with a fast poll so the enqueued
        // card task claims + runs promptly.
        .env("HANGAR_DAEMON_RUNTIME_ID", "runtime-1")
        .env("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap())
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in &extra_env {
        cmd.env(k, v);
    }
    let daemon = cmd.spawn().expect("spawn ainb-hangar-daemon");

    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Pipeline { home, daemon }
}

// ---------------------------------------------------------------------------
// F5 worktree-provisioning tripwire fixtures + helpers (spec F5 / tcp T1).
// ---------------------------------------------------------------------------

/// The scan-cache name of the real git repo the worktree tripwires seed under the
/// isolated `$HOME` — the card-create `@` dropdown offers it WITH a local path,
/// so a run provisions a real volatile worktree (favorites carry no local path).
pub const WORKTREE_REPO_NAME: &str = "testrepo";

/// A headless fake-claude that BLOCKS until the tripwire touches
/// `$HOME/interactive-go`, then emits a success result and exits 0 — so the
/// tripwire can observe the LIVE worktree before releasing the run to finalize +
/// tear down. Self-exits after ~30s so a wiring bug can never wedge a run.
///
/// The worktree fixtures run this HEADLESS with the OS sandbox DISABLED
/// (`HANGAR_DAEMON_DISABLE_SANDBOX=1`) so the agent can read the `$HOME` sentinel
/// (which lives outside the sandbox's task-root + worktree write roots). The
/// sandbox WIDENING to the worktree is a unit/code concern (`runner.rs`); these
/// tripwires target the worktree LIFECYCLE (provision on `ainb/<slug>` → clean
/// teardown), so disabling the sandbox isolates that behaviour.
const BLOCKING_FAKE_AGENT: &str = "#!/bin/sh\ni=0\nwhile [ ! -f \"$HOME/interactive-go\" ] && \
     [ \"$i\" -lt 300 ]; do sleep 0.1; i=$((i+1)); done\n\
     echo '{\"type\":\"system\",\"session_id\":\"worktree-sess\"}'\n\
     echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n";

/// Seed a real git repo at `$HOME/<name>` (one commit, so `git worktree add -b`
/// has a HEAD to branch from) + a `cache/repositories.json` scan cache pointing at
/// it, so the daemon's `hangar/repo_list` offers it in the card-create `@` roster
/// with a local checkout path (→ a run provisions a real worktree).
pub fn seed_scanned_repo(home: &Path, name: &str) {
    let repo = home.join(name);
    std::fs::create_dir_all(&repo).expect("create scanned repo dir");
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "t@e.com"],
        &["config", "user.name", "t"],
    ] {
        let ok = Command::new("git").args(args).current_dir(&repo).status();
        assert!(
            ok.is_ok_and(|s| s.success()),
            "git {args:?} in the scanned repo"
        );
    }
    std::fs::write(repo.join("README.md"), "seed").expect("write scanned repo README");
    for args in [&["add", "."][..], &["commit", "--quiet", "-m", "seed"]] {
        let ok = Command::new("git").args(args).current_dir(&repo).status();
        assert!(
            ok.is_ok_and(|s| s.success()),
            "git {args:?} in the scanned repo"
        );
    }

    let cache_dir = home.join(".agents-in-a-box").join("cache");
    std::fs::create_dir_all(&cache_dir).expect("create scan-cache dir");
    let json = format!(
        "{{\"version\":1,\"repositories\":[{{\"path\":\"{}\",\"name\":\"{name}\"}}]}}",
        repo.display()
    );
    std::fs::write(cache_dir.join("repositories.json"), json).expect("write scan cache");
}

/// Like [`prepare_pipeline_board_run_interactive`], but ALSO seeds a real git repo
/// (`$HOME/testrepo`) into the `@` scan cache and disables the daemon sandbox, so
/// a card RUN provisions a real volatile worktree on `ainb/<slug>` the tripwire
/// can observe live, then finalises + tears it down (spec F5). The fake agent
/// BLOCKS on the release sentinel so the live-worktree assertion is race-free.
pub fn prepare_pipeline_worktree_run() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        BLOCKING_FAKE_AGENT,
        |home| seed_scanned_repo(home, WORKTREE_REPO_NAME),
        |_| vec![disable_sandbox()],
    )
}

/// The canonical `gh pr create` PR URL the committing stub agent prints on its own
/// stdout line, captured into the task's `result.pr_url` by the P9.1 scan (tcp T2).
pub const WORKTREE_PR_URL: &str = "https://github.com/o/r/pull/42";

/// A HEADLESS fake-claude that makes a REAL commit in its worktree cwd, then prints
/// a canonical `gh pr create` PR URL line + a success result, and exits 0 (tcp T2).
///
/// The commit leaves the `ainb/<slug>` branch ahead of its base, so finalize
/// records the branch on the task; the printed PR URL is captured into
/// `result.pr_url`. Together they drive the card's branch + PR surfacing end to
/// end. Runs with the sandbox DISABLED so `git commit` can write the shared repo's
/// refs (the worktree's `.git` points into the origin repo, outside the sandbox's
/// worktree write-root). The repo's `user.name`/`user.email` come from the seeded
/// repo config (worktrees share it); set again defensively so the commit never
/// fails on identity.
const COMMITTING_FAKE_AGENT: &str = "#!/bin/sh\n\
     git config user.email t@e.com 2>/dev/null\n\
     git config user.name t 2>/dev/null\n\
     printf 'agent work\\n' > agent_output.txt\n\
     git add -A >/dev/null 2>&1\n\
     git commit --quiet -m 'agent commit' >/dev/null 2>&1\n\
     echo 'https://github.com/o/r/pull/42'\n\
     echo '{\"type\":\"system\",\"session_id\":\"pr-sess\"}'\n\
     echo '{\"type\":\"result\",\"content\":\"opened PR\"}'\n\
     exit 0\n";

/// Like [`prepare_pipeline_worktree_run`], but the fake agent COMMITS in its
/// worktree + prints a PR URL, and the daemon's `gh` is a stub (via
/// `HANGAR_GH_PATH`) that answers a passing, mergeable, open PR (tcp T2). Drives
/// the card branch + PR + CI surfacing to a completed run — never touching real
/// GitHub.
pub fn prepare_pipeline_worktree_pr() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        COMMITTING_FAKE_AGENT,
        |home| seed_scanned_repo(home, WORKTREE_REPO_NAME),
        |home| {
            let gh = write_stub_gh(home);
            vec![
                disable_sandbox(),
                (
                    "HANGAR_GH_PATH".to_string(),
                    gh.to_string_lossy().into_owned(),
                ),
            ]
        },
    )
}

/// The `HANGAR_DAEMON_DISABLE_SANDBOX=1` env pair the worktree fixtures widen the
/// daemon with (so a headless agent can read the release sentinel / write repo refs).
fn disable_sandbox() -> (String, String) {
    ("HANGAR_DAEMON_DISABLE_SANDBOX".to_string(), "1".to_string())
}

/// Write a stub `gh` under `$HOME` that answers `gh pr view … --json …` with a
/// canned passing / mergeable / open PR, so the daemon's `GhPrStatusProvider`
/// (pointed here via `HANGAR_GH_PATH`) resolves a real `PrStatus` WITHOUT touching
/// GitHub. Returns the stub's path (→ the env var value). Any args are ignored.
fn write_stub_gh(home: &Path) -> PathBuf {
    let body = "#!/bin/sh\n\
        cat <<'JSON'\n\
        {\"mergeable\":\"MERGEABLE\",\"state\":\"OPEN\",\"statusCheckRollup\":\
        [{\"__typename\":\"CheckRun\",\"conclusion\":\"SUCCESS\",\"status\":\"COMPLETED\",\"name\":\"Test\"}]}\n\
        JSON\n";
    write_executable(home, "stub-gh.sh", body)
}

/// Like [`prepare_pipeline_worktree_run`], but raises the fixture agent's
/// concurrency cap to 2 (and frees the fixture's running slot) so TWO tasks on the
/// SAME repo run concurrently — the a54 "N tasks on one repo never collide"
/// guarantee (spec F5): each provisions its own distinct `ainb/<slug>` worktree.
pub fn prepare_pipeline_worktree_concurrent() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        BLOCKING_FAKE_AGENT,
        |home| {
            seed_scanned_repo(home, WORKTREE_REPO_NAME);
            raise_agent_cap(home, 2);
        },
        |_| vec![disable_sandbox()],
    )
}

/// Raise the seeded `agent-1`'s `max_concurrent_tasks` to `cap` and free the
/// fixture's `running` `task-1`, so the claim loop actually runs `cap` tasks at
/// once. A pre-spawn variant of [`set_agent_concurrency`] (which needs a spawned
/// pipeline's `$HOME`); here it runs while no daemon is attached.
fn raise_agent_cap(home: &Path, cap: u32) {
    let hangar_dir = home.join(".agents-in-a-box");
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
            .expect("raise agent cap");
        sqlx::query(
            "UPDATE agent_task_queue SET status = 'done', finished_at = created_at \
             WHERE id = 'task-1'",
        )
        .execute(store.pool())
        .await
        .expect("free fixture running slot");
    });
}

/// Enqueue a `queued` task (id `wt-<suffix>`, NO issue) on the seeded
/// `runtime-1`/`agent-1`/`default` workspace carrying `repo_ref`, so a
/// claim-enabled daemon claims it and provisions a worktree from that repo. Used
/// by the concurrency tripwire (two tasks, one repo). Returns the task id.
pub fn enqueue_task_with_repo(home: &Path, suffix: &str, repo_ref: &str) -> String {
    let hangar_dir = home.join(".agents-in-a-box");
    let id = format!("wt-{suffix}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("enqueue-repo runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open enqueue-repo store");
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, repo_ref, created_at) \
             VALUES (?, ?, ?, ?, NULL, 'queued', ?, ?)",
        )
        .bind(&id)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind("runtime-1")
        .bind("agent-1")
        .bind(repo_ref)
        .bind(now_ms)
        .execute(store.pool())
        .await
        .unwrap_or_else(|e| panic!("enqueue repo task {id}: {e}"));
    });
    id
}

// ---------------------------------------------------------------------------
// tcp T4 (F7) fixtures: squad-from-card fan-out + card dependencies.
// ---------------------------------------------------------------------------

/// The squad the squad-card fixture creates (leader `agent-1` + two members).
pub const T4_SQUAD_ID: &str = "squad-t4";
/// The squad card's issue id (assigned to [`T4_SQUAD_ID`], placed on the board).
pub const T4_SQUAD_CARD_ISSUE: &str = "issue-squad-card";
/// The free-text ROLE stamped on squad member `agent-m1` (parity #25).
pub const T4_SQUAD_M1_ROLE: &str = "owns the migrations";
/// The skill attached (enabled) to squad member `agent-m1` (parity `7-rest`).
pub const T4_SQUAD_M1_SKILL: &str = "pathfinding";
/// The squad's user-authored routing instructions (parity #25), rendered
/// VERBATIM into the leader briefing.
pub const T4_SQUAD_INSTRUCTIONS: &str = "Route schema work to the DB owner.";
/// The dependency-chain BLOCKER card's issue id (card A).
pub const T4_DEP_BLOCKER_ISSUE: &str = "issue-dep-a";
/// The dependency-chain DEPENDENT card's issue id (card B, depends-on A, auto-run on).
pub const T4_DEP_DEPENDENT_ISSUE: &str = "issue-dep-b";
/// The card RELATED to the blocker (multica parity #20): it is seeded into the
/// SAME dep-chain pipeline (a second pipeline would double the fixture cost, and
/// this suite already flakes under concurrency) with `auto_run` ON, so the only
/// thing that can explain it never auto-running is its link KIND.
pub const T4_REL_ISSUE: &str = "issue-dep-r";

/// An agent actor-ref for the T4 squad seeding.
fn t4_agent_ref(id: &str) -> ainb_hangar_core::actor::ActorRef {
    ainb_hangar_core::actor::ActorRef::new(ainb_hangar_core::actor::ActorKind::Agent, id)
        .expect("valid agent ref")
}

/// Insert an extra agent bound to the EXISTING fixture `runtime-1` (a distinct
/// agent on the same runtime, owned by the seeded `user-1`), so the claim loop
/// runs it concurrently with the other agents on that runtime.
async fn seed_extra_agent_on_runtime_1(pool: &sqlx::SqlitePool, id: &str, name: &str) {
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, 'runtime-1', 'workspace', 'user-1')",
    )
    .bind(id)
    .bind(ainb_hangar_daemon::seed::WS_ID)
    .bind(name)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed extra agent {id}: {e}"));
}

/// Insert a card issue titled `title` in the fixture workspace and place it on the
/// seeded board's Todo column with `repo_ref` set (so a run provisions a worktree).
async fn seed_card_issue(pool: &sqlx::SqlitePool, id: &str, title: &str, repo_ref: &str, now: i64) {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::board::BoardRepo;
    use ainb_hangar_store::repo::card_parity::CardParityRepo;

    let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, 'member', 'user-1', ?)",
    )
    .bind(id)
    .bind(ainb_hangar_daemon::seed::WS_ID)
    .bind(title)
    .bind(now)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed card issue {id}: {e}"));
    BoardRepo::card_add(
        pool,
        &ws,
        BOARD_RUN_BOARD,
        id,
        Some(BOARD_RUN_TODO_COL),
        now,
    )
    .await
    .unwrap_or_else(|e| panic!("place card {id}: {e}"));
    CardParityRepo::set_issue_repo_agent(
        pool,
        ainb_hangar_daemon::seed::WS_ID,
        id,
        Some(repo_ref),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("set repo on card {id}: {e}"));
}

/// Free the fixture's seeded `running` `task-1` so its agent (`agent-1`, the squad
/// leader / the dep-chain runner) is under its concurrency cap and claimable.
async fn free_fixture_running_task(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "UPDATE agent_task_queue SET status = 'done', finished_at = created_at WHERE id = 'task-1'",
    )
    .execute(pool)
    .await
    .expect("free fixture running task-1");
}

/// Spawn a CLAIM-ENABLED daemon over a fixture seeded for the SQUAD-CARD fan-out
/// tripwire (tcp T4 / F7): a real git repo in the `@` scan cache, two extra member
/// agents on `runtime-1`, a squad (`shippers`, leader `agent-1` + the two members),
/// and a card assigned to that squad with the repo set. The blocking fake-claude
/// holds every fanned run so the "N live worktrees at once" assertion is race-free;
/// the sandbox is disabled so the agent can read the release sentinel.
pub fn prepare_pipeline_squad_card() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        BLOCKING_FAKE_AGENT,
        |home| {
            seed_scanned_repo(home, WORKTREE_REPO_NAME);
            let hangar_dir = home.join(".agents-in-a-box");
            let repo_path = home.join(WORKTREE_REPO_NAME).to_string_lossy().into_owned();
            let now = 1_700_000_100_000_i64;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("squad-seed runtime");
            rt.block_on(async {
                use ainb_hangar_core::ids::WorkspaceId;
                use ainb_hangar_store::repo::card_parity::CardParityRepo;
                use ainb_hangar_store::repo::squad::SquadRepo;

                let store =
                    ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open squad store");
                let pool = store.pool();
                let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");

                seed_extra_agent_on_runtime_1(pool, "agent-m1", "member-one").await;
                seed_extra_agent_on_runtime_1(pool, "agent-m2", "member-two").await;
                SquadRepo::create(
                    pool,
                    &ws,
                    T4_SQUAD_ID,
                    "shippers",
                    &t4_agent_ref("agent-1"),
                    now,
                )
                .await
                .expect("create squad");
                // m1 carries a ROLE and a live SKILL, and the squad carries
                // INSTRUCTIONS, so the leader-briefing tripwire can read all three
                // fragments off the CLAUDE.md the real daemon materialises
                // (parity #25 + `7-rest`). m2 stays bare, pinning the blank-omit.
                SquadRepo::add_member_with_role(
                    pool,
                    &ws,
                    T4_SQUAD_ID,
                    &t4_agent_ref("agent-m1"),
                    T4_SQUAD_M1_ROLE,
                )
                .await
                .expect("add m1");
                SquadRepo::add_member(pool, &ws, T4_SQUAD_ID, &t4_agent_ref("agent-m2"))
                    .await
                    .expect("add m2");
                SquadRepo::set_instructions(pool, &ws, T4_SQUAD_ID, T4_SQUAD_INSTRUCTIONS)
                    .await
                    .expect("set squad instructions");
                {
                    use ainb_hangar_core::ids::AgentId;
                    use ainb_hangar_store::repo::skill::SkillRepo;

                    let m1 = AgentId::from_str("agent-m1".to_string()).expect("m1 id");
                    let skill =
                        SkillRepo::create(pool, &ws, T4_SQUAD_M1_SKILL, None, Some("# b"), vec![])
                            .await
                            .expect("create m1 skill");
                    SkillRepo::attach_to_agent(pool, &ws, &m1, &skill)
                        .await
                        .expect("attach m1 skill");
                }

                seed_card_issue(
                    pool,
                    T4_SQUAD_CARD_ISSUE,
                    "SquadFanoutCard",
                    &repo_path,
                    now,
                )
                .await;
                CardParityRepo::set_issue_squad(pool, &ws, T4_SQUAD_CARD_ISSUE, Some(T4_SQUAD_ID))
                    .await
                    .expect("assign squad to card");
                free_fixture_running_task(pool).await;
            });
        },
        |_| vec![disable_sandbox()],
    )
}

/// Spawn a CLAIM-ENABLED daemon over a fixture seeded for the CARD-DEPENDENCY
/// chain tripwire (tcp T4 / F7): a real repo in the scan cache, two cards A + B
/// (each with the repo set) where B depends-on A and B's auto-run flag is on. The
/// blocking fake-claude holds A's run until the tripwire releases it; the sandbox
/// is disabled so the agent can read the sentinel.
pub fn prepare_pipeline_dep_chain() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        BLOCKING_FAKE_AGENT,
        |home| {
            seed_scanned_repo(home, WORKTREE_REPO_NAME);
            let hangar_dir = home.join(".agents-in-a-box");
            let repo_path = home.join(WORKTREE_REPO_NAME).to_string_lossy().into_owned();
            let now = 1_700_000_100_000_i64;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("dep-seed runtime");
            rt.block_on(async {
                use ainb_hangar_core::ids::WorkspaceId;
                use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

                let store =
                    ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open dep store");
                let pool = store.pool();
                let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");

                seed_card_issue(pool, T4_DEP_BLOCKER_ISSUE, "DepBlockerA", &repo_path, now).await;
                seed_card_issue(
                    pool,
                    T4_DEP_DEPENDENT_ISSUE,
                    "DepDependentB",
                    &repo_path,
                    now + 1,
                )
                .await;
                CardDependencyRepo::add_edge(
                    pool,
                    &ws,
                    T4_DEP_DEPENDENT_ISSUE,
                    T4_DEP_BLOCKER_ISSUE,
                    now,
                )
                .await
                .expect("B depends-on A");
                CardDependencyRepo::set_auto_run(pool, &ws, T4_DEP_DEPENDENT_ISSUE, true)
                    .await
                    .expect("B auto-run on");

                // multica parity #20: R is RELATED to A and also opts into
                // auto-run. A related link never gates and is invisible to the
                // finalize seam, so R must neither refuse a run nor gain a task
                // when A finishes — with auto_run equal on B and R, the LINK KIND
                // is the only difference between them.
                seed_card_issue(pool, T4_REL_ISSUE, "DepRelatedR", &repo_path, now + 2).await;
                CardDependencyRepo::add_link(
                    pool,
                    &ws,
                    T4_REL_ISSUE,
                    T4_DEP_BLOCKER_ISSUE,
                    ainb_hangar_store::repo::card_dependency::LinkKind::Related,
                    now,
                )
                .await
                .expect("R related-to A");
                CardDependencyRepo::set_auto_run(pool, &ws, T4_REL_ISSUE, true)
                    .await
                    .expect("R auto-run on");
                free_fixture_running_task(pool).await;
            });
        },
        |_| vec![disable_sandbox()],
    )
}

/// Count the `agent_task_queue` rows for `issue_id` in the fixture store (used by
/// the dependency-chain tripwire to prove B gains a task only after A completes).
#[must_use]
pub fn task_count_for_issue(home: &Path, issue_id: &str) -> i64 {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("task-count runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open count store");
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
            .bind(issue_id)
            .fetch_one(store.pool())
            .await
            .unwrap_or(0)
    })
}

/// Read the latest task status for `issue_id` in the fixture store, or `None` when
/// the issue has no task yet.
#[must_use]
pub fn latest_task_status_for_issue(home: &Path, issue_id: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("status runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM agent_task_queue WHERE issue_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(issue_id)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
    })
}

/// The id of the issue's NEWEST active (`queued`/`dispatched`/`running`) task, or
/// `None` when the set has drained — the exact task the OLD latest-wins logic keyed
/// on. The whole-squad-blocker proof transitions this newest sibling to `done` while
/// an OLDER one still runs: latest-wins would unblock the dependent there, the
/// whole-set contract must not.
#[must_use]
pub fn newest_active_task_for_issue(home: &Path, issue_id: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("newest-active runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM agent_task_queue \
             WHERE issue_id = ? AND status IN ('queued','dispatched','running') \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(issue_id)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
    })
}

/// The `CLAUDE.md` the daemon materialised into `task_id`'s task tree, or `None`
/// when it has not been written (yet).
///
/// This is the file the squad-leader briefing is appended to pre-spawn, so a
/// tripwire that reads it is asserting on the REAL injected prompt rather than a
/// re-built string. It lives in the TASK tree (not the run worktree), and the
/// tree is reclaimed after finalize — read it while the run is still held.
#[must_use]
pub fn materialised_context_prompt(home: &Path, task_id: &str) -> Option<String> {
    let path = home
        .join(".agents-in-a-box")
        .join("hangar")
        .join("workspaces")
        .join(ainb_hangar_daemon::seed::WS_SLUG)
        .join(task_id)
        .join("workdir")
        .join("CLAUDE.md");
    std::fs::read_to_string(path).ok()
}

/// Read one task row's status by its exact `task_id` in the fixture store, or `None`
/// when the id is absent. Backs the squad-cancel proof (every member id must reach
/// `cancelled`) and the whole-squad-blocker proof (per-member state).
#[must_use]
pub fn task_status_by_id(home: &Path, task_id: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("task-status runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = ?")
            .bind(task_id)
            .fetch_optional(store.pool())
            .await
            .ok()
            .flatten()
    })
}

/// Spawn a CLAIM-ENABLED daemon over a fixture where the dependency BLOCKER is a
/// SQUAD card (tcp T4 / FANOUT-SEMANTICS): the squad (`shippers`, leader `agent-1`
/// + two members) is assigned to blocker card A, and dependent card B depends-on A
/// with auto-run on. Proves B stays blocked until A's WHOLE fan-out set drains —
/// not merely until the latest member finishes. The blocking fake-claude holds
/// every fanned run; the sandbox is disabled so each agent can read the sentinel.
pub fn prepare_pipeline_squad_dep_chain() -> Pipeline {
    prepare_pipeline_board_run_seeded(
        BLOCKING_FAKE_AGENT,
        |home| {
            seed_scanned_repo(home, WORKTREE_REPO_NAME);
            let hangar_dir = home.join(".agents-in-a-box");
            let repo_path = home.join(WORKTREE_REPO_NAME).to_string_lossy().into_owned();
            let now = 1_700_000_100_000_i64;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("squad-dep-seed runtime");
            rt.block_on(async {
                use ainb_hangar_core::ids::WorkspaceId;
                use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
                use ainb_hangar_store::repo::card_parity::CardParityRepo;
                use ainb_hangar_store::repo::squad::SquadRepo;

                let store = ainb_hangar_store::Store::open_in(&hangar_dir)
                    .await
                    .expect("open squad-dep store");
                let pool = store.pool();
                let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("ws id");

                seed_extra_agent_on_runtime_1(pool, "agent-m1", "member-one").await;
                seed_extra_agent_on_runtime_1(pool, "agent-m2", "member-two").await;
                SquadRepo::create(
                    pool,
                    &ws,
                    T4_SQUAD_ID,
                    "shippers",
                    &t4_agent_ref("agent-1"),
                    now,
                )
                .await
                .expect("create squad");
                SquadRepo::add_member(pool, &ws, T4_SQUAD_ID, &t4_agent_ref("agent-m1"))
                    .await
                    .expect("add m1");
                SquadRepo::add_member(pool, &ws, T4_SQUAD_ID, &t4_agent_ref("agent-m2"))
                    .await
                    .expect("add m2");

                // Blocker A is the squad card; dependent B depends-on A, auto-run on.
                seed_card_issue(pool, T4_DEP_BLOCKER_ISSUE, "SquadBlockerA", &repo_path, now).await;
                seed_card_issue(
                    pool,
                    T4_DEP_DEPENDENT_ISSUE,
                    "DepDependentB",
                    &repo_path,
                    now + 1,
                )
                .await;
                CardParityRepo::set_issue_squad(pool, &ws, T4_DEP_BLOCKER_ISSUE, Some(T4_SQUAD_ID))
                    .await
                    .expect("assign squad to blocker A");
                CardDependencyRepo::add_edge(
                    pool,
                    &ws,
                    T4_DEP_DEPENDENT_ISSUE,
                    T4_DEP_BLOCKER_ISSUE,
                    now,
                )
                .await
                .expect("B depends-on A");
                CardDependencyRepo::set_auto_run(pool, &ws, T4_DEP_DEPENDENT_ISSUE, true)
                    .await
                    .expect("B auto-run on");
                free_fixture_running_task(pool).await;
            });
        },
        |_| vec![disable_sandbox()],
    )
}

/// A framed JSON-RPC client over the SPAWNED daemon's `hangar.sock` — a synchronous
/// wrapper (its own current-thread tokio runtime) so a non-async tripwire can drive
/// `board_card_run` etc. Models the framed `Client` in `rpc_board_management.rs`,
/// but dials the daemon BINARY's socket rather than an in-process server.
pub struct DaemonRpc {
    rt: tokio::runtime::Runtime,
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl DaemonRpc {
    /// Dial `$HOME/.agents-in-a-box/hangar.sock` (retrying until the daemon binds)
    /// and authenticate with the daemon token file — the mandatory first frame.
    pub fn connect_and_auth(home: &Path) -> Self {
        use tokio::net::UnixStream;
        let hangar_dir = home.join(".agents-in-a-box");
        let socket = hangar_dir.join("hangar.sock");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("daemon-rpc runtime");
        let (reader, writer) = rt.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(15);
            let stream = loop {
                match UnixStream::connect(&socket).await {
                    Ok(c) => break c,
                    Err(_) if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => panic!("never connected to daemon socket {socket:?}: {e}"),
                }
            };
            let (r, w) = stream.into_split();
            (tokio::io::BufReader::new(r), w)
        });
        let mut me = Self { rt, reader, writer };
        let token_path = ainb_hangar_proto::auth::token_file_in(&hangar_dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        let resp = me.call(
            ainb_hangar_proto::methods::AUTH_HELLO,
            serde_json::json!({ "token": token.trim() }),
        );
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
        me
    }

    /// Send one request and return the first id-bearing response frame (skipping any
    /// event push frames the daemon interleaves).
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let Self { rt, reader, writer } = self;
        rt.block_on(async move {
            let req = ainb_hangar_proto::RpcRequest {
                jsonrpc: ainb_hangar_proto::jsonrpc_version(),
                id: ainb_hangar_proto::RpcId::Number(7),
                method: method.into(),
                params,
            };
            let body = serde_json::to_vec(&req).expect("encode request");
            let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
            out.extend_from_slice(&body);
            writer.write_all(&out).await.expect("write request");
            writer.flush().await.expect("flush request");

            loop {
                let mut len: Option<usize> = None;
                let frame = loop {
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).await.expect("read header line");
                    assert!(n > 0, "connection closed awaiting a frame for {method}");
                    let t = line.trim_end_matches("\r\n");
                    if t.is_empty() {
                        let mut buf = vec![0u8; len.expect("Content-Length header")];
                        reader.read_exact(&mut buf).await.expect("read body");
                        break serde_json::from_slice::<serde_json::Value>(&buf)
                            .expect("decode frame");
                    }
                    if let Some((name, v)) = t.split_once(':') {
                        if name.trim().eq_ignore_ascii_case("Content-Length") {
                            len = v.trim().parse().ok();
                        }
                    }
                };
                if frame.get("id").is_some() {
                    return frame;
                }
            }
        })
    }
}

/// The daemon's per-run path slug for a task id — the worktree / task-tree dir name
/// and the `ainb/<slug>` branch suffix. Now the FULL task id (tcp vpm): the write
/// side keys the worktree / logs / output on the whole id so no truncated prefix can
/// cross-wire two runs, so a test that derives a run's on-disk path must use the
/// full id too. Kept as a helper (rather than inlining `task_id.to_string()`) so the
/// path-slug contract lives in one place beside `execenv::task_root`.
#[must_use]
pub fn task_short_id(task_id: &str) -> String {
    task_id.to_string()
}

/// The slug a SCRATCH card's durable dir is keyed on: `<issue-short-id>-<agent-short-id>`,
/// read off the latest task of the card with TITLE `title`. Stable across reruns
/// (same card + agent) and distinct per concurrent squad member (distinct agents on
/// one issue). `None` until a task has been dispatched.
#[must_use]
pub fn scratch_slug_by_title(home: &Path, title: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("scratch-slug runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT t.issue_id, t.agent_id FROM agent_task_queue t JOIN issue i ON t.issue_id = i.id \
             WHERE i.title LIKE ? ORDER BY t.created_at DESC, t.id DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten();
        row.map(|(issue, agent)| format!("{issue}-{agent}"))
    })
}

/// Read the short-id of the latest task on the card with issue TITLE `title`, the
/// slug the worktree dir + `ainb/<slug>` branch are keyed on (per-run; scratch is
/// keyed on (issue, agent) instead — see [`scratch_slug_by_title`]). `None` until a
/// task has been dispatched for that card.
#[must_use]
pub fn task_short_id_by_title(home: &Path, title: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("short-id runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        let id: Option<String> = sqlx::query_scalar(
            "SELECT t.id FROM agent_task_queue t JOIN issue i ON t.issue_id = i.id \
             WHERE i.title LIKE ? ORDER BY t.created_at DESC, t.id DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten();
        id.map(|id| task_short_id(&id))
    })
}

/// Read the `branch` recorded on the latest task of the card with issue TITLE
/// `title` (tcp T2) — the durable `ainb/<slug>` finalize stamps when the run left
/// commits. `None` until the run finalises with commits (or if it made none).
#[must_use]
pub fn task_branch_by_title(home: &Path, title: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("branch-read runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT t.branch FROM agent_task_queue t JOIN issue i ON t.issue_id = i.id \
             WHERE i.title LIKE ? ORDER BY t.created_at DESC, t.id DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
        .flatten()
    })
}

/// Read the `agent_kind` stamped on the latest task of the card with issue TITLE
/// `title` — the durable proof a run routed to the agent the card was edited to
/// (F6). `None` until a task has been enqueued for that card.
#[must_use]
pub fn task_agent_kind_by_title(home: &Path, title: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("task-agent runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        sqlx::query_scalar::<_, String>(
            "SELECT t.agent_kind FROM agent_task_queue t JOIN issue i ON t.issue_id = i.id \
             WHERE i.title LIKE ? ORDER BY t.created_at DESC, t.id DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
    })
}

/// Read the `(title, agent_kind)` persisted on the ISSUE (the durable card) whose
/// title matches `title` — the F6 edit-overlay proof the title rewrite + agent
/// pick landed on the card itself (before any run). `None` when no such issue.
#[must_use]
pub fn card_title_agent_by_title(home: &Path, title: &str) -> Option<(String, Option<String>)> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("card-agent runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT title, agent_kind FROM issue WHERE title LIKE ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
    })
}

/// The volatile-worktree checkout dir the daemon provisions for `slug`:
/// `$HOME/.agents-in-a-box/worktrees/<slug>`.
#[must_use]
pub fn worktree_dir(home: &Path, slug: &str) -> PathBuf {
    home.join(".agents-in-a-box").join("worktrees").join(slug)
}

/// The scratch repo dir the daemon provisions for `slug`:
/// `$HOME/.agents-in-a-box/scratch/<slug>`.
#[must_use]
pub fn scratch_dir(home: &Path, slug: &str) -> PathBuf {
    home.join(".agents-in-a-box").join("scratch").join(slug)
}

/// The branch a worktree run checks out for `slug` (`ainb/<slug>`).
#[must_use]
pub fn worktree_branch(slug: &str) -> String {
    format!("ainb/{slug}")
}

/// Whether `branch` exists in the git `repo` (`git branch --list <branch>` is
/// non-empty) — the proof a run provisioned an `ainb/<slug>` branch on the repo.
#[must_use]
pub fn git_branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
}

/// Read a board card's `(column_id, state)` from the isolated `$HOME`'s db by the
/// card's issue TITLE — the card-run tripwire's DB-level proof the interactively
/// created card auto-moved to `Done` and reads `state = done` (the green signal).
/// `None` when no card with that title exists yet.
#[must_use]
pub fn board_card_by_title(home: &Path, title: &str) -> Option<(Option<String>, Option<String>)> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("board-read runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let ws = ainb_hangar_daemon::seed::WS_ID;
        let boards =
            ainb_hangar_daemon::rpc::snapshots::boards_list(store.pool(), ws).await.ok()?;
        // Match by substring so a rare retry-dropped keystroke that prepends a
        // stray char to the typed title never breaks the lookup (there is only
        // one card in play).
        for board in &boards {
            for col in &board.columns {
                if let Some(card) = col.cards.iter().find(|c| c.title.contains(title)) {
                    return Some((Some(col.id.clone()), card.state.clone()));
                }
            }
            if let Some(card) = board.unmapped.iter().find(|c| c.title.contains(title)) {
                return Some((None, card.state.clone()));
            }
        }
        None
    })
}

/// Read the `session_name` the interactive runner recorded on the card's latest
/// task (matched by the card's issue TITLE) from the isolated `$HOME`'s db — the
/// exact `tmux_hangar-<task_id>` name the ccc / D6 interactive tripwire asserts a
/// live tmux session under. `None` while no task has been dispatched yet or the
/// task is headless (NULL `session_name`).
#[must_use]
pub fn interactive_session_for_title(home: &Path, title: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("session-name-read runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let like = format!("%{title}%");
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT t.session_name FROM agent_task_queue t \
             JOIN issue i ON t.issue_id = i.id \
             WHERE i.title LIKE ? \
             ORDER BY t.created_at DESC, t.id DESC LIMIT 1",
        )
        .bind(&like)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
        .flatten()
    })
}

/// Whether a tmux session with the EXACT name `name` is live (`tmux has-session`).
/// Used by the interactive tripwire to prove the runner spawned a REAL session and
/// later that it was reaped.
#[must_use]
pub fn tmux_session_live(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .is_ok_and(|s| s.success())
}

/// Seed the card-run board (`Todo` manual, `Done`↦`done` auto-move, no cards) into
/// the about-to-open `hangar.db`, plus the `claude-agent` assignee profile MASTER
/// on disk so the daemon's profile reconciler indexes it (a bare DB index row is
/// pruned on startup for having no master file — D14/T6).
fn seed_board_and_profile(hangar_dir: &Path) {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::board::BoardRepo;

    // The assignee profile master — the daemon's fs-watch reconciler indexes it on
    // startup, so `profile/list` offers `claude-agent` in the card-create picker.
    // Its slug matches the seeded agent NAME (D16) so the run routes to a runtime.
    let profiles_dir = hangar_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    std::fs::write(
        profiles_dir.join(format!("{BOARD_RUN_PROFILE}.md")),
        "---\nname: claude-agent\ndescription: Board card runner\nmodel: balanced\n---\nRun the card's issue.\n",
    )
    .expect("write assignee profile master");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("board-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(hangar_dir)
            .await
            .expect("open board-seed store");
        let pool = store.pool();
        let ws = WorkspaceId::from_str(ainb_hangar_daemon::seed::WS_ID).expect("non-empty ws id");
        let now = 1_700_000_000_000_i64;
        BoardRepo::create(pool, &ws, BOARD_RUN_BOARD, "Delivery", now)
            .await
            .expect("seed board");
        BoardRepo::column_add(
            pool,
            &ws,
            BOARD_RUN_BOARD,
            BOARD_RUN_TODO_COL,
            "Todo",
            None,
            false,
        )
        .await
        .expect("seed Todo column");
        BoardRepo::column_add(
            pool,
            &ws,
            BOARD_RUN_BOARD,
            BOARD_RUN_DONE_COL,
            "Done",
            Some("done"),
            true,
        )
        .await
        .expect("seed auto-move Done column");
    });
}

/// Seed one NEWER WAIT row + one OLDER **three-option** ASK raised from
/// [`ATTENTION_ASK_SESSION`] into an about-to-open `hangar.db`. Distinct from
/// [`seed_attention_pair`] only in the ASK's third option + raising session
/// (the delivery target); the ids/text match so the answer-flip tripwire reuses
/// the same [`ATTENTION_ASK_ID`] / [`ATTENTION_ASK_QUESTION`] markers.
fn seed_deliverable_ask(home: &Path) {
    use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};

    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("deliverable-ask seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open deliverable-ask store");
        let pool = store.pool();

        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: ATTENTION_WAIT_ID.to_string(),
                session_id: "s-idle".to_string(),
                cwd: "/work/idle".to_string(),
                workspace_id: None,
                kind: AttentionKind::Waiting,
                payload: serde_json::json!({
                    "kind": "WAIT",
                    "context": { "marker": "WAITING: still idle" }
                })
                .to_string(),
                degraded: false,
                created_at: 1_700_000_000_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .expect("seed waiting attention row");

        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: ATTENTION_ASK_ID.to_string(),
                session_id: ATTENTION_ASK_SESSION.to_string(),
                cwd: "/work/deploy".to_string(),
                workspace_id: None,
                kind: AttentionKind::AskUserQuestion,
                payload: serde_json::json!({
                    "kind": "ASK",
                    "context": {
                        "question": ATTENTION_ASK_QUESTION,
                        "options": [
                            { "label": ATTENTION_ASK_OPTION_1 },
                            { "label": ATTENTION_ASK_OPTION_2 },
                            { "label": ATTENTION_ASK_OPTION_3 },
                        ]
                    }
                })
                .to_string(),
                degraded: false,
                created_at: 1_699_999_000_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .expect("seed deliverable ask attention row");
    });
}

/// Write an executable fake `ainb` whose `list --format json` reports one
/// running session (`session_id = s-deploy`) bound to `target_tmux`, so the
/// daemon's `discover_from_ainb` (`<AINB_BIN> list --format json`) returns an
/// exact-id match for the seeded ASK's raising session. Every other invocation
/// prints `[]`. Returns its path.
fn write_fake_ainb(home: &Path, target_tmux: &str) -> PathBuf {
    let path = home.join(".agents-in-a-box").join("fake-ainb.sh");
    let body = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"list\" ]; then\n\
         \tprintf '%s' '[{{\"session_id\":\"{ATTENTION_ASK_SESSION}\",\
         \"tmux_session_name\":\"{target_tmux}\",\"workspace_name\":\"deploy\",\
         \"worktree_path\":\"/work/deploy\",\"created_at\":\"2026-07-01T00:00:00+00:00\",\
         \"is_running\":true,\"claude_active\":true}}]'\n\
         \texit 0\n\
         fi\n\
         printf '%s' '[]'\n"
    );
    std::fs::write(&path, &body).expect("write fake-ainb stub");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat fake-ainb").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake-ainb");
    }
    path
}

/// Read `(state, answer)` for an attention row from the isolated `$HOME`'s db,
/// or `None` if the row is absent / the db can't be opened. The answer-flip
/// tripwire uses this to assert the row deterministically reached `answered`
/// with the picked option — a DB-level belt-and-suspenders alongside the
/// board-flip (`0 need you`) + target-pane-delivery proofs.
#[must_use]
pub fn attention_row_state(home: &Path, id: &str) -> Option<(String, Option<String>)> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("attention-read runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        let row = ainb_hangar_store::repo::attention::AttentionRepo::get(store.pool(), id)
            .await
            .ok()??;
        Some((row.state, row.answer))
    })
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
    let hangar_dir = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.agents-in-a-box");

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

    // Spawn the daemon under the same $HOME (binds $HOME/.agents-in-a-box/hangar.sock).
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
/// `{home}/.agents-in-a-box/hangar/state.toml` (the plugin resolves `$HOME/.agents-in-a-box` when no
/// `$AINB_HANGAR_HOME` is set, as the TUI session is launched).
fn state_toml_path(home: &Path) -> PathBuf {
    home.join(".agents-in-a-box").join("hangar").join("state.toml")
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
/// `{home}/.agents-in-a-box/hangar.db` so the Kanban board (`K`) has a card in every
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
    let hangar_dir = home.join(".agents-in-a-box");
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

/// Write `body` to `dir/name`, chmod `0o755`, and return its path — the shared
/// executable-script writer the fake-provider helpers use.
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write executable script");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
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
    let hangar_dir = home.join(".agents-in-a-box");
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
/// `runtime-1` / `agent-1` / `default` workspace into `{home}/.agents-in-a-box/hangar.db`,
/// so a claim-enabled daemon claims + executes them.
///
/// `created_at` is set to wall-clock "now" (not the fixture's 1970-relative
/// epoch) so the queued-TTL sweeper does not reap them before the claim loop
/// runs. Each task carries no `issue_id` (`NULL`) so the partial-unique pending
/// index never collides. Used by the daemon-health tripwire to drive real
/// completions into the throughput ring.
pub fn enqueue_tasks(home: &Path, prefix: &str, count: usize) {
    let hangar_dir = home.join(".agents-in-a-box");
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
/// `{home}/.agents-in-a-box/hangar.db`.
///
/// The P4 fixture seeds `agent-1` at the schema default cap of **1** and leaves
/// its `task-1` in the `running` state — which fully consumes that single slot,
/// so a claim-enabled daemon would never claim any further queued task. The
/// daemon-health tripwire raises the cap (and/or clears `task-1`) so the seeded
/// queue actually drains, driving the throughput ring.
pub fn set_agent_concurrency(home: &Path, cap: u32) {
    let hangar_dir = home.join(".agents-in-a-box");
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

/// Poll `{home}/.agents-in-a-box/hangar.db` until at least `want` tasks with id prefix
/// `seed-<prefix>-` have reached a terminal status (`done`/`failed`/`cancelled`),
/// or `deadline` passes. Returns the terminal count actually observed.
///
/// The daemon-health tripwire uses this to wait for the claim-enabled daemon to
/// finish executing the seeded tasks (driving the throughput ring) before
/// opening the `D` screen.
pub fn wait_for_terminal(home: &Path, prefix: &str, want: usize, deadline: Instant) -> usize {
    let hangar_dir = home.join(".agents-in-a-box");
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
/// into an already-seeded `{home}/.agents-in-a-box/hangar.db` so the autopilot-manager
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

    let hangar_dir = home.join(".agents-in-a-box");
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
                api_trigger_enabled: false,
            },
        )
        .await
        .expect("seed daily-triage autopilot");
    });
}

/// The ghost runtime's id — a SECOND runtime the daemon does not own, so the
/// presence sweeper decays it instead of heart-beating it every tick.
pub const GHOST_RUNTIME_ID: &str = "rt-ghost";
/// The agent bound to [`GHOST_RUNTIME_ID`]; its Agents-screen row is the one
/// whose availability dot walks online → unstable → offline.
pub const GHOST_AGENT_NAME: &str = "ghostrider";

/// Like [`prepare_pipeline`], plus a SECOND (`rt-ghost`) runtime carrying a
/// fresh heartbeat and one agent (`ghostrider`) bound to it — for the
/// availability-decay tripwire.
///
/// It must not be the daemon's OWN runtime: the daemon beats for that one every
/// tick, so it could never decay. The unique index is
/// `(workspace_id, daemon_id, provider)`, hence the distinct `daemon_id` /
/// `provider`. `presence_sweep_ms` tightens the sweep cadence so several ticks
/// land inside the tripwire's budget.
pub fn prepare_pipeline_ghost_runtime(presence_sweep_ms: &str) -> Pipeline {
    prepare_pipeline_seeded(
        &[
            ("HANGAR_DAEMON_DISABLE_CLAIM", "1"),
            ("HANGAR_PRESENCE_SWEEP_MS", presence_sweep_ms),
        ],
        seed_ghost_runtime,
    )
}

/// Seed the ghost runtime + agent into an already-seeded fixture db.
pub fn seed_ghost_runtime(home: &Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("ghost-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open ghost-seed store");
        sqlx::query(
            "INSERT INTO agent_runtime \
             (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
             VALUES (?, ?, 'daemon-ghost', 'ghost', 'local', ?, 'online')",
        )
        .bind(GHOST_RUNTIME_ID)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind(now)
        .execute(store.pool())
        .await
        .expect("seed ghost runtime");
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES ('ag-ghost', ?, ?, ?, 'workspace', 'user-1')",
        )
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind(GHOST_AGENT_NAME)
        .bind(GHOST_RUNTIME_ID)
        .execute(store.pool())
        .await
        .expect("seed ghost agent");
    });
}

/// Backdate one runtime's heartbeat by `age_ms` relative to now — the sqlite
/// poke the availability tripwire drives the decay with.
pub fn backdate_runtime_heartbeat(home: &Path, runtime_id: &str, age_ms: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("backdate runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open backdate store");
        sqlx::query("UPDATE agent_runtime SET last_seen_at = ? WHERE id = ?")
            .bind(now - age_ms)
            .bind(runtime_id)
            .execute(store.pool())
            .await
            .expect("backdate heartbeat");
    });
}

/// The PERSISTED liveness status of one runtime — proves the sweeper wrote the
/// row, not merely that the snapshot derived a value.
#[must_use]
pub fn runtime_status(home: &Path, runtime_id: &str) -> Option<String> {
    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime-status runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.ok()?;
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_runtime WHERE id = ?")
            .bind(runtime_id)
            .fetch_optional(store.pool())
            .await
            .ok()
            .flatten()
    })
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
/// `{home}/.agents-in-a-box/hangar/logs` by mtime ([`ainb_hangar_core::logs::read_tail`]).
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

    let log_dir = home.join(".agents-in-a-box").join("hangar").join("logs");
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

/// Re-send single-char `key` every ~1.5s until `pred` holds on the pane or
/// `deadline` passes (the first frames after a state change can drop a lone
/// keystroke). Returns the matching capture, or `None` on timeout.
pub fn press_until(
    sess: &TuiSession,
    key: &str,
    deadline: Instant,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    loop {
        sess.send_key(key);
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), &pred) {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Drive the card-create overlay (spec F1-F4) from a fresh Boards screen through
/// title → repo (`@`-pick) → agent, stopping once the assignee-profile picker is
/// on screen. `repo_down` is how many ↓ to move in the `@` dropdown before Enter
/// (0 = `scratch`, always first; 1 = the first roster repo, …). Enter on the agent
/// stage takes the cascade-default chip (claude). Returns the profile-picker
/// capture; panics with the pane on any stage timeout.
///
/// Shared by the headless / interactive card-run tripwires (which pick `scratch`,
/// `repo_down = 0`) and the F5 worktree tripwire (which picks the scanned repo,
/// `repo_down = 1`).
pub fn drive_card_create_to_profile(
    sess: &TuiSession,
    title: &str,
    repo_down: u32,
    scale: u64,
) -> String {
    let secs = |n: u64| Instant::now() + Duration::from_secs(n * scale);

    // `c` opens the title input (re-sent — a lone key can drop after a tab switch).
    press_until(sess, "c", secs(20), |c| c.contains("New card title"))
        .unwrap_or_else(|| panic!("card-title input never opened:\n{}", sess.capture()));
    sess.type_literal(title);
    sess.poll_capture(secs(10), |c| c.contains(title))
        .unwrap_or_else(|| panic!("typed title never echoed:\n{}", sess.capture()));

    // Enter → repo stage (F1 order: title → repo → agent → profile).
    sess.send_enter();
    sess.poll_capture(secs(15), |c| c.contains("Repo for"))
        .unwrap_or_else(|| panic!("repo stage never opened:\n{}", sess.capture()));

    // `@` opens the fuzzy dropdown (scratch always first). Check-then-send so a
    // late-observed open never fires a SECOND `@` (which would push `@` into the
    // fuzzy query and filter the roster repo out).
    let at_deadline = secs(15);
    loop {
        if sess.capture().contains("[scratch]") {
            break;
        }
        sess.send_key("@");
        if sess
            .poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
                c.contains("[scratch]")
            })
            .is_some()
        {
            break;
        }
        if Instant::now() >= at_deadline {
            panic!("`@` dropdown never opened:\n{}", sess.capture());
        }
    }

    // Move to the desired repo, then Enter picks it → agent stage. `Down` re-sent
    // until the highlight leaves `scratch` (its `[scratch]` brackets clear), so a
    // dropped key never leaves the pick on scratch when a roster repo was wanted.
    if repo_down > 0 {
        let move_deadline = secs(10);
        loop {
            if !sess.capture().contains("[scratch]") {
                break;
            }
            sess.send_key("Down");
            std::thread::sleep(Duration::from_millis(250));
            if Instant::now() >= move_deadline {
                panic!(
                    "repo highlight never moved off scratch:\n{}",
                    sess.capture()
                );
            }
        }
        // Any further steps (repo_down > 1) move deeper into the roster.
        for _ in 1..repo_down {
            sess.send_key("Down");
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    sess.send_enter();
    sess.poll_capture(secs(15), |c| c.contains("Agent for"))
        .unwrap_or_else(|| panic!("agent stage never opened:\n{}", sess.capture()));

    // Enter takes the cascade-default agent (claude) → profile stage.
    sess.send_enter();
    sess.poll_capture(secs(15), |c| c.contains("Assignee profile"))
        .unwrap_or_else(|| panic!("profile stage never opened:\n{}", sess.capture()))
}

/// Read the daemon's structured JSONL logs under
/// `<home>/.agents-in-a-box/hangar/logs/` and return the tail of every `daemon.*`
/// file (newest file last, last 200 lines each). The daemon logs at `info` by
/// default (finalize outcomes, `board card auto-moved`, `board auto-move failed`,
/// attention-ingest), so folding this into a FAILING tripwire's panic makes a
/// CI-only failure diagnosable without a local Linux repro. Empty-safe: returns a
/// marker string when the dir/files are absent (the daemon never booted, or a
/// buffered line has not flushed yet).
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
    out
}

/// Print the canonical SKIP line and return — keeps every tripwire's skip path
/// identical and greppable.
pub fn skip(reason: &str) {
    eprintln!(
        "SKIP: {reason} (need tmux + built `ainb`/`ainb-hangar-daemon` + staged hangar-tui plugin; \
         run `just stage-plugins` + `cargo build -p ainb`)"
    );
}
