// ABOUTME: Shared fleet types — Session / SessionState / Signal / Block / Liveness.

use std::collections::BTreeSet;
use std::fmt;

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
    /// Exact pane metadata from `tmux list-panes -a`.
    Tmux,
}

/// Provider owning a Fleet session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Copilot,
    Unknown,
}

impl Provider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable Fleet identity. Cwd is deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(String);

impl SessionKey {
    /// Identity for a provider session with an authoritative provider id.
    #[must_use]
    pub fn managed(provider: Provider, provider_session_id: &str) -> Self {
        Self(format!("{provider}:{provider_session_id}"))
    }

    /// Identity for a legacy pane without an authoritative provider id.
    #[must_use]
    pub fn legacy(
        provider: Provider,
        exact_tmux_target: &str,
        process_start_fingerprint: &str,
    ) -> Self {
        Self(format!(
            "legacy:{provider}:{exact_tmux_target}:{process_start_fingerprint}"
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Provider lifecycle, independent from attention state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifecycleState {
    Starting,
    Running,
    TurnComplete,
    Idle,
    Exited,
    Unknown,
}

/// Human attention required by a session, independent from lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttentionState {
    None,
    Ask,
    Approval,
    Waiting,
    Error,
}

/// Whether Fleet has an authoritative provider control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagementState {
    Managed,
    Degraded,
}

/// Control and observation features supported for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    StructuredAnswer,
    ApprovalDecision,
    SendPrompt,
    ContinueTurn,
    RetryTurn,
    InterruptTurn,
    Start,
    Restart,
    Stop,
    Kill,
    Archive,
    TextSend,
    TmuxAttach,
}

/// Explicit capability set used to gate Fleet actions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(BTreeSet<Capability>);

impl Capabilities {
    #[must_use]
    pub fn new(items: impl IntoIterator<Item = Capability>) -> Self {
        Self(items.into_iter().collect())
    }

    #[must_use]
    pub fn degraded_tmux() -> Self {
        Self::new([Capability::TextSend, Capability::TmuxAttach])
    }

    #[must_use]
    pub fn contains(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }

    pub fn extend(&mut self, other: &Self) {
        self.0.extend(other.0.iter().copied());
    }
}

/// Origin of a Fleet observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    AppServer,
    Hook,
    Tmux,
    AinbRegistry,
    Broker,
    Job,
    Transcript,
}

/// Trust level assigned to an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Inferred,
    Observed,
    Authoritative,
}

/// Current health of a session control or observation transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportHealth {
    Healthy,
    Degraded,
    Unavailable,
    Unknown,
}

/// Authoritative Fleet read-model record used by new control-plane surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSession {
    pub session_key: SessionKey,
    pub provider: Provider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_tmux_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_start_fingerprint: Option<String>,
    pub lifecycle: LifecycleState,
    pub attention: AttentionState,
    pub management: ManagementState,
    pub capabilities: Capabilities,
    pub provenance: BTreeSet<Provenance>,
    pub confidence: Confidence,
    pub transport_health: TransportHealth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
    pub version: u64,
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
    TurnEnd {
        at: i64,
    },
    TurnActive {
        at: i64,
    },
    AskUserQuestion {
        at: i64,
        raw: String,
    },
    WaitingSummary {
        at: i64,
        summary: String,
    },
    NeedsInputMarker {
        at: i64,
        source: String,
    },
    ApiError {
        at: i64,
        pattern: String,
        raw: String,
    },
    Idle {
        at: i64,
        since_ms: i64,
    },
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

#[cfg(test)]
mod fleet_identity_tests {
    use super::*;

    #[test]
    fn managed_key_uses_provider_and_provider_session_id() {
        assert_eq!(
            SessionKey::managed(Provider::Codex, "thread-123").as_str(),
            "codex:thread-123"
        );
        assert_eq!(
            SessionKey::managed(Provider::Claude, "session-456").as_str(),
            "claude:session-456"
        );
    }

    #[test]
    fn legacy_key_changes_with_exact_target_or_process_start() {
        let first = SessionKey::legacy(Provider::Claude, "agent:1.0", "pid=1;started=10");
        let other_pane = SessionKey::legacy(Provider::Claude, "agent:1.1", "pid=1;started=10");
        let restarted = SessionKey::legacy(Provider::Claude, "agent:1.0", "pid=2;started=20");

        assert_ne!(first, other_pane);
        assert_ne!(first, restarted);
    }

    #[test]
    fn degraded_capabilities_exclude_structured_and_destructive_actions() {
        let capabilities = Capabilities::degraded_tmux();
        assert!(capabilities.contains(Capability::TextSend));
        assert!(capabilities.contains(Capability::TmuxAttach));
        assert!(!capabilities.contains(Capability::StructuredAnswer));
        assert!(!capabilities.contains(Capability::Kill));
    }
}
