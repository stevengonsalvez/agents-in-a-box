// ABOUTME: JSONL log reader for loading historical session logs from disk
// Reads log entries in JSON Lines format for efficient streaming and parsing
// Supports both tracing-subscriber JSON format and custom JSONL format

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

use super::live_logs_stream::{LogEntry, LogEntryLevel};
use super::log_writer::JsonlLogEntry;

/// tracing-subscriber JSON format entry
#[derive(Debug, Clone, Deserialize)]
pub struct TracingJsonEntry {
    pub timestamp: String,
    pub level: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub fields: TracingFields,
    #[serde(default)]
    pub span: Option<TracingSpan>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TracingFields {
    #[serde(default)]
    pub message: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TracingSpan {
    pub name: String,
}

impl TracingJsonEntry {
    /// Convert to LogEntry for display
    pub fn to_log_entry(&self) -> LogEntry {
        let level = match self.level.to_uppercase().as_str() {
            "ERROR" => LogEntryLevel::Error,
            "WARN" => LogEntryLevel::Warn,
            "DEBUG" | "TRACE" => LogEntryLevel::Debug,
            _ => LogEntryLevel::Info,
        };

        // Parse timestamp
        let timestamp = DateTime::parse_from_rfc3339(&self.timestamp)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        LogEntry {
            timestamp,
            level,
            source: self.target.clone(),
            message: self.fields.message.clone(),
            session_id: None,
            parsed_data: None,
            metadata: HashMap::new(),
        }
    }
}

/// Info about an application log file
#[derive(Debug, Clone)]
pub struct AppLogInfo {
    /// Filename (e.g., "agents-in-a-box-20260107-001310.jsonl")
    pub filename: String,
    /// Display name (e.g., "2026-01-07 00:13:10")
    pub display_name: String,
    /// Full path to the log file
    pub log_path: PathBuf,
    /// Number of log entries (estimated or actual)
    pub log_count: usize,
    /// Count of error-level logs
    pub error_count: usize,
    /// Count of warning-level logs
    pub warn_count: usize,
    /// File size in bytes
    pub file_size: u64,
    /// Last modified time
    pub modified: SystemTime,
    /// Whether this is a JSONL file (true) or plain text (false)
    pub is_jsonl: bool,
}

/// Summary of a session's logs for display in the viewer
#[derive(Debug, Clone)]
pub struct SessionLogInfo {
    /// Session UUID
    pub session_id: Uuid,
    /// Path to the log file
    pub log_path: PathBuf,
    /// Number of log entries
    pub log_count: usize,
    /// First log timestamp
    pub first_timestamp: Option<DateTime<Utc>>,
    /// Last log timestamp
    pub last_timestamp: Option<DateTime<Utc>>,
    /// Count of error-level logs
    pub error_count: usize,
    /// Count of warning-level logs
    pub warn_count: usize,
    /// File size in bytes
    pub file_size: u64,
}

/// JSONL log reader for loading historical logs
pub struct JsonlLogReader;

impl JsonlLogReader {
    /// Read all logs from a session log file
    pub fn read_session_logs(path: &Path) -> Result<Vec<LogEntry>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open log file: {:?}", path))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result
                .with_context(|| format!("Failed to read line {} from {:?}", line_num + 1, path))?;

            // Skip empty lines
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonlLogEntry>(&line) {
                Ok(jsonl_entry) => {
                    entries.push(jsonl_entry.to_log_entry());
                }
                Err(e) => {
                    // Log parse errors but continue reading
                    tracing::debug!(
                        "Failed to parse log entry at line {}: {} - {:?}",
                        line_num + 1,
                        e,
                        path
                    );
                }
            }
        }

        Ok(entries)
    }

    /// Read logs from a session log file with a limit
    pub fn read_session_logs_limited(path: &Path, limit: usize) -> Result<Vec<LogEntry>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open log file: {:?}", path))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line_result in reader.lines() {
            if entries.len() >= limit {
                break;
            }

