//! Durable issue-comment emission at run-loop lifecycle checkpoints (e38.6).
//!
//! A running agent streams its work to the live transcript (the runner tees the
//! provider's JSONL to `logs/<provider>.jsonl`), but that buffer is bounded and
//! rolls — once it overflows, the record of *what the agent did* is gone. The
//! issue's `comment` thread (e38.5) is the durable surface: a row in the
//! `comment` table outlives any transcript window.
//!
//! This module bridges the two. At each FSM checkpoint the run loop reaches —
//! the task starts running, and the run finishes (success / failure / timeout) —
//! it calls [`emit_checkpoint`], which writes one **system-authored** comment to
//! the task's issue so the activity is recorded permanently in the thread the
//! UI already renders.
//!
//! # Authorship
//!
//! The comment is authored as the **running agent** (`agent:<agent_id>`), so it
//! shows in the issue's comment thread attributed to the actor that did the
//! work — exactly where a user looking at the issue expects to see it.
//!
//! # Scoping
//!
//! Only a task bound to an issue (`issue_id = Some`) gets comments — a chat /
//! autopilot placeholder task carries a `NULL` issue (see
//! [`Task::issue_id`](ainb_hangar_store::repo::task::Task::issue_id)) and is
//! skipped silently. The write is workspace-scoped through [`CommentRepo`]'s
//! join to `issue`, so a comment can only land on an issue in the task's own
//! tenant.
//!
//! # Best-effort
//!
//! A comment is *observability*, never part of the task FSM. Every fault here
//! (the issue vanished, a store error, a malformed agent id) is logged and
//! swallowed — [`emit_checkpoint`] returns `()` and never propagates, so a
//! comment-write fault can never block or fail the task transition that
//! triggered it. The FSM write has already committed by the time we comment.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::repo::comment::{CommentRepo, NewComment};
use ainb_hangar_store::repo::task::Task;
use ainb_hangar_store::service::fail::FailureReason;
use sqlx::SqlitePool;

/// A run-loop lifecycle point that emits one durable issue comment (e38.6).
///
/// The run loop drives `dispatched -> running -> done|failed`; a comment is
/// written when the run *starts* ([`Self::Started`]) and when it reaches a
/// *terminal* outcome ([`Self::Succeeded`] / [`Self::Failed`]). A terminal
/// failure carries its [`FailureReason`] so the comment records *why* the run
/// stopped, not merely that it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    /// The task transitioned `dispatched -> running`: the agent began work.
    Started,
    /// The run completed cleanly (the provider exited 0).
    Succeeded,
    /// The run reached a terminal failure; `reason` is the FSM failure reason
    /// recorded on the row (carried into the blocker comment body).
    Failed {
        /// Why the run failed (e.g. timeout, agent error, runtime offline).
        reason: FailureReason,
    },
}

impl Checkpoint {
    /// The comment body this checkpoint posts to the issue thread.
    ///
    /// Bodies are short, human-readable, and stable; a failure body carries the
    /// [`FailureReason::as_db_str`] token so the thread records the cause.
    fn body(self) -> String {
        match self {
            Self::Started => "Agent started working on this issue.".to_string(),
            Self::Succeeded => "Agent completed the run successfully.".to_string(),
            Self::Failed { reason } => {
                format!(
                    "Agent run blocked — stopped with failure reason: {}.",
                    reason.as_db_str()
                )
            }
        }
    }
}

/// Emit one durable, agent-authored comment to `task`'s issue for `checkpoint`.
///
/// A no-op (returns immediately) when:
/// - the task carries no issue (`issue_id = None`) — a chat / autopilot task; or
/// - the agent id is malformed (cannot form an `agent:<id>` author).
///
/// Otherwise it mints a fresh comment id, stamps `created_at` from `clock`, and
/// inserts through [`CommentRepo`] (which scopes the write to the task's
/// workspace via its join to `issue`). **Best-effort**: a store fault — or a
/// `false` "landed nothing" return because the issue vanished or belongs to
/// another tenant — is logged at `warn` and swallowed. This function never
/// returns an error and never panics; a comment failure must not block the task
/// FSM that called it.
pub async fn emit_checkpoint(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    task: &Task,
    checkpoint: Checkpoint,
) {
    // A chat / autopilot placeholder task carries no issue — nothing to comment
    // on. Skip silently (the common, expected case).
    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };

    // Author as the running agent so the comment shows attributed to the actor
    // that did the work. A malformed agent id is logged and skipped — never a
    // reason to disturb the FSM.
    let author = match ActorRef::new(ActorKind::Agent, task.agent_id.clone()) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                task_id = %task.id,
                agent_id = %task.agent_id,
                error = %e,
                "progress comment: malformed agent id; skipping"
            );
            return;
        }
    };

    let comment = NewComment {
        id: idgen.new_ulid(),
        issue_id: issue_id.to_string(),
        author,
        body: checkpoint.body(),
        created_at: clock.now_ms(),
    };

    match CommentRepo::insert(pool, &task.workspace_id, &comment).await {
        Ok(true) => {
            tracing::debug!(
                task_id = %task.id,
                issue_id = %issue_id,
                checkpoint = ?checkpoint,
                "progress comment written"
            );
        }
        Ok(false) => {
            // The (issue_id, workspace_id) pair resolved to no issue: the issue
            // was deleted, or never belonged to this tenant. Nothing landed —
            // log it, but it is not an FSM fault.
            tracing::warn!(
                task_id = %task.id,
                issue_id = %issue_id,
                "progress comment: issue not found in workspace; nothing written"
            );
        }
        Err(e) => {
            tracing::warn!(
                task_id = %task.id,
                issue_id = %issue_id,
                error = %e,
                "progress comment write failed; continuing (best-effort)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_body_announces_the_start() {
        assert!(Checkpoint::Started.body().to_lowercase().contains("start"));
    }

    #[test]
    fn success_body_announces_completion() {
        let body = Checkpoint::Succeeded.body().to_lowercase();
        assert!(body.contains("complete") || body.contains("success"));
    }

    #[test]
    fn failure_body_carries_the_reason_token() {
        for reason in [
            FailureReason::Timeout,
            FailureReason::AgentError,
            FailureReason::RuntimeOffline,
        ] {
            let body = Checkpoint::Failed { reason }.body();
            assert!(
                body.contains(reason.as_db_str()),
                "body {body:?} must carry reason token {:?}",
                reason.as_db_str()
            );
        }
    }
}
