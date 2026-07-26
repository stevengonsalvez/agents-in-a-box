//! The daemon's inbox aggregator — the writer that turns the live event stream
//! into the durable inbox (e38.14).
//!
//! [`crate::events::EventBroker`] has fanned typed [`HangarEvent`]s from the
//! daemon's mutation paths to subscribed plugins since e38.2, but those events
//! were *purely live*: a plugin that was not attached when an event fired never
//! saw it, and there was no unread count or mark-read. This module closes that
//! gap. It subscribes to the broker exactly like an RPC connection does, maps
//! each issue / comment / task [`HangarEvent`] to one [`NewInboxEntry`], and
//! writes it to the `inbox_entry` table (migration 0021) so the inbox screen and
//! the `hangar/inbox_list` RPC read a durable aggregate.
//!
//! ## Take-effect seam
//!
//! [`spawn`] wires this into the live daemon: `boot()` spawns it alongside the
//! RPC server and the sweepers, handing it a fresh [`broker.subscribe()`] receiver
//! and the shared pool. From then on every committed mutation that emits an
//! issue/comment/task event ALSO lands an inbox row — the aggregation is not a
//! schema waiting for a writer, it is the writer.
//!
//! ## Which events aggregate
//!
//! Only the three families the inbox is *about* produce a row: issue lifecycle
//! (`IssueCreated` / `IssueUpdated` / `IssueDeleted`), comments (`CommentAdded`),
//! and task lifecycle (`TaskQueued` / `TaskStarted` / `TaskFinished`). The
//! high-frequency, low-signal events — `TaskProgress` heartbeats, per-line
//! `TaskMessage`, `AgentPresence`, autopilot/workspace/skill housekeeping — are
//! deliberately NOT aggregated: an inbox is a digest, not a transcript firehose.
//! [`entry_for_event`] returns `None` for those, so they pass through silently.
//!
//! ## Delivery semantics
//!
//! Best-effort, mirroring the broker: a write fault is logged and dropped, never
//! propagated (a failed inbox write must never down the daemon or lose the live
//! event the plugin already received). A lagged receiver (a slow aggregator that
//! fell behind the broadcast capacity) skips the dropped events and keeps going —
//! the next snapshot pull reconciles.

use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::inbox::{InboxKind, InboxRepo, NewInboxEntry};
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::events::ScopedEvent;

/// The flattened inbox shape derived from one [`HangarEvent`]: the kind family,
/// the wire event token, the subject id, and a short human summary line.
///
/// Pure data so [`entry_for_event`] is unit-testable without a pool — the mapping
/// is the interesting logic; the insert is mechanical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxFields {
    /// The entity family the entry is about.
    pub kind: InboxKind,
    /// The wire event discriminant (e.g. `issue_created`).
    pub event: &'static str,
    /// The id of the issue / comment / task the entry addresses.
    pub subject_id: String,
    /// A short pre-rendered line for the list row.
    pub summary: String,
}

/// Map one [`HangarEvent`] to its aggregated inbox fields, or `None` when the
/// event is not one the inbox aggregates (heartbeats, transcript lines, presence,
/// housekeeping).
///
/// This is the whole policy of *what lands in the inbox* — kept pure + total so a
/// new event variant is a compile error to ignore (the match is exhaustive) and
/// the digest-not-firehose choice is reviewable in one place.
#[must_use]
pub fn entry_for_event(event: &HangarEvent) -> Option<InboxFields> {
    match event {
        HangarEvent::IssueCreated(row) => Some(InboxFields {
            kind: InboxKind::Issue,
            event: "issue_created",
            subject_id: row.id.as_str().to_string(),
            summary: format!("New issue: {}", row.title),
        }),
        HangarEvent::IssueUpdated(row) => Some(InboxFields {
            kind: InboxKind::Issue,
            event: "issue_updated",
            subject_id: row.id.as_str().to_string(),
            summary: format!("Issue updated: {}", row.title),
        }),
        HangarEvent::IssueDeleted { issue_id } => Some(InboxFields {
            kind: InboxKind::Issue,
            event: "issue_deleted",
            subject_id: issue_id.as_str().to_string(),
            summary: format!("Issue deleted: {}", issue_id.as_str()),
        }),
        HangarEvent::CommentAdded(row) => Some(InboxFields {
            kind: InboxKind::Comment,
            event: "comment_added",
            subject_id: row.id.as_str().to_string(),
            summary: format!("New comment on {}", row.issue_id.as_str()),
        }),
        HangarEvent::TaskQueued { task_id, .. } => Some(InboxFields {
            kind: InboxKind::Task,
            event: "task_queued",
            subject_id: task_id.as_str().to_string(),
            summary: format!("Task queued: {}", task_id.as_str()),
        }),
        HangarEvent::TaskStarted { task_id, .. } => Some(InboxFields {
            kind: InboxKind::Task,
            event: "task_started",
            subject_id: task_id.as_str().to_string(),
            summary: format!("Task started: {}", task_id.as_str()),
        }),
        HangarEvent::TaskFinished {
            task_id, result, ..
        } => Some(InboxFields {
            kind: InboxKind::Task,
            event: "task_finished",
            subject_id: task_id.as_str().to_string(),
            summary: format!("Task finished ({result:?}): {}", task_id.as_str()),
        }),
        // Digest, not firehose: heartbeats, transcript lines, presence, and
        // autopilot/workspace/skill housekeeping do not land an inbox row.
        HangarEvent::TaskProgress { .. }
        | HangarEvent::TaskMessage { .. }
        | HangarEvent::AgentPresence { .. }
        | HangarEvent::SkillUpdated { .. }
        | HangarEvent::AutopilotUpdated(_)
        | HangarEvent::AutopilotRunChanged { .. }
        // Attention events (spec P2) have their own durable `attention` table and
        // their own control-centre surfaces; they never land in the workspace
        // notification digest.
        | HangarEvent::AttentionRaised { .. }
        | HangarEvent::AttentionAnswered { .. }
        | HangarEvent::WorkspaceChanged { .. } => None,
    }
}

