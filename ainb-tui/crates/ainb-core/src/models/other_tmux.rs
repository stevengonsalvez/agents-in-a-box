// ABOUTME: Model for tmux sessions not managed by agents-in-a-box

use serde::{Deserialize, Serialize};

/// Represents a tmux session that exists on the system but was not
/// created by agents-in-a-box (i.e., doesn't have the "tmux_" prefix)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtherTmuxSession {
    /// The tmux session name
    pub name: String,
    /// Whether someone is currently attached to this session
    pub attached: bool,
    /// Number of windows in the session
    pub windows: usize,
    /// Creation time (if available from tmux)
    pub created: Option<String>,
}

impl OtherTmuxSession {
    pub fn new(name: String, attached: bool, windows: usize) -> Self {
        Self {
            name,
            attached,
            windows,
            created: None,
        }
    }

    /// Status indicator for display
    pub fn status_indicator(&self) -> &'static str {
        if self.attached {
            "🔗" // Attached
        } else {
            "○" // Not attached
        }
    }
}
