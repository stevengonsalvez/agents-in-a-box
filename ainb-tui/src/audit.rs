// ABOUTME: Audit logging for user-initiated mutations
//
// Logs all destructive or state-changing operations to the standard tracing
// log with target "audit" for easy filtering. This helps diagnose issues like
// unexpectedly deleted worktrees or changed configurations.
//
// Filter audit events: grep 'target.*audit' ~/.agents-in-a-box/logs/ainb-tui.log

use tracing::info;
use uuid::Uuid;

/// Types of auditable actions
#[derive(Debug, Clone)]
pub enum AuditAction {
    SessionCreated,
    SessionDeleted,
    SessionStopped,
    SessionResumed,
    ConfigSaved,
    OrphanedContainersCleanup,
    GitWorktreePrune,
}

impl std::fmt::Display for AuditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditAction::SessionCreated => write!(f, "SESSION_CREATED"),
            AuditAction::SessionDeleted => write!(f, "SESSION_DELETED"),
            AuditAction::SessionStopped => write!(f, "SESSION_STOPPED"),
            AuditAction::SessionResumed => write!(f, "SESSION_RESUMED"),
            AuditAction::ConfigSaved => write!(f, "CONFIG_SAVED"),
            AuditAction::OrphanedContainersCleanup => write!(f, "ORPHANED_CLEANUP"),
            AuditAction::GitWorktreePrune => write!(f, "GIT_WORKTREE_PRUNE"),
        }
    }
}

/// Result of an audited action
#[derive(Debug, Clone)]
pub enum AuditResult {
    Success,
    Failed(String),
}

impl std::fmt::Display for AuditResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditResult::Success => write!(f, "success"),
            AuditResult::Failed(msg) => write!(f, "failed: {}", msg),
        }
    }
}

/// What triggered the audit action
#[derive(Debug, Clone)]
pub enum AuditTrigger {
    UserKeypress(String),
    Automatic,
}

impl std::fmt::Display for AuditTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditTrigger::UserKeypress(key) => write!(f, "keypress:{}", key),
            AuditTrigger::Automatic => write!(f, "automatic"),
        }
    }
}

// ============================================================================
// Convenience functions for common audit scenarios
// ============================================================================

/// Log a session deletion
pub fn audit_session_deleted(
    session_id: Uuid,
    tmux_session: Option<String>,
    worktree_path: Option<String>,
    trigger: AuditTrigger,
    result: AuditResult,
) {
    info!(
        target: "audit",
        action = %AuditAction::SessionDeleted,
        session_id = %session_id,
        tmux_session = ?tmux_session,
        path = ?worktree_path,
        trigger = %trigger,
        result = %result,
        "Session deleted"
    );
}

/// Log a soft-stop (tmux killed; worktree, sessions.json, symlink preserved)
pub fn audit_session_stopped(
    session_id: Uuid,
    tmux_session: Option<String>,
    worktree_path: Option<String>,
    trigger: AuditTrigger,
    result: AuditResult,
) {
    info!(
        target: "audit",
        action = %AuditAction::SessionStopped,
        session_id = %session_id,
        tmux_session = ?tmux_session,
        path = ?worktree_path,
        trigger = %trigger,
        result = %result,
        "Session stopped"
    );
}

/// Log a resume (tmux re-created from preserved sessions.json metadata)
pub fn audit_session_resumed(
    session_id: Uuid,
    tmux_session: Option<String>,
    worktree_path: Option<String>,
    transcript_path: Option<String>,
    trigger: AuditTrigger,
    result: AuditResult,
) {
    info!(
        target: "audit",
        action = %AuditAction::SessionResumed,
        session_id = %session_id,
        tmux_session = ?tmux_session,
        path = ?worktree_path,
        transcript = ?transcript_path,
        trigger = %trigger,
        result = %result,
        "Session resumed"
    );
}

/// Log a session creation
pub fn audit_session_created(
    session_id: Uuid,
    tmux_session: &str,
    worktree_path: &str,
    branch_name: &str,
    trigger: AuditTrigger,
    result: AuditResult,
) {
    info!(
        target: "audit",
        action = %AuditAction::SessionCreated,
        session_id = %session_id,
        tmux_session = %tmux_session,
        path = %worktree_path,
        branch = %branch_name,
        trigger = %trigger,
        result = %result,
        "Session created"
    );
}

/// Log a git worktree prune operation
pub fn audit_git_worktree_prune(
    trigger: AuditTrigger,
    result: AuditResult,
    pruned_count: Option<usize>,
) {
    info!(
        target: "audit",
        action = %AuditAction::GitWorktreePrune,
        pruned_count = ?pruned_count,
        trigger = %trigger,
        result = %result,
        "Git worktree prune"
    );
}

/// Log a config save
pub fn audit_config_saved(
    config_path: &str,
    trigger: AuditTrigger,
    result: AuditResult,
    details: Option<String>,
) {
    info!(
        target: "audit",
        action = %AuditAction::ConfigSaved,
        path = %config_path,
        details = ?details,
        trigger = %trigger,
        result = %result,
        "Config saved"
    );
}

/// Log orphaned containers cleanup
pub fn audit_orphaned_cleanup(trigger: AuditTrigger, result: AuditResult, details: String) {
    info!(
        target: "audit",
        action = %AuditAction::OrphanedContainersCleanup,
        details = %details,
        trigger = %trigger,
        result = %result,
        "Orphaned cleanup"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_display() {
        assert_eq!(
            format!("{}", AuditAction::SessionDeleted),
            "SESSION_DELETED"
        );
        assert_eq!(
            format!("{}", AuditAction::SessionStopped),
            "SESSION_STOPPED"
        );
        assert_eq!(
            format!("{}", AuditAction::SessionResumed),
            "SESSION_RESUMED"
        );
        assert_eq!(format!("{}", AuditAction::ConfigSaved), "CONFIG_SAVED");
    }

    #[test]
    fn test_result_display() {
        assert_eq!(format!("{}", AuditResult::Success), "success");
        assert_eq!(
            format!("{}", AuditResult::Failed("oops".into())),
            "failed: oops"
        );
    }

    #[test]
    fn test_trigger_display() {
        assert_eq!(
            format!("{}", AuditTrigger::UserKeypress("D".into())),
            "keypress:D"
        );
        assert_eq!(format!("{}", AuditTrigger::Automatic), "automatic");
    }
}
