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
//! ## Why these SKIP rather than assert today
//!
//! The end-to-end stack these tripwires drive — `ainb tui` discovering and
//! spawning the `ainb-plugin-hangar` subprocess, that plugin rendering the Core 5
//! screens from a **seeded daemon** over the `hangar/*` snapshot RPCs — is not yet
//! standable at P4: the plugin's `render` paints only chrome (the per-screen
//! renderers exist but the host render-data pipeline + the daemon seed RPCs land
//! in P5). Until then [`hangar_tui_ready`] returns `false` and each tripwire
//! prints `SKIP` and returns, per the skill's rule "if the stack is unavailable
//! the test must SKIP gracefully — don't claim a pass". Once P5 wires the
//! pipeline, flip [`hangar_tui_ready`] to the real readiness probe and the
//! assertions below become live with zero structural change.

#![allow(dead_code)] // each tripwire uses a subset of these helpers.

use std::path::PathBuf;
use std::process::Command;
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

/// Best-effort locate the built `ainb` binary the tripwire would drive.
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
    // tests run from the crate dir; the workspace target is two levels up.
    let exe = std::env::current_exe().ok()?;
    // .../target/<profile>/deps/<test-bin> → .../target/<profile>/ainb
    let profile_dir = exe.parent()?.parent()?;
    let candidate = profile_dir.join("ainb");
    candidate.exists().then_some(candidate)
}

/// Whether the seeded TUI render pipeline is standable.
///
/// The full `ainb tui` → hangar-plugin → daemon render path is **currently not**
/// standable (see the module note): the host render-data pipeline + the daemon
/// seed RPCs land in P5. Flip this to a real probe there.
pub fn hangar_tui_ready() -> bool {
    // Gate on an explicit opt-in env so a future P5 CI lane can enable these
    // without editing the test, while today they skip everywhere.
    std::env::var("AINB_HANGAR_TUI_E2E").is_ok() && ainb_bin().is_some()
}

/// The combined precondition: tmux present, binary locatable, pipeline ready.
/// Returns `false` (and the caller SKIPs) when any is missing.
pub fn can_run_tripwire() -> bool {
    tmux_available() && ainb_bin().is_some() && hangar_tui_ready()
}

/// A uniquely-named tmux session that kills itself by **exact name** on drop.
pub struct TuiSession {
    name: String,
}

impl TuiSession {
    /// Spawn `ainb tui` in a detached tmux session under an isolated `$HOME`,
    /// seeded so the onboarding wizard is skipped and the hangar daemon is seeded
    /// with the P4 fixture. Panics only on a tmux spawn failure (the caller has
    /// already gated on [`can_run_tripwire`]).
    pub fn spawn(bin: &std::path::Path, home: &std::path::Path) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("hangar-p4-trip-{}-{nanos}", std::process::id());

        // 80×24 floor (the phase's minimum-size contract).
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", "180", "-y", "50"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed for {name}");

        // Determinism rails: pin the clock + id seed so seeded task ids are stable.
        let cmd = format!(
            "HOME='{}' HANGAR_NOW=1700000000 HANGAR_ID_SEED=p4 exec '{}' tui",
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

    /// Wait up to 45s for the seeded hangar issue-list landing screen.
    pub fn wait_ready(&self) -> Option<String> {
        self.poll_capture(Instant::now() + Duration::from_secs(45), |c| {
            c.contains(READY_MARKER)
        })
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

/// Seed an isolated `$HOME` so `ainb tui` skips onboarding and the hangar daemon
/// boots against the P4 seed fixture. Returns the tempdir guard (delete on drop).
///
/// The concrete seed wiring (onboarding skip toml + daemon DB seed) lands with
/// the P5 render pipeline; this stub creates the dir so the helper signature is
/// stable now and the tripwires compile.
pub fn seed_isolated_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("isolated HOME tempdir")
}

/// Print the canonical SKIP line and return — keeps every tripwire's skip path
/// identical and greppable.
pub fn skip(reason: &str) {
    eprintln!("SKIP: {reason} (hangar TUI e2e pipeline lands in P5; see tripwire_p4_common.rs)");
}
