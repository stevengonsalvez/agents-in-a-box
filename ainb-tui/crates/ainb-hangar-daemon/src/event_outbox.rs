//! The daemon's event-outbox drain — the writer that persists every emitted
//! [`HangarEvent`] into the durable `event_log` outbox (T1 / architecture
//! §4.1–§4.2).
//!
//! [`crate::events::EventBroker`] has fanned typed events to live subscribers
//! since e38.2, and [`crate::inbox_aggregator`] folds the three notification
//! families into the read/unread inbox DIGEST. Neither is a replayable log: a
//! subscriber that was absent while events fired (late join), or that dropped
//! its socket (daemon restart), had no way to catch up. This module closes that
//! gap — it drains the broker's LOSSLESS outbox channel and appends every event
//! to `event_log` (migration 0024) with a monotonic `seq`, so a reconnecting
//! subscriber resumes from exactly where it left off.
//!
//! ## Lossless, unlike the inbox aggregator
//!
//! The inbox aggregator subscribes to the broker's [`tokio::sync::broadcast`]
//! channel, which DROPS events for a lagged consumer — acceptable for a digest
//! that the next snapshot reconciles. The outbox must never gap (a missing `seq`
//! would silently truncate a replay), so it consumes the broker's dedicated
//! unbounded [`mpsc`](tokio::sync::mpsc) channel instead: emission never blocks a
//! mutation path AND never drops an event.
//!
//! ## Which events persist
//!
//! ALL of them. Where the inbox is a curated digest, the outbox is the RAW log:
//! [`outbox_fields`] is total over [`HangarEvent`], so every variant — including
//! the high-frequency `TaskProgress` / `TaskMessage` heartbeats — lands a row.
//! A new variant is a compile error to ignore.
//!
//! ## Delivery semantics
//!
//! Best-effort at the storage boundary, mirroring the broker: a write fault is
//! logged and dropped, never propagated (a failed outbox write must never down
//! the daemon or lose the live event the plugin already received). The only loss
//! window is a hard crash between emit and the drain's commit — inherent to any
//! async persistence and acceptable for a laptop control plane; a reconnecting
//! subscriber still resumes from everything that did commit.

use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::event_log::{EventOutboxRepo, NewEvent};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::events::ScopedEvent;

/// The durable-log columns derived from one [`HangarEvent`]: the wire event
/// discriminant (`event_type`) and the id of the subject it is about
/// (`entity`), or `None` for a variant with no single subject.
///
/// Kept pure + total so it is unit-testable without a pool and a new event
/// variant is a compile error to ignore. The returned `event_type` MUST match
/// the event's serde tag (the `event` field of its JSON), so the stored `type`
/// column and `payload` never disagree — [`outbox_type_matches_serde_tag`]
/// pins this in a test.
#[must_use]
pub fn outbox_fields(event: &HangarEvent) -> (&'static str, Option<String>) {
    match event {
        HangarEvent::IssueCreated(row) => ("issue_created", Some(row.id.as_str().to_string())),
        HangarEvent::IssueUpdated(row) => ("issue_updated", Some(row.id.as_str().to_string())),
        HangarEvent::IssueDeleted { issue_id } => {
            ("issue_deleted", Some(issue_id.as_str().to_string()))
        }
        HangarEvent::TaskQueued { task_id, .. } => {
            ("task_queued", Some(task_id.as_str().to_string()))
        }
        HangarEvent::TaskStarted { task_id, .. } => {
            ("task_started", Some(task_id.as_str().to_string()))
        }
        HangarEvent::TaskProgress { task_id, .. } => {
            ("task_progress", Some(task_id.as_str().to_string()))
        }
        HangarEvent::TaskMessage { task_id, .. } => {
            ("task_message", Some(task_id.as_str().to_string()))
        }
        HangarEvent::TaskFinished { task_id, .. } => {
            ("task_finished", Some(task_id.as_str().to_string()))
        }
        HangarEvent::CommentAdded(row) => ("comment_added", Some(row.id.as_str().to_string())),
        HangarEvent::AgentPresence { agent_id, .. } => {
            ("agent_presence", Some(agent_id.as_str().to_string()))
        }
        HangarEvent::SkillUpdated { skill, .. } => ("skill_updated", Some(skill.clone())),
        HangarEvent::AutopilotUpdated(row) => ("autopilot_updated", Some(row.id.clone())),
        HangarEvent::AutopilotRunChanged { autopilot_id, .. } => {
            ("autopilot_run_changed", Some(autopilot_id.clone()))
        }
        HangarEvent::WorkspaceChanged { to, .. } => ("workspace_changed", Some(to.clone())),
    }
}

/// Persist one scoped event into the durable outbox: derive its columns,
/// serialise the payload verbatim, append. Returns the assigned monotonic
/// `seq`. A store fault propagates so the caller can log it (the caller never
/// lets it down the daemon).
async fn persist_one(
    pool: &SqlitePool,
    now_ms: i64,
    scoped: &ScopedEvent,
) -> Result<i64, sqlx::Error> {
    let (event_type, entity) = outbox_fields(&scoped.event);
    // The payload is the exact JSON that was (or will be) the notification's
    // `params`, so the replay path re-frames it verbatim. A serialise fault is
    // impossible for these serde-derived types; fall back to an empty object so
    // a row still lands rather than losing the seq.
    let payload = serde_json::to_string(&scoped.event).unwrap_or_else(|_| "{}".to_string());
    EventOutboxRepo::append(
        pool,
        &NewEvent {
            workspace_id: scoped.workspace_id.clone(),
            event_type: event_type.to_string(),
            entity,
            payload,
            ts: now_ms,
        },
    )
    .await
}

