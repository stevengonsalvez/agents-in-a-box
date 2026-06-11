//! Structured-log line model + reader (P8.6).
//!
//! Shared by the `ainb hangar logs tail` CLI verb and the TUI `LogsScreen`.
//!
//! The daemon's P8.1 observability sink writes one self-describing JSON object
//! per event to a **daily-rotated** appender at `<log_dir>/daemon.<date>`
//! (`tracing_appender::rolling` with `Rotation::DAILY`, prefix `daemon`). The
//! files are therefore `daemon.2026-05-31` etc. — **never** a literal
//! `daemon.jsonl`. Both logs surfaces glob the `daemon.*` prefix and read the
//! newest by modification time.
//!
//! The wire shape (per the P8.1 `tracing_subscriber::fmt().json()` formatter):
//!
//! ```json
//! {"timestamp":"2026-05-31T12:00:00.123456Z","level":"INFO",
//!  "target":"ainb_hangar_daemon","fields":{"message":"boot","task_id":"t-1"}}
//! ```
//!
//! `timestamp`/`level`/`target` sit at the top; the event message and every
//! custom field nest under `fields`. This module is **pure + IO-tolerant**: a
//! line missing any field parses into a [`LogLine`] with that part blank rather
//! than panicking (the pretty-printer must never crash on a partial event, per
//! the P8.6 acceptance criterion), and a non-JSON line is skipped.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

/// The rolling appender's filename prefix. The daily rotation appends a
/// `.<YYYY-MM-DD>` suffix, so files are `daemon.<date>`; both logs surfaces
/// glob this prefix rather than guessing the date.
pub const LOG_FILE_PREFIX: &str = "daemon";

/// The env var naming the Hangar home directory (mirrors
/// `ainb_hangar_store::Store::home_env` / `ainb_hangar_daemon::hangar_dir`).
const HANGAR_HOME_ENV: &str = "AINB_HANGAR_HOME";

/// Resolve the daemon's structured-log directory: `<hangar_home>/hangar/logs`.
///
/// Mirrors `ainb_hangar_daemon::log_dir` without taking a dependency on the
/// daemon crate (the plugin can't): `$AINB_HANGAR_HOME` when set + non-empty,
/// else `~/.ainb`, then `/hangar/logs`. Returns `None` only when neither the
/// env var nor a home directory can be resolved.
///
/// Both the `ainb hangar logs tail` CLI and the TUI `LogsScreen` resolve the
/// log dir through this so they always read the same files the daemon writes.
#[must_use]
pub fn default_log_dir() -> Option<PathBuf> {
    let home = match std::env::var_os(HANGAR_HOME_ENV).filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir()?.join(".ainb"),
    };
    Some(home.join("hangar").join("logs"))
}

/// A single structured-log event, flattened from one JSON line.
///
/// Every field is owned + always present (blank when the source line omitted
/// it), so downstream renderers never branch on `Option`: a partial event still
/// formats cleanly. `fields` keeps the non-`message` event fields as ordered
/// `(key, value)` pairs for the `k=v` tail, with `message` lifted out into its
/// own field so it can lead the rendered line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogLine {
    /// The RFC3339 timestamp string exactly as the appender wrote it. Blank
    /// when absent.
    pub timestamp: String,
    /// The level token (`INFO`, `WARN`, `ERROR`, `DEBUG`, `TRACE`). Blank when
    /// absent.
    pub level: String,
    /// The event target (module path). Blank when absent.
    pub target: String,
    /// The event message (`fields.message`). Blank when absent.
    pub message: String,
    /// The remaining `fields.*` entries (everything but `message`), in stable
    /// order, rendered as the `k=v` tail. Values are stringified scalars.
    pub fields: Vec<(String, String)>,
}

/// A parsed log level for filtering + colouring.
///
/// Ordered most-verbose (`Trace`) to least (`Error`) so a `--level` floor can
/// be expressed as "this severity or worse" via the numeric rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// `TRACE` — most verbose.
    Trace,
    /// `DEBUG`.
    Debug,
    /// `INFO`.
    Info,
    /// `WARN`.
    Warn,
    /// `ERROR` — least verbose / highest severity.
    Error,
}