            let line = line_result?;
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(jsonl_entry) = serde_json::from_str::<JsonlLogEntry>(&line) {
                entries.push(jsonl_entry.to_log_entry());
            }
        }

        Ok(entries)
    }

    /// Read the last N logs from a session (reads file in reverse efficiently)
    pub fn read_session_logs_tail(path: &Path, count: usize) -> Result<Vec<LogEntry>> {
        // For simplicity, read all and take last N
        // TODO: Implement efficient tail reading for large files
        let all_logs = Self::read_session_logs(path)?;
        let start = all_logs.len().saturating_sub(count);
        Ok(all_logs.into_iter().skip(start).collect())
    }

    /// Get summary info about a session's logs without reading all entries
    pub fn get_session_info(path: &Path) -> Result<SessionLogInfo> {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .with_context(|| format!("Invalid session log filename: {:?}", path))?;

        let metadata = fs::metadata(path)?;
        let file_size = metadata.len();

        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let mut log_count = 0;
        let mut error_count = 0;
        let mut warn_count = 0;
        let mut first_timestamp: Option<DateTime<Utc>> = None;
        let mut last_timestamp: Option<DateTime<Utc>> = None;

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };

            if line.trim().is_empty() {
                continue;
            }

            if let Ok(entry) = serde_json::from_str::<JsonlLogEntry>(&line) {
                log_count += 1;

                if first_timestamp.is_none() {
                    first_timestamp = Some(entry.ts);
                }
                last_timestamp = Some(entry.ts);

                match entry.level.as_str() {
                    "error" => error_count += 1,
                    "warn" => warn_count += 1,
                    _ => {}
                }
            }
        }

        Ok(SessionLogInfo {
            session_id,
            log_path: path.to_path_buf(),
            log_count,
            first_timestamp,
            last_timestamp,
            error_count,
            warn_count,
            file_size,
        })
    }

    /// List all session logs in a directory with their info
    pub fn list_sessions(log_dir: &Path) -> Result<Vec<SessionLogInfo>> {
        let sessions_dir = log_dir.join("sessions");
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();

        for entry in fs::read_dir(&sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "jsonl") {
                match Self::get_session_info(&path) {
                    Ok(info) => sessions.push(info),
                    Err(e) => {
                        tracing::debug!("Failed to read session info from {:?}: {}", path, e);
                    }
                }
            }
        }

        // Sort by last timestamp (newest first)
        sessions.sort_by(|a, b| b.last_timestamp.cmp(&a.last_timestamp));

        Ok(sessions)
    }

    /// Stream logs from a file as an iterator
    pub fn stream_logs(path: &Path) -> Result<impl Iterator<Item = Result<LogEntry>>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open log file: {:?}", path))?;

        let reader = BufReader::new(file);

        Ok(reader.lines().filter_map(|line_result| {
            match line_result {
                Ok(line) => {
                    if line.trim().is_empty() {
                        return None;
                    }
                    match serde_json::from_str::<JsonlLogEntry>(&line) {
                        Ok(entry) => Some(Ok(entry.to_log_entry())),
                        Err(e) => Some(Err(anyhow::anyhow!("Parse error: {}", e))),
                    }
                }
                Err(e) => Some(Err(e.into())),
            }
        }))
    }

    /// Search logs for a pattern
    pub fn search_logs(path: &Path, pattern: &str) -> Result<Vec<LogEntry>> {
        let pattern_lower = pattern.to_lowercase();
        let logs = Self::read_session_logs(path)?;

        Ok(logs
            .into_iter()
            .filter(|entry| {
                entry.message.to_lowercase().contains(&pattern_lower)
                    || entry.source.to_lowercase().contains(&pattern_lower)
            })
            .collect())
    }

    /// Filter logs by level
    pub fn filter_by_level(logs: &[LogEntry], min_level: LogEntryLevel) -> Vec<&LogEntry> {
        logs.iter()
            .filter(|entry| {
                match min_level {
                    LogEntryLevel::Debug => true,
                    LogEntryLevel::Info => !matches!(entry.level, LogEntryLevel::Debug),
                    LogEntryLevel::Warn => matches!(entry.level, LogEntryLevel::Warn | LogEntryLevel::Error),
                    LogEntryLevel::Error => matches!(entry.level, LogEntryLevel::Error),
                }
            })
            .collect()
    }

    // ============================================================
    // Application tracing log methods (for ~/.agents-in-a-box/logs/)
    // ============================================================

    /// Read application tracing logs (tracing-subscriber JSON format)
    pub fn read_tracing_logs(path: &Path) -> Result<Vec<LogEntry>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open log file: {:?}", path))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result
                .with_context(|| format!("Failed to read line {} from {:?}", line_num + 1, path))?;

            // Skip empty lines
            if line.trim().is_empty() {
                continue;
            }

            // Try parsing as tracing JSON format first
            if let Ok(tracing_entry) = serde_json::from_str::<TracingJsonEntry>(&line) {
                entries.push(tracing_entry.to_log_entry());
            } else {
                // Fall back to parsing as plain text log line
                if let Some(entry) = Self::parse_plain_text_line(&line) {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }

    /// Parse a plain text log line: "TIMESTAMP  LEVEL message"
    fn parse_plain_text_line(line: &str) -> Option<LogEntry> {
        // Format: "2026-01-07T00:13:10.375412Z  INFO Docker available"
        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        if parts.len() < 3 {
            return None;
        }

        let timestamp_str = parts[0];
        let level_str = parts.iter().skip(1).find(|s| !s.is_empty())?;
        let message_start = line.find(level_str)? + level_str.len();
        let message = line[message_start..].trim();

        let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        let level = match level_str.to_uppercase().as_str() {
            "ERROR" => LogEntryLevel::Error,
            "WARN" | "WARNING" => LogEntryLevel::Warn,
            "DEBUG" | "TRACE" => LogEntryLevel::Debug,
            _ => LogEntryLevel::Info,
        };

        Some(LogEntry {
            timestamp,
            level,
            source: "app".to_string(),
            message: message.to_string(),
            session_id: None,
            parsed_data: None,
            metadata: HashMap::new(),
        })
    }

    /// List application log files in the logs directory
    /// Supports both .jsonl (new) and .log (legacy) files
    pub fn list_app_logs(log_dir: &Path) -> Result<Vec<AppLogInfo>> {
        if !log_dir.exists() {
            return Ok(Vec::new());
        }

        let mut logs = Vec::new();

        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip directories and the sessions subdirectory
            if path.is_dir() {
                continue;
            }

            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Only process agents-in-a-box log files
            if !filename.starts_with("agents-in-a-box-") {
                continue;
            }

            let is_jsonl = filename.ends_with(".jsonl");
            let is_log = filename.ends_with(".log");

            if !is_jsonl && !is_log {
                continue;
            }

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_size = metadata.len();
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            // Parse display name from filename: agents-in-a-box-YYYYMMDD-HHMMSS.jsonl
            let display_name = Self::parse_filename_to_display(&filename);

            // Get log counts by scanning the file
            let (log_count, error_count, warn_count) = Self::count_logs_in_file(&path, is_jsonl);

            logs.push(AppLogInfo {
                filename,
                display_name,
                log_path: path,
                log_count,
                error_count,
                warn_count,
                file_size,
                modified,
                is_jsonl,
            });
        }

        // Sort by modified time (newest first)
        logs.sort_by(|a, b| b.modified.cmp(&a.modified));

        Ok(logs)
    }

    /// Parse filename to human-readable display name
    fn parse_filename_to_display(filename: &str) -> String {
        // agents-in-a-box-YYYYMMDD-HHMMSS.jsonl -> YYYY-MM-DD HH:MM:SS
        let stripped = filename
            .strip_prefix("agents-in-a-box-")
            .unwrap_or(filename);
        let stripped = stripped
            .strip_suffix(".jsonl")
            .or_else(|| stripped.strip_suffix(".log"))
            .unwrap_or(stripped);

        // Parse YYYYMMDD-HHMMSS
        if stripped.len() >= 15 {
            let date_part = &stripped[0..8];   // YYYYMMDD
            let time_part = &stripped[9..15];  // HHMMSS

            if date_part.len() == 8 && time_part.len() == 6 {
                return format!(
                    "{}-{}-{} {}:{}:{}",
                    &date_part[0..4],  // YYYY
                    &date_part[4..6],  // MM
                    &date_part[6..8],  // DD
                    &time_part[0..2],  // HH
                    &time_part[2..4],  // MM
                    &time_part[4..6],  // SS
                );
            }
        }

        stripped.to_string()
    }

    /// Count logs in a file (quick scan)
    fn count_logs_in_file(path: &Path, is_jsonl: bool) -> (usize, usize, usize) {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return (0, 0, 0),
        };

        let reader = BufReader::new(file);
        let mut log_count = 0;
        let mut error_count = 0;
        let mut warn_count = 0;

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };

            if line.trim().is_empty() {
                continue;
            }

            log_count += 1;

            if is_jsonl {
                // Parse JSON to check level
                if let Ok(entry) = serde_json::from_str::<TracingJsonEntry>(&line) {
                    match entry.level.to_uppercase().as_str() {
                        "ERROR" => error_count += 1,
                        "WARN" => warn_count += 1,
                        _ => {}
                    }
                }
            } else {
                // Check plain text for level
                let upper = line.to_uppercase();
                if upper.contains(" ERROR ") {
                    error_count += 1;
                } else if upper.contains(" WARN ") {
                    warn_count += 1;
                }
            }
        }

        (log_count, error_count, warn_count)
    }

    /// Get info about a specific app log file
    pub fn get_app_log_info(path: &Path) -> Result<AppLogInfo> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .with_context(|| format!("Invalid log filename: {:?}", path))?;

        let metadata = fs::metadata(path)?;
        let file_size = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        let is_jsonl = filename.ends_with(".jsonl");
        let display_name = Self::parse_filename_to_display(&filename);
        let (log_count, error_count, warn_count) = Self::count_logs_in_file(path, is_jsonl);

        Ok(AppLogInfo {
            filename,
            display_name,
            log_path: path.to_path_buf(),
            log_count,
            error_count,
            warn_count,
            file_size,
            modified,
            is_jsonl,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::log_writer::JsonlLogWriter;
    use tempfile::tempdir;

    #[test]
    fn test_read_written_logs() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        // Write some logs
        let mut writer = JsonlLogWriter::new(log_dir.clone()).unwrap();
        let session_id = Uuid::new_v4();

        let entry1 = LogEntry::new(LogEntryLevel::Info, "test".to_string(), "Message 1".to_string());
        let entry2 = LogEntry::new(LogEntryLevel::Error, "test".to_string(), "Error message".to_string());
        let entry3 = LogEntry::new(LogEntryLevel::Warn, "test".to_string(), "Warning".to_string());

        writer.write_entry(session_id, &entry1).unwrap();
        writer.write_entry(session_id, &entry2).unwrap();
        writer.write_entry(session_id, &entry3).unwrap();
        writer.flush().unwrap();

        // Read them back
        let log_path = writer.session_log_path(session_id);
        let logs = JsonlLogReader::read_session_logs(&log_path).unwrap();

        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].message, "Message 1");
        assert!(matches!(logs[1].level, LogEntryLevel::Error));
    }

    #[test]
    fn test_session_info() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let mut writer = JsonlLogWriter::new(log_dir.clone()).unwrap();
        let session_id = Uuid::new_v4();

        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Info, "test".to_string(), "Info".to_string())).unwrap();
        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Error, "test".to_string(), "Error".to_string())).unwrap();
        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Warn, "test".to_string(), "Warn".to_string())).unwrap();
        writer.flush().unwrap();

        let log_path = writer.session_log_path(session_id);
        let info = JsonlLogReader::get_session_info(&log_path).unwrap();

        assert_eq!(info.session_id, session_id);
        assert_eq!(info.log_count, 3);
        assert_eq!(info.error_count, 1);
        assert_eq!(info.warn_count, 1);
    }

    #[test]
    fn test_list_sessions() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let mut writer = JsonlLogWriter::new(log_dir.clone()).unwrap();

        // Create two sessions
        let session1 = Uuid::new_v4();
        let session2 = Uuid::new_v4();

        writer.write_entry(session1, &LogEntry::new(LogEntryLevel::Info, "s1".to_string(), "Session 1".to_string())).unwrap();
        writer.write_entry(session2, &LogEntry::new(LogEntryLevel::Info, "s2".to_string(), "Session 2".to_string())).unwrap();
        writer.flush().unwrap();

        let sessions = JsonlLogReader::list_sessions(&log_dir).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_search_logs() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let mut writer = JsonlLogWriter::new(log_dir).unwrap();
        let session_id = Uuid::new_v4();

        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Info, "test".to_string(), "Hello world".to_string())).unwrap();
        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Info, "test".to_string(), "Goodbye world".to_string())).unwrap();
        writer.write_entry(session_id, &LogEntry::new(LogEntryLevel::Info, "test".to_string(), "Something else".to_string())).unwrap();
        writer.flush().unwrap();

        let log_path = writer.session_log_path(session_id);
        let results = JsonlLogReader::search_logs(&log_path, "world").unwrap();

        assert_eq!(results.len(), 2);
    }
}
