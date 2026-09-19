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
    /// Most rows to return. Absent or above [`SESSION_LIST_MAX`] means
    /// [`SESSION_LIST_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result envelope for `workspace/session_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionListResult {
    /// Matching sessions, ordered newest first, at most the requested limit.
    pub sessions: Vec<WorkspaceSessionEntry>,
    /// More rows matched than were returned.
    #[serde(default)]
    pub truncated: bool,
    /// The one-time import of `sessions.json` AND at least one reconcile
    /// pass of that file have completed, so the table is authoritative.
    /// `false` means either has failed or not yet run, and a client must not
    /// treat an empty table as "no sessions".
    #[serde(default)]
    pub import_complete: bool,
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

/// Request parameters for `workspace/session_reconcile`. It takes none: the
/// daemon reconciles the `sessions.json` it resolves for its own home.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionReconcileParams {}

/// Result envelope for `workspace/session_reconcile`: the pass that ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionReconcileResult {
    /// File sessions inserted into the table by this pass.
    pub imported: i64,
    /// File sessions not inserted because their tmux name belongs to another
    /// session id in the table (the table wins).
    pub skipped: i64,
    /// File records that failed validation and stayed in the file only.
    pub rejected: i64,
    /// Table rows deleted because `sessions.json` no longer has them (the
    /// file is the authority on which sessions exist until the flip).
    #[serde(default)]
    pub deleted: i64,
    /// Unix milliseconds when the pass committed.
    pub completed_at: i64,
}

/// Upper bound on the rows one `workspace/session_list` returns.
pub const SESSION_LIST_MAX: u32 = 2_000;
/// Longest accepted session id, in bytes. A canonical UUID is 36.
pub const SESSION_ID_MAX_LEN: usize = 128;
/// Longest accepted tmux session name, in bytes.
pub const TMUX_NAME_MAX_LEN: usize = 256;
/// Longest accepted workspace name, in bytes.
pub const WORKSPACE_NAME_MAX_LEN: usize = 256;
/// Longest accepted worktree path, in bytes.
pub const WORKTREE_PATH_MAX_LEN: usize = 4_096;
/// Longest accepted agent type or model source tag, in bytes.
pub const TAG_MAX_LEN: usize = 64;
/// Longest accepted model name, in bytes.
pub const MODEL_MAX_LEN: usize = 256;
/// Longest accepted Codex thread id, in bytes.
pub const THREAD_ID_MAX_LEN: usize = 128;

/// Whether `id` is a canonical hyphenated UUID (`8-4-4-4-12` hex digits).
///
/// The CLI keys sessions by `uuid::Uuid`; an id that does not parse would
/// have to be replaced by an invented one on every read, so the boundary
/// refuses it instead.
#[must_use]
pub fn is_canonical_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Whether `name` is usable as an exact tmux session target.
///
/// Non-empty, at most [`TMUX_NAME_MAX_LEN`] bytes, no whitespace or control
/// characters, none of `:` and `.` (tmux's target separators, which tmux
/// itself refuses in a session name), and no leading `=` or `-` (an exact
/// target prefix and a flag).
#[must_use]
pub fn is_valid_tmux_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= TMUX_NAME_MAX_LEN
        && !name.starts_with('=')
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ':' || c == '.')
}

fn check_len(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        return Err(format!(
            "{field} is {} bytes; the limit is {max}",
            value.len()
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} contains a control character"));
    }
    Ok(())
}

impl WorkspaceSessionEntry {
    /// Check every field against the limits above.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first field that fails.
    pub fn validate(&self) -> Result<(), String> {
        check_len("session_id", &self.session_id, SESSION_ID_MAX_LEN)?;
        if !is_canonical_uuid(&self.session_id) {
            return Err("session_id must be a canonical UUID".to_string());
        }
        if !is_valid_tmux_name(&self.tmux_session_name) {
            return Err(
                "tmux_session_name must be 1 to 256 bytes with no whitespace, control \
                 characters, ':' or '.', and must not start with '=' or '-'"
                    .to_string(),
            );
        }
        check_len("worktree_path", &self.worktree_path, WORKTREE_PATH_MAX_LEN)?;
        let path = std::path::Path::new(&self.worktree_path);
        if !path.is_absolute()
            || path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("worktree_path must be absolute with no '..' component".to_string());
        }
        check_len(
            "workspace_name",
            &self.workspace_name,
            WORKSPACE_NAME_MAX_LEN,
        )?;
        check_len("agent_type", &self.agent_type, TAG_MAX_LEN)?;
        check_len("model_source", &self.model_source, TAG_MAX_LEN)?;
        if let Some(model) = &self.model {
            check_len("model", model, MODEL_MAX_LEN)?;
        }
        if let Some(codex_model) = &self.codex_model {
            check_len("codex_model", codex_model, MODEL_MAX_LEN)?;
        }
        if let Some(thread) = &self.codex_thread_id {
            check_len("codex_thread_id", thread, THREAD_ID_MAX_LEN)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> WorkspaceSessionEntry {
        WorkspaceSessionEntry {
            session_id: "12345678-1234-1234-1234-123456789abc".to_string(),
            tmux_session_name: "tmux_repo_feat-x".to_string(),
            worktree_path: "/home/u/.agents-in-a-box/worktrees/repo".to_string(),
            workspace_name: "repo".to_string(),
            created_at: 1,
            agent_type: "Claude".to_string(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: "LegacyTyped".to_string(),
            codex_model: None,
            codex_thread_id: None,
        }
    }

    #[test]
    fn a_well_formed_entry_passes() {
        assert_eq!(entry().validate(), Ok(()));
    }

    #[test]
    fn a_ulid_session_id_is_refused() {
        let mut e = entry();
        e.session_id = "01J8Z3K6Q2N4T5V7W9X0Y1Z2A3".to_string();
        assert!(e.validate().unwrap_err().contains("session_id"));
    }

    #[test]
    fn an_unhyphenated_uuid_is_refused() {
        assert!(!is_canonical_uuid("12345678123412341234123456789abc"));
        assert!(is_canonical_uuid("12345678-1234-1234-1234-123456789ABC"));
    }

    #[test]
    fn tmux_names_with_separators_or_control_characters_are_refused() {
        for bad in [
            "",
            "a b",
            "a\nb",
            "a:b",
            "a.b",
            "=a",
            "-a",
            &"x".repeat(257),
        ] {
            let mut e = entry();
            e.tmux_session_name = bad.to_string();
            assert!(e.validate().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn relative_or_parent_worktree_paths_are_refused() {
        for bad in ["", "relative/path", "/a/../b"] {
            let mut e = entry();
            e.worktree_path = bad.to_string();
            assert!(
                e.validate().unwrap_err().contains("worktree_path"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn oversized_fields_are_refused() {
        let mut e = entry();
        e.workspace_name = "w".repeat(WORKSPACE_NAME_MAX_LEN + 1);
        assert!(e.validate().unwrap_err().contains("workspace_name"));

        let mut e = entry();
        e.model = Some("m".repeat(MODEL_MAX_LEN + 1));
        assert!(e.validate().unwrap_err().contains("model"));

        let mut e = entry();
        e.worktree_path = format!("/{}", "p".repeat(WORKTREE_PATH_MAX_LEN));
        assert!(e.validate().unwrap_err().contains("worktree_path"));
    }
}
