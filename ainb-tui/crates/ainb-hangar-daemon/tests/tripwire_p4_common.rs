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

#![allow(dead_code)] // each tripwire uses a subset of these helpers.
// `Duration::from_secs(60)` reads fine as a poll budget; `from_mins` is unstable.
// Same rationale as `run_loop.rs`'s crate-level allow.
#![allow(clippy::duration_suboptimal_units)]
// Test-helper rustdoc: prose-heavy module/fn docs (multi-sentence first
// paragraphs, plain words that aren't code items) are intentional here.
#![allow(clippy::too_long_first_doc_paragraph, clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The expected on-screen marker proving the seeded hangar TUI actually rendered
/// (the issue-list landing screen shows the seeded `Refactor API` issue).
pub const READY_MARKER: &str = "Refactor API";

/// `true` when the `tmux` binary is usable.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
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
    plugin_root().map(|r| r.join("hangar-tui").join("hangar-tui"))
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

    // Spawn the daemon under the same $HOME (binds $HOME/.ainb/hangar.sock).
    let bin = daemon_bin().expect("gated by can_run_tripwire");
    let daemon = Command::new(bin)
        .env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1") // serve RPC only; don't run claude.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ainb-hangar-daemon");

    // Wait for the socket to appear (the daemon binds it during boot).
    let socket = hangar_dir.join("hangar.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    Pipeline { home, daemon }
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
    let root_cargo = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("Cargo.toml"));
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
        let store = ainb_hangar_store::Store::open_in(hangar_dir)
            .await
            .expect("open seed store");
        ainb_hangar_daemon::seed::seed_p4_fixture(store.pool())
            .await
            .expect("seed P4 fixture");
    });
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

        let cmd = format!(
            "HOME='{}' {plugin_root}exec '{}' tui",
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

    /// Poll the pane until `pred` holds or `deadline` passes, returning the
    /// matching capture. No bare sleep: a 200ms inter-poll gap, deadline-bounded.
    pub fn poll_capture(
        &self,
        deadline: Instant,
        pred: impl Fn(&str) -> bool,
    ) -> Option<String> {
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
        // 1. Wait for the home screen to be *interactive* — the home footer hint
        //    (`Enter select | Tab content | ↑↓ navigate`) only paints once the
        //    home screen owns the keyboard. Matching on a sidebar label alone
        //    races the initial tmux/session discovery and drops the `g` keystroke.
        let home = self.poll_capture(Instant::now() + Duration::from_secs(45), |c| {
            c.contains("Tab content") && c.contains("navigate")
        })?;
        // Negative: we are NOT already on the hangar issue list before pressing g.
        assert!(!home.contains(READY_MARKER), "issue list rendered before `g`:\n{home}");

        // 2. Open the Hangar plugin screen (single-char nav, no Enter). The home
        //    screen's first frames race the initial tmux/session discovery, so a
        //    single `g` can be dropped. Re-send `g` every ~1.5s until the Hangar
        //    plugin chrome (its tab strip) replaces the host home screen, bounded
        //    by a deadline — then wait for the snapshot rows to fill in.
        let deadline = Instant::now() + Duration::from_secs(60);
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
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .status();
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
