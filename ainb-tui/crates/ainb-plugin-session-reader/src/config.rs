//! Plugin-side config: the `[session_reader]` table of
//! `~/.agents-in-a-box/config.toml`.
//!
//! Read-only — the host owns the file; this plugin only consumes its
//! own table (mirroring the burndown plugin's disk-read pattern, since
//! `PluginInitParams` carries no config channel). Any read or parse
//! failure falls back to defaults: config must never break a scan.
//!
//! ```toml
//! [session_reader]
//! incremental_window_days = 30
//! ```

// Consumed by the incremental scan path (`scan_incremental`, P4) and
// cold-start seeding (P8) on this branch; until those land the module
// is exercised only by its tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default trailing window (days) for the incremental refresh: files
/// whose mtime is older than `now - window` are served from the
/// persisted stable aggregate instead of being re-aggregated.
pub const DEFAULT_INCREMENTAL_WINDOW_DAYS: u32 = 30;

/// Parsed `[session_reader]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SessionReaderConfig {
    /// Trailing recent-window size in days. `0` is clamped to `1` so a
    /// misconfigured zero window cannot disable recency entirely.
    pub incremental_window_days: u32,
}

impl Default for SessionReaderConfig {
    fn default() -> Self {
        Self {
            incremental_window_days: DEFAULT_INCREMENTAL_WINDOW_DAYS,
        }
    }
}

impl SessionReaderConfig {
    /// The effective window, clamped to at least one day.
    #[must_use]
    pub fn window_days(self) -> u32 {
        self.incremental_window_days.max(1)
    }
}

/// Load from the default host config path. Missing file, unreadable
/// file, unparseable TOML, or a malformed `[session_reader]` table all
/// degrade to [`SessionReaderConfig::default`] (with a warn log).
#[must_use]
pub fn load() -> SessionReaderConfig {
    match default_config_path() {
        Some(path) => load_from(&path),
        None => SessionReaderConfig::default(),
    }
}

/// Load from an explicit path (testable entry point).
#[must_use]
pub fn load_from(path: &Path) -> SessionReaderConfig {
    let Ok(content) = std::fs::read_to_string(path) else {
        return SessionReaderConfig::default();
    };
    let root: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("session-reader: config parse failed ({err}); using defaults");
            return SessionReaderConfig::default();
        }
    };
    match root.get("session_reader").cloned() {
        Some(table) => table.try_into().unwrap_or_else(|err| {
            tracing::warn!(
                "session-reader: [session_reader] table malformed ({err}); using defaults"
            );
            SessionReaderConfig::default()
        }),
        None => SessionReaderConfig::default(),
    }
}

/// `~/.agents-in-a-box/config.toml`, resolved via `$HOME`.
fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".agents-in-a-box").join("config.toml"))
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
    fn present_value_is_read() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[session_reader]\nincremental_window_days = 7\n");
        assert_eq!(load_from(&path).incremental_window_days, 7);
    }

    #[test]
    fn missing_file_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = load_from(&dir.path().join("nope.toml"));
        assert_eq!(cfg, SessionReaderConfig::default());
        assert_eq!(cfg.incremental_window_days, DEFAULT_INCREMENTAL_WINDOW_DAYS);
    }

    #[test]
    fn missing_table_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[usage]\nplan = \"max\"\n");
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn garbage_toml_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "not toml at {{{ all");
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn malformed_table_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[session_reader]\nincremental_window_days = \"soon\"\n",
        );
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[session_reader]\nincremental_window_days = 14\nfuture_knob = true\n",
        );
        assert_eq!(load_from(&path).incremental_window_days, 14);
    }

    #[test]
    fn zero_window_clamps_to_one_day() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[session_reader]\nincremental_window_days = 0\n");
        assert_eq!(load_from(&path).window_days(), 1);
    }
}