impl LogLevel {
    /// Parse a level token (case-insensitive). Returns `None` for an
    /// unrecognised token so a malformed `level` neither matches a filter nor
    /// crashes the reader.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "TRACE" => Some(Self::Trace),
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" | "WARNING" => Some(Self::Warn),
            "ERROR" | "ERR" => Some(Self::Error),
            _ => None,
        }
    }

    /// The canonical uppercase token (`INFO`, `WARN`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

impl LogLine {
    /// Parse one JSON line into a [`LogLine`], tolerating missing fields.
    ///
    /// Returns `None` only when the line is not a JSON object at all (blank
    /// lines, partial flushes); a JSON object missing `level`/`target`/
    /// `timestamp`/`fields` yields a `LogLine` with those parts blank, never an
    /// error. A `fields.message` is lifted into [`LogLine::message`]; the
    /// remaining `fields.*` scalars become the ordered `(key, value)` tail.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        let obj = v.as_object()?;

        let timestamp = obj.get("timestamp").and_then(scalar_to_string).unwrap_or_default();
        let level = obj
            .get("level")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let target = obj
            .get("target")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut message = String::new();
        let mut fields = Vec::new();
        if let Some(field_obj) = obj.get("fields").and_then(serde_json::Value::as_object) {
            for (k, val) in field_obj {
                if k == "message" {
                    message = scalar_to_string(val).unwrap_or_default();
                } else if let Some(s) = scalar_to_string(val) {
                    fields.push((k.clone(), s));
                }
            }
        }

        Some(Self {
            timestamp,
            level,
            target,
            message,
            fields,
        })
    }

    /// The parsed [`LogLevel`], if the level token is recognised.
    #[must_use]
    pub fn level_enum(&self) -> Option<LogLevel> {
        LogLevel::parse(&self.level)
    }

    /// Whether this line passes a `--level` floor: kept when its severity is at
    /// or above `min`. A line whose level token is unrecognised is **dropped**
    /// by a filter (it can't be proven to meet the floor) but always kept when
    /// `min` is `None`.
    #[must_use]
    pub fn passes_level(&self, min: Option<LogLevel>) -> bool {
        min.is_none_or(|min| self.level_enum().is_some_and(|l| l >= min))
    }
}

/// Stringify a JSON scalar for the `k=v` tail. Strings drop their quotes;
/// numbers / bools render literally; `null` and composite values (arrays /
/// objects) are skipped (return `None`) so the tail stays scalar.
fn scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

/// Find every `daemon.*` log file in `dir`, newest (by mtime) first.
///
/// Returns an empty vec when `dir` is missing or holds no matching file — a
/// fresh install with no daemon run yet is not an error. Files whose mtime
/// can't be read sort last (treated as oldest) rather than panicking.
#[must_use]
pub fn log_files_newest_first(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(LOG_FILE_PREFIX))
        })
        .map(|p| {
            let mtime = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (p, mtime)
        })
        .collect();
    files.sort_by_key(|(_, mtime)| Reverse(*mtime));
    files.into_iter().map(|(p, _)| p).collect()
}

