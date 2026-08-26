// ABOUTME: Persistent storage for durable session labels
// Stores custom labels by stable tmux session name.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Normalize a durable session label before it reaches disk or the UI.
///
/// Blank input clears a label. Control characters are rejected because labels
/// render inside terminal rows, and 64 characters keeps every layout usable.
pub fn normalize_session_label(raw: &str) -> Result<Option<String>, String> {
    let label = raw.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.chars().any(char::is_control) {
        return Err("Session label cannot contain control characters".to_string());
    }
    if label.chars().count() > 64 {
        return Err("Session label must be 64 characters or fewer".to_string());
    }
    Ok(Some(label.to_string()))
}

/// Store for durable session labels.
/// Maps tmux session name to a human-provided label, independent from Git.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionLabelStore {
    /// Map of tmux_session_name -> display_name
    #[serde(flatten)]
    names: HashMap<String, String>,
}

impl SessionLabelStore {
    /// Get the storage file path
    fn storage_path() -> Option<PathBuf> {
        std::env::var_os("AINB_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .map(|base| base.join(".agents-in-a-box").join("session-labels.json"))
    }

    fn legacy_storage_path() -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join(".agents-in-a-box").join("ssh_display_names.json"))
    }

    /// Load from disk (returns empty store if file doesn't exist)
    pub fn load() -> Self {
        Self::storage_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .or_else(|| Self::legacy_storage_path().and_then(|path| fs::read_to_string(path).ok()))
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Save to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        if let Some(path) = Self::storage_path() {
            // Ensure directory exists
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = serde_json::to_string_pretty(self)?;
            fs::write(path, content)?;
        }
        Ok(())
    }

    /// Get display name for a tmux session
    pub fn get(&self, tmux_session_name: &str) -> Option<&String> {
        self.names.get(tmux_session_name)
    }

    /// Set display name (None removes it)
    pub fn set(&mut self, tmux_session_name: String, display_name: Option<String>) {
        match display_name {
            Some(name) => {
                self.names.insert(tmux_session_name, name);
            }
            None => {
                self.names.remove(&tmux_session_name);
            }
        }
    }
}

/// Compatibility alias for code that still refers to SSH display names.
pub type SshDisplayNameStore = SessionLabelStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_labels_trim_clear_and_reject_unsafe_values() {
        assert_eq!(
            normalize_session_label("  RPC flake  ").expect("valid label"),
            Some("RPC flake".to_string())
        );
        assert_eq!(normalize_session_label("   ").expect("blank clears"), None);
        assert!(normalize_session_label("two\nlines").is_err());
        assert!(normalize_session_label(&"x".repeat(65)).is_err());
    }

    #[test]
    fn test_store_get_set() {
        let mut store = SessionLabelStore::default();

        // Initially empty
        assert!(store.get("ssh-test-22").is_none());

        // Set a name
        store.set("ssh-test-22".to_string(), Some("My Server".to_string()));
        assert_eq!(store.get("ssh-test-22"), Some(&"My Server".to_string()));

        // Clear the name
        store.set("ssh-test-22".to_string(), None);
        assert!(store.get("ssh-test-22").is_none());
    }

    #[test]
    fn test_store_serialization() {
        let mut store = SessionLabelStore::default();
        store.set("ssh-prod-22".to_string(), Some("Production".to_string()));
        store.set("ssh-staging-22".to_string(), Some("Staging".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: SessionLabelStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.get("ssh-prod-22"), Some(&"Production".to_string()));
        assert_eq!(loaded.get("ssh-staging-22"), Some(&"Staging".to_string()));
    }
}
