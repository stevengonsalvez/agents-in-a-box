// ABOUTME: Plugin-side reader for the `[notifyd]` table of the host config.

//! Plugin-side config: the `[notifyd]` table of
//! `~/.agents-in-a-box/config/config.toml`.
//!
//! Read-only, and read the same way `ainb-plugin-session-reader` reads its own
//! table: the host owns the file, `PluginInitParams` carries no config channel,
//! and every failure degrades to the coded defaults. A malformed config must
//! never stop notifications, it may only fail to tune them.
//!
//! ```toml
//! [notifyd]
//! os_debounce_secs = 30
//! approval_timeout_secs = 900
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Coded default OS-notification debounce, in seconds.
pub const DEFAULT_OS_DEBOUNCE_SECS: u64 = 60;

/// Coded default AWAIT ceiling for a permission request, in seconds.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 600;

/// The ceiling on `approval_timeout_secs`.
///
/// The broker sits at the BOTTOM of a timeout ladder: broker AWAIT < the
/// client's re-dial deadline (640s) < Claude's registered `PermissionRequest`
/// hook timeout (660s). A configured value above the client deadline means the
/// hook is hard-killed before the broker ever answers, which turns a
/// deliberate deny into a silent hang, so it is clamped rather than trusted.
pub const MAX_APPROVAL_TIMEOUT_SECS: u64 = 630;

/// Parsed `[notifyd]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotifydConfig {
    /// Per-`(session, event)` debounce window for OS notifications, in seconds.
    pub os_debounce_secs: u64,
    /// Seconds an unanswered permission request waits before it is auto-denied.
    pub approval_timeout_secs: u64,
}

impl Default for NotifydConfig {
    fn default() -> Self {
        Self {
            os_debounce_secs: DEFAULT_OS_DEBOUNCE_SECS,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
        }
    }
}

impl NotifydConfig {
    /// The effective debounce window.
    #[must_use]
    pub fn os_debounce(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.os_debounce_secs)
    }

    /// The effective AWAIT ceiling, clamped to [1, [`MAX_APPROVAL_TIMEOUT_SECS`]].
    ///
    /// `0` would auto-deny every request the instant it arrived; anything above
    /// the ceiling breaks the timeout ladder (see [`MAX_APPROVAL_TIMEOUT_SECS`]).
    #[must_use]
    pub fn approval_timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.approval_timeout_secs.clamp(1, MAX_APPROVAL_TIMEOUT_SECS),
        )
    }
}

/// Load from the default host config path. Missing file, unreadable file,
/// unparseable TOML, or a malformed `[notifyd]` table all degrade to
/// [`NotifydConfig::default`].
#[must_use]
pub fn load() -> NotifydConfig {
    match default_config_path() {
        Some(path) => load_from(&path),
        None => NotifydConfig::default(),
    }
}

/// Load from an explicit path (testable entry point).
#[must_use]
pub fn load_from(path: &Path) -> NotifydConfig {
    let Ok(content) = std::fs::read_to_string(path) else {
        return NotifydConfig::default();
    };
    let root: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("notifyd: config parse failed ({err}); using defaults");
            return NotifydConfig::default();
        }
    };
    match root.get("notifyd").cloned() {
        Some(table) => table.try_into().unwrap_or_else(|err| {
            tracing::warn!("notifyd: [notifyd] table malformed ({err}); using defaults");
            NotifydConfig::default()
        }),
        None => NotifydConfig::default(),
    }
}

/// `~/.agents-in-a-box/config/config.toml`, resolved via `$HOME`.
fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".agents-in-a-box").join("config").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_config(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, body).expect("write config");
        path
    }

    #[test]
    fn present_values_are_read() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[notifyd]\nos_debounce_secs = 15\napproval_timeout_secs = 120\n",
        );
        let cfg = load_from(&path);
        assert_eq!(cfg.os_debounce().as_secs(), 15);
        assert_eq!(cfg.approval_timeout().as_secs(), 120);
    }

    #[test]
    fn a_partial_table_keeps_the_other_default() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[notifyd]\nos_debounce_secs = 5\n");
        let cfg = load_from(&path);
        assert_eq!(cfg.os_debounce().as_secs(), 5);
        assert_eq!(
            cfg.approval_timeout().as_secs(),
            DEFAULT_APPROVAL_TIMEOUT_SECS
        );
    }

    #[test]
    fn missing_file_defaults() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            load_from(&dir.path().join("nope.toml")),
            NotifydConfig::default()
        );
    }

    #[test]
    fn a_broken_table_defaults_rather_than_failing() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[notifyd]\nos_debounce_secs = \"soon\"\n");
        assert_eq!(load_from(&path), NotifydConfig::default());
    }

    /// The AWAIT ceiling has to stay under the client's re-dial deadline, or a
    /// configured value silently turns a deny into a hung hook.
    #[test]
    fn an_absurd_approval_timeout_is_clamped_to_the_ladder() {
        let cfg = NotifydConfig {
            os_debounce_secs: 60,
            approval_timeout_secs: 86_400,
        };
        assert_eq!(cfg.approval_timeout().as_secs(), MAX_APPROVAL_TIMEOUT_SECS);

        let instant = NotifydConfig {
            os_debounce_secs: 60,
            approval_timeout_secs: 0,
        };
        assert_eq!(instant.approval_timeout().as_secs(), 1);
    }
}