/// Read + parse the last `limit` log lines from the newest `daemon.*` file in
/// `dir`, applying an optional `--level` floor.
///
/// Reads the single newest file (daily rotation means "tail" is naturally the
/// current day's file). A missing dir / no matching file / unreadable file all
/// yield an empty vec rather than an error — the surfaces show an empty pane.
/// Lines are returned oldest-first (chronological) so the tail reads top-down.
#[must_use]
pub fn read_tail(dir: &Path, limit: usize, min_level: Option<LogLevel>) -> Vec<LogLine> {
    let Some(newest) = log_files_newest_first(dir).into_iter().next() else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(&newest) else {
        return Vec::new();
    };
    let mut parsed: Vec<LogLine> = contents
        .lines()
        .filter_map(LogLine::parse)
        .filter(|l| l.passes_level(min_level))
        .collect();
    if parsed.len() > limit {
        parsed.drain(0..parsed.len() - limit);
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"{"timestamp":"2026-05-31T12:00:00.000001Z","level":"INFO","target":"ainb_hangar_daemon","fields":{"message":"ready","task_id":"t-1"}}"#;
    const WARN: &str = r#"{"timestamp":"2026-05-31T12:00:01Z","level":"WARN","target":"x::y","fields":{"message":"retry","attempts":2}}"#;
    const ERROR: &str = r#"{"timestamp":"2026-05-31T12:00:02Z","level":"ERROR","target":"x::z","fields":{"message":"boom","code":7}}"#;

    #[test]
    fn parse_lifts_message_and_keeps_scalar_field_tail() {
        let l = LogLine::parse(INFO).expect("valid line");
        assert_eq!(l.level, "INFO");
        assert_eq!(l.target, "ainb_hangar_daemon");
        assert_eq!(l.message, "ready");
        assert_eq!(l.fields, vec![("task_id".to_string(), "t-1".to_string())]);
        assert_eq!(l.level_enum(), Some(LogLevel::Info));
    }

    #[test]
    fn parse_tolerates_missing_fields_and_skips_non_json() {
        // Missing `fields`, `target`, `timestamp` — blank, not an error.
        let partial = LogLine::parse(r#"{"level":"WARN"}"#).expect("object parses");
        assert_eq!(partial.level, "WARN");
        assert!(partial.message.is_empty());
        assert!(partial.target.is_empty());
        assert!(partial.fields.is_empty());
        // A non-JSON / blank line is skipped.
        assert!(LogLine::parse("not json").is_none());
        assert!(LogLine::parse("").is_none());
    }

    #[test]
    fn parse_numeric_field_renders_without_quotes() {
        let l = LogLine::parse(WARN).expect("valid");
        assert_eq!(l.fields, vec![("attempts".to_string(), "2".to_string())]);
    }

    #[test]
    fn passes_level_floor_filters_correctly() {
        let info = LogLine::parse(INFO).unwrap();
        let err = LogLine::parse(ERROR).unwrap();
        assert!(info.passes_level(None));
        assert!(info.passes_level(Some(LogLevel::Info)));
        assert!(!info.passes_level(Some(LogLevel::Warn)));
        assert!(err.passes_level(Some(LogLevel::Warn)));
        // An unrecognised level is dropped by any floor, kept when None.
        let weird = LogLine::parse(r#"{"level":"BOGUS"}"#).unwrap();
        assert!(weird.passes_level(None));
        assert!(!weird.passes_level(Some(LogLevel::Trace)));
    }

    #[test]
    fn log_files_newest_first_globs_prefix_and_orders_by_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("daemon.2026-05-30");
        let newer = dir.path().join("daemon.2026-05-31");
        let ignored = dir.path().join("claude.jsonl");
        std::fs::write(&older, "old\n").unwrap();
        std::fs::write(&ignored, "x\n").unwrap();
        // Make `newer` strictly newer by mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&newer, "new\n").unwrap();

        let files = log_files_newest_first(dir.path());
        assert_eq!(files.len(), 2, "only daemon.* files: {files:?}");
        assert_eq!(files[0], newer, "newest first");
        assert_eq!(files[1], older);
    }

    #[test]
    fn log_files_newest_first_missing_dir_is_empty() {
        let missing = std::path::Path::new("/nonexistent/hangar/logs/xyz");
        assert!(log_files_newest_first(missing).is_empty());
    }

    #[test]
    fn read_tail_bounds_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("daemon.2026-05-31");
        std::fs::write(&file, format!("{INFO}\n{WARN}\n{ERROR}\n")).unwrap();

        // All three, oldest-first.
        let all = read_tail(dir.path(), 10, None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].message, "ready");
        assert_eq!(all[2].message, "boom");

        // Bounded to the last 1.
        let last = read_tail(dir.path(), 1, None);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].message, "boom");

        // Filtered at WARN floor: INFO dropped.
        let warn = read_tail(dir.path(), 10, Some(LogLevel::Warn));
        assert_eq!(warn.len(), 2);
        assert!(warn.iter().all(|l| l.message != "ready"));
    }

    #[test]
    fn read_tail_missing_dir_is_empty() {
        let missing = std::path::Path::new("/nonexistent/hangar/logs/xyz");
        assert!(read_tail(missing, 10, None).is_empty());
    }
}
