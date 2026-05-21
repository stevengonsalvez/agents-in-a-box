// ABOUTME: Shared fleet types — Session / SessionState / Signal / Block / Liveness.

use serde::{Deserialize, Serialize};

/// Which discovery source contributed to a session record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    /// `ainb list --format json`
    Ainb,
    /// `~/.claude-peers.db` (broker SQLite)
    Peers,
    /// `~/.claude/jobs/<id>/`
    Jobs,
}

/// Unified session identity. May be backed by 1+ sources after merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Stable id — preferred order: peer id > ainb session_id > bg job id.
    pub id: String,

    /// Working directory. Primary key for cross-source dedupe.
    pub cwd: String,

    /// OS pid when known (peers and bg jobs publish; ainb does not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,

    /// Git root resolved at discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,

    /// tmux session name if running in tmux (from ainb or peer.tty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,

    /// ainb workspace name (e.g. `shotclubhouse_shotclubhouse_feat_x`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,

    /// Worktree path (ainb-managed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,

    /// Peer id in claude-peers broker, if registered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,

    /// Background-job id (~/.claude/jobs/<id>/) if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_job_id: Option<String>,

    /// Path to the active JSONL transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,

    /// Sources that contributed to this record.
    pub sources: Vec<SessionSource>,

    /// Peer-published summary. May start with `WAITING:` to flag block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Unix ms of last activity observed by any source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
}

/// State signals merged into [`SessionState`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Signal {
    TurnEnd { at: i64 },
    TurnActive { at: i64 },
    AskUserQuestion { at: i64, raw: String },
    WaitingSummary { at: i64, summary: String },
    NeedsInputMarker { at: i64, source: String },
    ApiError { at: i64, pattern: String, raw: String },
    Idle { at: i64, since_ms: i64 },
}

/// Coarse liveness derived from signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    MidTurn,
    Idle,
    Unknown,
}

/// Whether the session is blocked waiting on something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Block {
    None,
    NeedsInput,
    ApiError,
    WaitingSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session: Session,
    pub liveness: Liveness,
    pub block: Block,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_ms: Option<i64>,
    pub signals: Vec<Signal>,
}

/// Mirrors the `peers` row in `~/.claude-peers.db`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerPeer {
    pub id: String,
    pub pid: u32,
    pub cwd: String,
    pub git_root: Option<String>,
    pub tty: Option<String>,
    pub summary: String,
    pub registered_at: String,
    pub last_seen: String,
}

/// Mirrors `ainb list --format json` row shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AinbSession {
    pub session_id: String,
    pub tmux_session_name: String,
    pub workspace_name: String,
    pub worktree_path: String,
    pub created_at: String,
    pub is_running: bool,
    pub claude_active: bool,
}

/// Outcome of a send-route decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "kebab-case")]
pub enum SendOutcome {
    Broker { peer_id: String },
    Tmux { tmux_session: String },
    Failed { reason: String },
}