/// Aggregate one scoped event into the inbox: map it, mint an id, insert.
///
/// Returns `Ok(true)` when a row landed, `Ok(false)` when the event is not one
/// the inbox aggregates. A store fault propagates so the caller can log it (the
/// caller never lets it down the daemon).
async fn aggregate_one(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    now_ms: i64,
    scoped: &ScopedEvent,
) -> Result<bool, sqlx::Error> {
    let Some(fields) = entry_for_event(&scoped.event) else {
        return Ok(false);
    };
    InboxRepo::insert(
        pool,
        &NewInboxEntry {
            id: idgen.new_ulid(),
            workspace_id: scoped.workspace_id.clone(),
            kind: fields.kind,
            event: fields.event.to_string(),
            subject_id: fields.subject_id,
            summary: fields.summary,
            created_at: now_ms,
        },
    )
    .await?;
    Ok(true)
}

/// Spawn the inbox aggregator: drain `rx` forever, writing every aggregatable
/// event into the inbox table.
///
/// This is the take-effect wiring (`boot()` calls it with `broker.subscribe()`).
/// Each event is mapped + inserted best-effort: a write fault is logged and
/// dropped (a failed inbox write never downs the daemon or loses the live event),
/// and a lagged receiver skips the dropped span and keeps going. The returned
/// [`JoinHandle`] is dropped by `boot()` (process exit tears the task down,
/// mirroring the sweepers); a future supervisor can keep it to stop cleanly.
#[must_use]
pub fn spawn(
    pool: SqlitePool,
    mut rx: broadcast::Receiver<ScopedEvent>,
) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    use ainb_hangar_core::idgen::SystemIdGen;

    tokio::spawn(async move {
        let idgen = SystemIdGen;
        let clock = SystemClock;
        loop {
            match rx.recv().await {
                Ok(scoped) => {
                    if let Err(e) = aggregate_one(&pool, &idgen, clock.now_ms(), &scoped).await {
                        tracing::warn!(error = %e, "inbox aggregate write failed");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "inbox aggregator lagged; events dropped");
                }
                // The broker (and every sink) was dropped: the daemon is shutting
                // down. Exit the task cleanly.
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::ids::{CommentId, IssueId, TaskId};
    use ainb_hangar_proto::events::{CommentRow, IssueRow, MessageKind, TaskResult};

    fn issue_row(id: &str, title: &str) -> IssueRow {
        IssueRow {
            id: IssueId::from_str(id).unwrap(),
            display_id: None,
            workspace_id: "ws-a".into(),
            title: title.into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: "member:u1".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn issue_created_aggregates_with_title_summary() {
        let f = entry_for_event(&HangarEvent::IssueCreated(issue_row("issue-1", "Fix it")))
            .expect("issue_created aggregates");
        assert_eq!(f.kind, InboxKind::Issue);
        assert_eq!(f.event, "issue_created");
        assert_eq!(f.subject_id, "issue-1");
        assert!(f.summary.contains("Fix it"));
    }

    #[test]
    fn comment_added_aggregates_as_comment_kind() {
        let row = CommentRow {
            id: CommentId::from_str("c-1").unwrap(),
            issue_id: IssueId::from_str("issue-1").unwrap(),
            author: "member:u1".into(),
            body: "lgtm".into(),
            created_at: 0,
        };
        let f = entry_for_event(&HangarEvent::CommentAdded(row)).expect("comment aggregates");
        assert_eq!(f.kind, InboxKind::Comment);
        assert_eq!(f.event, "comment_added");
        assert_eq!(f.subject_id, "c-1");
    }

    #[test]
    fn task_finished_aggregates_as_task_kind() {
        let f = entry_for_event(&HangarEvent::TaskFinished {
            task_id: TaskId::from_str("t-1").unwrap(),
            result: TaskResult::Success,
            ended_at: chrono::DateTime::from_timestamp_millis(0).unwrap(),
        })
        .expect("task_finished aggregates");
        assert_eq!(f.kind, InboxKind::Task);
        assert_eq!(f.event, "task_finished");
        assert_eq!(f.subject_id, "t-1");
    }

    #[test]
    fn heartbeats_and_transcript_lines_do_not_aggregate() {
        // A progress heartbeat is not a notification.
        assert!(
            entry_for_event(&HangarEvent::TaskProgress {
                task_id: TaskId::from_str("t-1").unwrap(),
                tool_calls: 3,
                elapsed_ms: 100,
            })
            .is_none()
        );
        // A per-line transcript message is not a notification.
        assert!(
            entry_for_event(&HangarEvent::TaskMessage {
                task_id: TaskId::from_str("t-1").unwrap(),
                kind: MessageKind::Agent,
                body: "thinking".into(),
            })
            .is_none()
        );
    }
}
