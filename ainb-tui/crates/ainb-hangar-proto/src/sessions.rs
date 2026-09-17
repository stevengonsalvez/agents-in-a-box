//! Wire types for durable sessions RPC (spec P6d, #1166).

use serde::{Deserialize, Serialize};

/// One session entry on the wire, matching `SessionMetadata`'s 13 fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionEntry {
    /// Unique session ID (UUID string).
    pub session_id: String,
    /// Tmux session name (unique).
    pub tmux_session_name: String,
    /// Worktree path string.
    pub worktree_path: String,
    /// Owning workspace slug or name.
    pub workspace_name: String,
    /// Creation timestamp in epoch milliseconds.
    pub created_at: i64,
    /// Agent type string (e.g. "Claude", "Codex", "Shell").
    pub agent_type: String,
    /// Whether headroom proxy is enabled.
    #[serde(default)]
    pub headroom_enabled: bool,
    /// Whether RTK hooks are enabled.
    #[serde(default)]
    pub rtk_enabled: bool,
    /// Optional skip_permissions flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_permissions: Option<bool>,
    /// Model name or ID override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model source string ("LegacyTyped", "Raw").
    #[serde(default = "default_model_source")]
    pub model_source: String,
    /// Legacy Codex model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_model: Option<String>,
    /// Shared Codex app-server remote thread ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_thread_id: Option<String>,
}

fn default_model_source() -> String {
    "LegacyTyped".to_string()
}

/// Request parameters for `workspace/session_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionListParams {
    /// Optional workspace filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
}

/// Result envelope for `workspace/session_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionListResult {
    /// All matching sessions, ordered newest first.
    pub sessions: Vec<WorkspaceSessionEntry>,
}

/// Request parameters for `workspace/session_upsert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionUpsertParams {
    /// Session metadata to persist.
    pub session: WorkspaceSessionEntry,
}

/// Result envelope for `workspace/session_upsert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionUpsertResult {
    /// Whether the upsert succeeded.
    pub ok: bool,
}

/// Request parameters for `workspace/session_delete`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionDeleteParams {
    /// Delete by session UUID string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Delete by tmux session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session_name: Option<String>,
}

/// Result envelope for `workspace/session_delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionDeleteResult {
    /// Whether a matching session was deleted.
    pub deleted: bool,
}
