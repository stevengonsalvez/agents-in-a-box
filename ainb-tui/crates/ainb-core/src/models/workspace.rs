// ABOUTME: Workspace data model representing a git repository that can contain multiple sessions

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{Session, ShellSession};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub path: PathBuf,
    pub sessions: Vec<Session>,
    /// Single shell session per workspace (used for quick directory switching)
    #[serde(default)]
    pub shell_session: Option<ShellSession>,
    // Legacy field for migration - will be removed in future
    #[serde(default, skip_serializing)]
    shell_sessions: Vec<ShellSession>,
}

impl Workspace {
    pub fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            sessions: Vec::new(),
            shell_session: None,
            shell_sessions: Vec::new(),
        }
    }

    pub fn add_session(&mut self, session: Session) {
        self.sessions.push(session);
    }

    pub fn remove_session(&mut self, session_id: &uuid::Uuid) -> bool {
        let initial_len = self.sessions.len();
        self.sessions.retain(|s| &s.id != session_id);
        self.sessions.len() != initial_len
    }

    pub fn get_session_mut(&mut self, session_id: &uuid::Uuid) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| &s.id == session_id)
    }

    pub fn get_session(&self, session_id: &uuid::Uuid) -> Option<&Session> {
        self.sessions.iter().find(|s| &s.id == session_id)
    }

    pub fn running_sessions(&self) -> Vec<&Session> {
        self.sessions.iter().filter(|s| s.status.is_running()).collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    // Shell session methods (single shell per workspace)

    /// Set the workspace shell session
    pub fn set_shell_session(&mut self, shell_session: ShellSession) {
        self.shell_session = Some(shell_session);
    }

    /// Clear the workspace shell session
    pub fn clear_shell_session(&mut self) {
        self.shell_session = None;
    }

    /// Get mutable reference to shell session
    pub fn get_shell_session_mut(&mut self) -> Option<&mut ShellSession> {
        self.shell_session.as_mut()
    }

    /// Get reference to shell session
    pub fn get_shell_session(&self) -> Option<&ShellSession> {
        self.shell_session.as_ref()
    }

    /// Check if shell session exists and is running
    pub fn has_running_shell(&self) -> bool {
        self.shell_session.as_ref().map(|s| s.status.is_running()).unwrap_or(false)
    }

    /// Total count of all sessions (AI + shell)
    pub fn total_session_count(&self) -> usize {
        self.sessions.len() + if self.shell_session.is_some() { 1 } else { 0 }
    }

    /// Migrate legacy shell_sessions to single shell_session (if any)
    pub fn migrate_legacy_shells(&mut self) {
        if self.shell_session.is_none() && !self.shell_sessions.is_empty() {
            // Take the most recently accessed shell session
            if let Some(shell) = self.shell_sessions.iter()
                .max_by_key(|s| s.last_accessed)
                .cloned()
            {
                self.shell_session = Some(shell);
            }
        }
        self.shell_sessions.clear();
    }
}