/// Spawn the outbox drain: pull every event off the broker's lossless outbox
/// channel forever, appending each to `event_log` with a monotonic `seq`.
///
/// This is the take-effect wiring (`boot()` calls it with the receiver from
/// [`crate::events::EventBroker::with_outbox`]). Each event is appended
/// best-effort: a write fault is logged and dropped (a failed outbox write never
/// downs the daemon or loses the live event). The channel closing (every sink
/// dropped at shutdown) exits the task cleanly. The returned [`JoinHandle`] is
/// dropped by `boot()` (process exit tears the task down, mirroring the inbox
/// aggregator); a future supervisor can keep it to stop cleanly.
///
/// [`JoinHandle`]: tokio::task::JoinHandle
#[must_use]
pub fn spawn(
    pool: SqlitePool,
    mut rx: mpsc::UnboundedReceiver<ScopedEvent>,
) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};

    tokio::spawn(async move {
        let clock = SystemClock;
        while let Some(scoped) = rx.recv().await {
            if let Err(e) = persist_one(&pool, clock.now_ms(), &scoped).await {
                tracing::warn!(error = %e, "event outbox append failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::ids::{AgentId, CommentId, IssueId, TaskId};
    use ainb_hangar_proto::events::{CommentRow, IssueRow, MessageKind, PresenceState, TaskResult};
    use ainb_hangar_store::Store;

    fn issue_row(id: &str) -> IssueRow {
        IssueRow {
            id: IssueId::from_str(id).unwrap(),
            display_id: None,
            workspace_id: "ws-a".into(),
            title: "Fix it".into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: "member:u1".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        }
    }

    fn task_finished(id: &str) -> HangarEvent {
        HangarEvent::TaskFinished {
            task_id: TaskId::from_str(id.to_string()).unwrap(),
            result: TaskResult::Success,
            ended_at: chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        }
    }

    /// The `event_type` returned by [`outbox_fields`] must equal the serde tag in
    /// the serialised event, for EVERY variant — otherwise the stored `type`
    /// column and `payload` would disagree and a `type`-filtered read would lie.
    #[test]
    fn outbox_type_matches_serde_tag() {
        let samples = vec![
            HangarEvent::IssueCreated(issue_row("i1")),
            HangarEvent::IssueUpdated(issue_row("i1")),
            HangarEvent::IssueDeleted {
                issue_id: IssueId::from_str("i1").unwrap(),
            },
            HangarEvent::TaskQueued {
                task_id: TaskId::from_str("t1").unwrap(),
                issue_id: IssueId::from_str("i1").unwrap(),
                agent_id: AgentId::from_str("a1").unwrap(),
            },
            HangarEvent::TaskStarted {
                task_id: TaskId::from_str("t1").unwrap(),
                started_at: chrono::DateTime::from_timestamp_millis(0).unwrap(),
            },
            HangarEvent::TaskProgress {
                task_id: TaskId::from_str("t1").unwrap(),
                tool_calls: 1,
                elapsed_ms: 2,
            },
            HangarEvent::TaskMessage {
                task_id: TaskId::from_str("t1").unwrap(),
                kind: MessageKind::Agent,
                body: "hi".into(),
            },
            task_finished("t1"),
            HangarEvent::CommentAdded(CommentRow {
                id: CommentId::from_str("c1").unwrap(),
                issue_id: IssueId::from_str("i1").unwrap(),
                author: "member:u1".into(),
                body: "lgtm".into(),
                created_at: 0,
            }),
            HangarEvent::AgentPresence {
                agent_id: AgentId::from_str("a1").unwrap(),
                state: PresenceState::Online,
            },
            HangarEvent::SkillUpdated {
                skill: "commit".into(),
                updated_at: 0,
            },
            HangarEvent::WorkspaceChanged {
                from: None,
                to: "ws-b".into(),
            },
        ];
        for event in &samples {
            let (event_type, _entity) = outbox_fields(event);
            let json = serde_json::to_value(event).unwrap();
            assert_eq!(
                json["event"], event_type,
                "outbox_fields type must match the serde tag for {event:?}"
            );
        }
    }

    /// The drain persists an emitted event into `event_log` with a monotonic
    /// `seq`, and the stored payload round-trips back to the same event.
    #[tokio::test]
    async fn drain_persists_emitted_events_with_monotonic_seq() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-a")
            .bind("ws-a")
            .bind("ws-a")
            .bind(1_000_i64)
            .execute(store.pool())
            .await
            .unwrap();

        let (broker, outbox_rx) = crate::events::EventBroker::with_outbox();
        let handle = spawn(store.pool().clone(), outbox_rx);

        let sink = broker.sink();
        sink.emit("ws-a", task_finished("t1"));
        sink.emit("ws-a", task_finished("t2"));
        // Dropping every sink closes the channel so the drain exits once it has
        // persisted the queued events — a deterministic join, no sleeps.
        drop(sink);
        drop(broker);
        handle.await.unwrap();

        let rows = EventOutboxRepo::replay(store.pool(), "ws-a", 0, 100).await.unwrap();
        assert_eq!(rows.len(), 2, "both emitted events persisted");
        assert!(rows[1].seq > rows[0].seq, "seq is monotonic");
        assert_eq!(rows[0].event_type, "task_finished");
        assert_eq!(rows[0].entity.as_deref(), Some("t1"));
        // The payload round-trips to the original typed event.
        let decoded: HangarEvent = serde_json::from_str(&rows[1].payload).unwrap();
        assert_eq!(decoded, task_finished("t2"));
    }
}
