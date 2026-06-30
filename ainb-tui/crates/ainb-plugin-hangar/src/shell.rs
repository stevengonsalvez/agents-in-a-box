//! Host-shell side effects for the Hangar plugin — opening a URL in the user's
//! browser (P9.2).
//!
//! The task-detail PR badge (`o`) needs to open the captured PR URL in the host
//! browser. That is a real OS side effect (`open` on macOS, `xdg-open` on Linux;
//! Windows is excluded by the Unix-only release matrix per
//! `reference_rust_unix_only_apis`), which makes the action untestable if the
//! handler shells out directly.
//!
//! [`Opener`] is the seam: the plugin holds a `Box<dyn Opener>` and calls
//! [`Opener::open`] for the `o` action. Production uses [`SystemOpener`]; tests
//! inject a [`RecordingOpener`] that writes the URL to a probe file instead of
//! launching a browser, so the tmux tripwire can assert the URL without a real
//! browser popping up.
//!
//! [`default_opener`] picks the impl from the environment: when
//! `$HANGAR_OPENER_PROBE_FILE` is set (the tripwire flips it) the real `ainb tui`
//! binary uses a [`RecordingOpener`] writing to that path; otherwise it uses the
//! real [`SystemOpener`]. So the same compiled binary the tripwire launches
//! records the URL rather than opening a browser, with zero production cost.

use std::io;

/// The env var the tripwire sets to redirect the opener to a probe file instead
/// of launching a real browser.
pub const OPENER_PROBE_ENV: &str = "HANGAR_OPENER_PROBE_FILE";

/// The env var the tripwire / test sets to redirect the daemon-starter to a
/// probe file instead of actually spawning `ainb hangar daemon start` (e38.36).
pub const DAEMON_START_PROBE_ENV: &str = "HANGAR_DAEMON_START_PROBE_FILE";

/// The env var that overrides the resolved `ainb` host binary path (e38.36).
///
/// When unset the daemon-starter resolves a sibling of the plugin's own
/// executable, then the bare `ainb` on `$PATH`.
pub const AINB_BIN_ENV: &str = "AINB_BIN";

/// Bare name of the host binary the plugin shells to start the daemon.
const AINB_BIN_NAME: &str = "ainb";

/// The `ainb` subcommand args that start the daemon (e38.36).
///
/// The literal CLI command the offline empty-state prints for a manual start,
/// and the verb the [`SystemDaemonStarter`] passes to the resolved `ainb`
/// binary. Kept as one source of truth so the on-screen hint and the real spawn
/// never drift.
pub const DAEMON_START_ARGS: [&str; 3] = ["hangar", "daemon", "start"];

/// Opens a URL in the host environment.
///
/// The seam between the pure task-detail screen and the OS browser launch, so the
/// `o` (open-PR) action is testable without popping a real browser.
pub trait Opener: std::fmt::Debug + Send + Sync {
    /// Open `url` (typically in the user's default browser).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] when the underlying launch fails (e.g. the
    /// platform `open`/`xdg-open` command could not be spawned).
    fn open(&self, url: &str) -> io::Result<()>;
}

/// The real opener: launches the platform browser-open command.
///
/// `open` on macOS, `xdg-open` on Linux. Fire-and-forget: it spawns the command
/// and does not wait for the browser to exit. Windows is excluded by the
/// Unix-only release matrix.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, url: &str) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        let cmd = "open";
        #[cfg(target_os = "linux")]
        let cmd = "xdg-open";
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let cmd = {
            // Unix-only per the release matrix; on any other target the open is a
            // typed error rather than a silent no-op.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "opening URLs is only supported on macOS and Linux",
            ));
        };

        std::process::Command::new(cmd)
            .arg(url)
            .spawn()
            .map(|_child| ())
    }
}

/// A test opener that records the opened URL to a file instead of launching a
/// browser.
///
/// Writes `url` to the path it was constructed with. The tmux tripwire points it
/// at a tempfile (via `$HANGAR_OPENER_PROBE_FILE`) and asserts the file contents
/// equal the badged PR URL — proving the `o` action fired, with no real browser.
#[derive(Debug, Clone)]
pub struct RecordingOpener {
    probe_path: std::path::PathBuf,
}

