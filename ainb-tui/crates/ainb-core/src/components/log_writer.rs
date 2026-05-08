// ABOUTME: JSONL log writer for persisting session logs to disk
// Writes log entries in JSON Lines format for efficient streaming and parsing

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use uuid::Uuid;

use super::live_logs_stream::LogEntryLevel;
// log_parser types not currently used, but kept for future expansion

/// Serializable log entry for JSONL format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonlLogEntry {
    /// Timestamp in ISO 8601 format
    pub ts: DateTime<Utc>,
    /// Log level
    pub level: String,
    /// Source (container name or component)
    pub source: String,
    /// Log message content
    pub msg: String,
    /// Session UUID
    pub session: Uuid,
    /// Optional category
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<String>,
    /// Additional metadata
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub meta: HashMap<String, String>,
}

impl JsonlLogEntry {
    /// Create from a LogEntry
    pub fn from_log_entry(
        entry: &super::live_logs_stream::LogEntry,
        session_id: Uuid,
    ) -> Self {
        let level = match entry.level {
            LogEntryLevel::Debug => "debug",
            LogEntryLevel::Info => "info",
            LogEntryLevel::Warn => "warn",
            LogEntryLevel::Error => "error",
        };

        let cat = entry.parsed_data.as_ref().map(|p| p.category.label().to_string());

        Self {
            ts: entry.timestamp,
            level: level.to_string(),
            source: entry.source.clone(),
            msg: entry.message.clone(),
            session: session_id,
            cat,
            meta: entry.metadata.clone(),
        }
    }

    /// Convert back to LogEntry for display
    pub fn to_log_entry(&self) -> super::live_logs_stream::LogEntry {
        let level = match self.level.as_str() {
            "debug" => LogEntryLevel::Debug,
            "warn" => LogEntryLevel::Warn,
            "error" => LogEntryLevel::Error,
            _ => LogEntryLevel::Info,
        };

        super::live_logs_stream::LogEntry {
            timestamp: self.ts,
            level,
            source: self.source.clone(),
            message: self.msg.clone(),
            session_id: Some(self.session),
            parsed_data: None,
            metadata: self.meta.clone(),
        }
    }
}

/// JSONL log writer that persists log entries to disk
#[derive(Debug)]
pub struct JsonlLogWriter {
    /// Base directory for log files
    log_dir: PathBuf,
    /// Open file handles per session
    session_files: HashMap<Uuid, BufWriter<File>>,
    /// Whether writing is enabled
    enabled: bool,
}

impl JsonlLogWriter {
    /// Create a new log writer
    ///
    /// # Arguments
    /// * `log_dir` - Base directory for log files (will create sessions/ subdirectory)
    pub fn new(log_dir: PathBuf) -> Result<Self> {
        let sessions_dir = log_dir.join("sessions");
        fs::create_dir_all(&sessions_dir)
            .with_context(|| format!("Failed to create log directory: {:?}", sessions_dir))?;

        Ok(Self {
            log_dir,
            session_files: HashMap::new(),
            enabled: true,
        })
    }

    /// Create a disabled writer (for testing or when logging is off)
    pub fn disabled() -> Self {
        Self {
            log_dir: PathBuf::new(),
            session_files: HashMap::new(),
            enabled: false,
        }
    }

    /// Check if the writer is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the log directory path
    pub fn log_dir(&self) -> &PathBuf {
        &self.log_dir
    }

    /// Get the sessions directory path
    pub fn sessions_dir(&self) -> PathBuf {
        self.log_dir.join("sessions")
    }

    /// Get the path for a session's log file
    pub fn session_log_path(&self, session_id: Uuid) -> PathBuf {
        self.sessions_dir().join(format!("{}.jsonl", session_id))
    }

    /// Write a log entry for a session
    pub fn write_entry(
        &mut self,
        session_id: Uuid,
        entry: &super::live_logs_stream::LogEntry,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let jsonl_entry = JsonlLogEntry::from_log_entry(entry, session_id);
        self.write_jsonl_entry(session_id, &jsonl_entry)
    }

