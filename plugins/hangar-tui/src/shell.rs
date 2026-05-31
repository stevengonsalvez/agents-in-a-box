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

        std::process::Command::new(cmd).arg(url).spawn().map(|_child| ())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording opener writes the URL verbatim to its probe file.
    #[test]
    fn recording_opener_writes_url_to_probe_file() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.txt");
        let opener = RecordingOpener::new(&probe);
        opener.open("https://example.com/pr/1").expect("record open");
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
}