impl RecordingOpener {
    /// A recording opener that writes opened URLs to `probe_path`.
    #[must_use]
    pub fn new(probe_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            probe_path: probe_path.into(),
        }
    }
}

impl Opener for RecordingOpener {
    fn open(&self, url: &str) -> io::Result<()> {
        std::fs::write(&self.probe_path, url)
    }
}

/// The opener the production plugin uses, chosen from the environment.
///
/// When `$HANGAR_OPENER_PROBE_FILE` is set (the tripwire flips it) the binary
/// records opened URLs to that path via a [`RecordingOpener`]; otherwise it uses
/// the real [`SystemOpener`]. This lets the tmux tripwire drive the real
/// `ainb tui` binary's `o` action without a browser popping up.
#[must_use]
pub fn default_opener() -> Box<dyn Opener> {
    match std::env::var_os(OPENER_PROBE_ENV) {
        Some(path) if !path.is_empty() => Box::new(RecordingOpener::new(path)),
        _ => Box::new(SystemOpener),
    }
}

/// Starts the Hangar daemon by shelling the host `ainb hangar daemon start`
/// command (e38.36).
///
/// The seam between the offline empty-state `[s]` action and the OS process
/// launch, so the start path is testable without spawning a real daemon. The
/// plugin holds a `Box<dyn DaemonStarter>` and calls [`DaemonStarter::start`]
/// for `[s]`; production uses [`SystemDaemonStarter`], tests inject a
/// [`RecordingDaemonStarter`] that writes a marker file instead of launching the
/// binary.
pub trait DaemonStarter: std::fmt::Debug + Send + Sync {
    /// Start the Hangar daemon (typically by spawning `ainb hangar daemon
    /// start`).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] when the underlying launch fails (e.g. the
    /// resolved `ainb` binary could not be spawned). The plugin surfaces the
    /// message in the offline empty-state rather than crashing.
    fn start(&self) -> io::Result<()>;
}

/// The real daemon starter: spawns the resolved `ainb` binary with
/// `hangar daemon start` as a fire-and-forget background child.
///
/// The `ainb hangar daemon start` subcommand itself double-forks the daemon and
/// returns immediately, so this only needs to spawn the foreground `ainb`
/// process and not wait on it.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDaemonStarter;

impl DaemonStarter for SystemDaemonStarter {
    fn start(&self) -> io::Result<()> {
        let bin = resolve_ainb_bin();
        std::process::Command::new(bin)
            .args(DAEMON_START_ARGS)
            .spawn()
            .map(|_child| ())
    }
}

/// Resolve the `ainb` host binary the daemon starter shells (e38.36).
///
/// Order, mirroring the daemon's own `resolve_daemon_bin`: the [`AINB_BIN_ENV`]
/// override → a sibling of the plugin's own executable (the normal install
/// layout puts `ainb` next to the bundled plugin binary) → the bare `ainb` name
/// on `$PATH`. The plugin runs as a subprocess of `ainb`, so `current_exe`
/// points at the plugin binary, whose install dir also holds `ainb`.
fn resolve_ainb_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os(AINB_BIN_ENV).filter(|p| !p.is_empty()) {
        return std::path::PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(AINB_BIN_NAME);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from(AINB_BIN_NAME)
}

/// A test daemon starter that records the start request to a file instead of
/// spawning the real `ainb` binary.
///
/// Writes a fixed marker (the `ainb hangar daemon start` command line) to the
/// path it was constructed with. Tests point it at a tempfile and assert the
/// file exists / contains the command — proving the `[s]` action fired, with no
/// real daemon spawned.
#[derive(Debug, Clone)]
pub struct RecordingDaemonStarter {
    probe_path: std::path::PathBuf,
}

impl RecordingDaemonStarter {
    /// A recording starter that writes a start marker to `probe_path`.
    #[must_use]
    pub fn new(probe_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            probe_path: probe_path.into(),
        }
    }
}