    /// Write a raw JSONL entry
    pub fn write_jsonl_entry(
        &mut self,
        session_id: Uuid,
        entry: &JsonlLogEntry,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let writer = self.get_or_create_writer(session_id)?;

        let json = serde_json::to_string(entry)
            .context("Failed to serialize log entry")?;

        writeln!(writer, "{}", json)
            .context("Failed to write log entry")?;

        Ok(())
    }

    /// Get or create a buffered writer for a session
    fn get_or_create_writer(&mut self, session_id: Uuid) -> Result<&mut BufWriter<File>> {
        if !self.session_files.contains_key(&session_id) {
            let path = self.session_log_path(session_id);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("Failed to open log file: {:?}", path))?;

            self.session_files.insert(session_id, BufWriter::new(file));
        }

        Ok(self.session_files.get_mut(&session_id).unwrap())
    }

    /// Flush all buffered writes to disk
    pub fn flush(&mut self) -> Result<()> {
        for (session_id, writer) in &mut self.session_files {
            writer.flush()
                .with_context(|| format!("Failed to flush logs for session {}", session_id))?;
        }
        Ok(())
    }

    /// Close the log file for a session
    pub fn close_session(&mut self, session_id: Uuid) -> Result<()> {
        if let Some(mut writer) = self.session_files.remove(&session_id) {
            writer.flush()
                .with_context(|| format!("Failed to flush logs for session {}", session_id))?;
        }
        Ok(())
    }

    /// Close all session log files
    pub fn close_all(&mut self) -> Result<()> {
        for (_, mut writer) in self.session_files.drain() {
            let _ = writer.flush();
        }
        Ok(())
    }

    /// List all session log files
    pub fn list_session_logs(&self) -> Result<Vec<(Uuid, PathBuf)>> {
        let sessions_dir = self.sessions_dir();
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in fs::read_dir(&sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "jsonl") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(uuid) = Uuid::parse_str(stem) {
                        sessions.push((uuid, path));
                    }
                }
            }
        }

        // Sort by modification time (newest first)
        sessions.sort_by(|a, b| {
            let time_a = fs::metadata(&a.1).and_then(|m| m.modified()).ok();
            let time_b = fs::metadata(&b.1).and_then(|m| m.modified()).ok();
            time_b.cmp(&time_a)
        });

        Ok(sessions)
    }
}

impl Drop for JsonlLogWriter {
    fn drop(&mut self) {
        let _ = self.close_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_jsonl_entry_serialization() {
        let entry = JsonlLogEntry {
            ts: Utc::now(),
            level: "info".to_string(),
            source: "test-container".to_string(),
            msg: "Test message".to_string(),
            session: Uuid::new_v4(),
            cat: Some("System".to_string()),
            meta: HashMap::new(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"level\":\"info\""));
        assert!(json.contains("\"msg\":\"Test message\""));

        // Deserialize back
        let parsed: JsonlLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.level, "info");
        assert_eq!(parsed.msg, "Test message");
    }

    #[test]
    fn test_log_writer_creates_directory() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");

        let writer = JsonlLogWriter::new(log_dir.clone()).unwrap();
        assert!(writer.sessions_dir().exists());
    }

    #[test]
    fn test_log_writer_writes_entries() {
        let temp = tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        let mut writer = JsonlLogWriter::new(log_dir).unwrap();

        let session_id = Uuid::new_v4();
        let entry = super::super::live_logs_stream::LogEntry::new(
            LogEntryLevel::Info,
            "test".to_string(),
            "Test message".to_string(),
        );

        writer.write_entry(session_id, &entry).unwrap();
        writer.flush().unwrap();

        // Verify file exists and contains the entry
        let log_path = writer.session_log_path(session_id);
        assert!(log_path.exists());

        let contents = fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("Test message"));
    }

    #[test]
    fn test_disabled_writer() {
        let writer = JsonlLogWriter::disabled();
        assert!(!writer.is_enabled());
    }
}