impl DaemonStarter for RecordingDaemonStarter {
    fn start(&self) -> io::Result<()> {
        std::fs::write(&self.probe_path, DAEMON_START_ARGS.join(" "))
    }
}

/// A test daemon starter that always fails, to exercise the empty-state error
/// surface without crashing the plugin.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingDaemonStarter;

impl DaemonStarter for FailingDaemonStarter {
    fn start(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "ainb binary not found",
        ))
    }
}

/// The daemon starter the production plugin uses, chosen from the environment.
///
/// When `$HANGAR_DAEMON_START_PROBE_FILE` is set (a tripwire flips it) the
/// binary records the start request to that path via a [`RecordingDaemonStarter`]
/// instead of spawning a real daemon; otherwise it uses the real
/// [`SystemDaemonStarter`]. Mirrors [`default_opener`].
#[must_use]
pub fn default_daemon_starter() -> Box<dyn DaemonStarter> {
    match std::env::var_os(DAEMON_START_PROBE_ENV) {
        Some(path) if !path.is_empty() => Box::new(RecordingDaemonStarter::new(path)),
        _ => Box::new(SystemDaemonStarter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording opener writes the URL verbatim to its probe file.
    #[test]
    fn recording_opener_writes_url_to_probe_file() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.txt");
        let opener = RecordingOpener::new(&probe);
        opener
            .open("https://example.com/pr/1")
            .expect("record open");
        let written = std::fs::read_to_string(&probe).expect("read probe");
        assert_eq!(written, "https://example.com/pr/1");
    }

    /// With the probe env unset, `default_opener` falls back to the real
    /// [`SystemOpener`] (it does not record). We don't *set* the env here —
    /// the plugin forbids `unsafe`, and `std::env::set_var` is `unsafe` on the
    /// current edition; the env-driven recording path is exercised end-to-end by
    /// the `tripwire_pr_badge` daemon tripwire, which sets the var for the real
    /// `ainb tui` binary. Here we only pin the no-env default shape.
    #[test]
    fn default_opener_without_probe_env_is_system_opener() {
        // The test harness does not set HANGAR_OPENER_PROBE_FILE, so the default
        // opener is the real system opener (debug-formats as `SystemOpener`).
        if std::env::var_os(OPENER_PROBE_ENV).is_none() {
            let opener = default_opener();
            assert_eq!(format!("{opener:?}"), "SystemOpener");
        }
    }

    /// The recording daemon starter writes the start command line to its probe
    /// file (e38.36) — the testable seam the `[s]` action drives.
    #[test]
    fn recording_daemon_starter_writes_start_marker() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("started.txt");
        let starter = RecordingDaemonStarter::new(&probe);
        starter.start().expect("record start");
        let written = std::fs::read_to_string(&probe).expect("read probe");
        assert_eq!(written, "hangar daemon start");
    }

    /// The failing starter surfaces a typed error rather than panicking, so the
    /// plugin can show it in the empty-state (e38.36).
    #[test]
    fn failing_daemon_starter_returns_error() {
        let err = FailingDaemonStarter.start().expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    /// With the probe env unset, `default_daemon_starter` falls back to the real
    /// [`SystemDaemonStarter`] (debug-formats as `SystemDaemonStarter`).
    #[test]
    fn default_daemon_starter_without_probe_env_is_system_starter() {
        if std::env::var_os(DAEMON_START_PROBE_ENV).is_none() {
            let starter = default_daemon_starter();
            assert_eq!(format!("{starter:?}"), "SystemDaemonStarter");
        }
    }

    /// The on-screen literal command and the spawned argv share one source of
    /// truth — `ainb` + the joined args spells the documented manual command.
    #[test]
    fn daemon_start_args_match_documented_command() {
        assert_eq!(
            format!("{AINB_BIN_NAME} {}", DAEMON_START_ARGS.join(" ")),
            "ainb hangar daemon start"
        );
    }
}
