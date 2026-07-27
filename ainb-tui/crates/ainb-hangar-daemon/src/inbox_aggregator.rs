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

use ainb_hangar_core::actor::{ActorKind, ActorRef, local_member};
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::inbox::{InboxKind, InboxRepo, NewInboxEntry};
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::issue_subscriber::IssueSubscriberRepo;
use ainb_hangar_store::repo::member::MemberRepo;
use ainb_hangar_store::repo::task::TaskRepo;
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

/// Parse a stored/wire actor token (`member:<id>` / `agent:<id>`), dropping a
/// malformed one rather than addressing a notification to nobody.
fn actor(raw: &str) -> Option<ActorRef> {
    raw.parse::<ActorRef>().ok()
}

/// The last-resort recipient for an event with no derivable participant: the
/// workspace's OWNER member, else the local human.
///
/// Mirrors the reference's guarantee that a notification is never dropped on the
/// floor — an event we cannot attribute still lands *somewhere* a human reads,
/// rather than vanishing.
async fn fallback_recipient(pool: &SqlitePool, workspace_id: &str) -> ActorRef {
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return local_member();
    };
    let members = MemberRepo::list(pool, &ws).await.unwrap_or_default();
    members
        .iter()
        .find(|m| m.role == "owner")
        .and_then(|m| ActorRef::new(ActorKind::Member, m.user_id.clone()).ok())
        .unwrap_or_else(local_member)
}

/// The issue's SUBSCRIBER set (multica parity #22, migration 0062), falling back
/// to the participant derivation when the issue has no subscriber rows at all.
///
/// The fallback is what makes the conversion upgrade-safe: an issue written by a
/// path that predates the auto-subscribe writers (or one whose backfill found
/// nothing) still notifies its creator and assignee, so no upgrade can silence a
/// notification. Once ANY row exists the table is authoritative — that is how an
/// unsubscribe actually takes effect.
async fn issue_subscribers_or_participants(pool: &SqlitePool, issue_id: &str) -> Vec<ActorRef> {
    match IssueSubscriberRepo::actors(pool, issue_id).await {
        Ok(subs) if !subs.is_empty() => subs,
        Ok(_) => issue_participants(pool, issue_id).await,
        Err(e) => {
            tracing::warn!(error = %e, issue_id, "inbox subscriber lookup failed");
            issue_participants(pool, issue_id).await
        }
    }
}

/// The participants of an issue: its creator plus its assignee (when set).
async fn issue_participants(pool: &SqlitePool, issue_id: &str) -> Vec<ActorRef> {
    match IssueRepo::get_by_id(pool, issue_id).await {
        Ok(Some(issue)) => {
            let mut out = vec![issue.creator];
            out.extend(issue.assignee);
            out
        }
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, issue_id, "inbox recipient lookup failed");
            Vec::new()
        }
    }
}

/// Who an aggregated event is FOR — the reference's `notifySubscribers`
/// (subscribers minus the actor) plus `notifyDirect` (a targeted actor),
/// expressed against hangar's schema.
///
/// Since migration 0062 (multica parity #22) "subscribers" is a REAL
/// `issue_subscriber` read, not the participant approximation the previous
/// revision documented: a watcher who is neither creator nor assignee now hears
/// about a comment, and an actor who unsubscribed does not. You are still never
/// notified of your own action (multica `notification_listeners.go:316`). An
/// event with no derivable recipient falls back to the workspace OWNER, then to
/// [`local_member`], so a notification is never silently dropped.
async fn recipients_for(pool: &SqlitePool, scoped: &ScopedEvent) -> Vec<ActorRef> {
    let ws = scoped.workspace_id.as_str();
    let mut out: Vec<ActorRef> = match &scoped.event {
        // The creator caused it, so the ASSIGNEE hears about it. An unassigned
        // issue lands in its creator's own tracking inbox (the single-user flow),
        // which is the only way a solo human sees their own board move.
        HangarEvent::IssueCreated(row) => {
            let creator = actor(&row.creator);
            match row.assignee.as_deref().and_then(actor) {
                Some(a) if Some(&a) != creator.as_ref() => vec![a],
                _ => creator.into_iter().collect(),
            }
        }
        // The wire event carries no mutator, so nobody can be excluded: both
        // participants hear about the edit.
        HangarEvent::IssueUpdated(row) => {
            let mut v: Vec<ActorRef> = actor(&row.creator).into_iter().collect();
            v.extend(row.assignee.as_deref().and_then(actor));
            v
        }
        // The row is already gone; there is nothing left to look up.
        HangarEvent::IssueDeleted { .. } => Vec::new(),
        // The comment's participants minus its author — the closest hangar has
        // to `notifySubscribers`.
        HangarEvent::CommentAdded(row) => {
            let author = actor(&row.author);
            let watchers = issue_subscribers_or_participants(pool, row.issue_id.as_str()).await;
            watchers.into_iter().filter(|p| Some(p) != author.as_ref()).collect()
        }
        // `notifyDirect` with an AGENT recipient: the agent's own work queue.
        // This is the row that proves an agent has an inbox of its own.
        HangarEvent::TaskQueued { agent_id, .. } => {
            ActorRef::new(ActorKind::Agent, agent_id.as_str()).into_iter().collect()
        }
        // "Your task started / finished": the issue's creator hears it; a task
        // with no issue reports back to its own agent.
        HangarEvent::TaskStarted { task_id, .. } | HangarEvent::TaskFinished { task_id, .. } => {
            task_recipients(pool, task_id.as_str()).await
        }
        _ => Vec::new(),
    };
    // Dedupe by canonical form, preserving order.
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert(a.to_string()));
    if out.is_empty() {
        out.push(fallback_recipient(pool, ws).await);
    }
    out
}

/// Recipients for a task lifecycle event: the originating issue's creator, or
/// the executing agent when the task has no issue.
async fn task_recipients(pool: &SqlitePool, task_id: &str) -> Vec<ActorRef> {
    let task = match TaskRepo::get_by_id(pool, task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, task_id, "inbox task recipient lookup failed");
            return Vec::new();
        }
    };
    if let Some(issue_id) = task.issue_id.as_deref() {
        match IssueRepo::get_by_id(pool, issue_id).await {
            Ok(Some(issue)) => return vec![issue.creator],
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, issue_id, "inbox issue lookup failed"),
        }
    }
    ActorRef::new(ActorKind::Agent, task.agent_id).into_iter().collect()
}

/// Aggregate one scoped event into the inbox: map it, resolve its recipients,
/// mint an id and insert ONE ROW PER RECIPIENT.
///
/// Returns `Ok(true)` when at least one row landed, `Ok(false)` when the event is
/// not one the inbox aggregates. A store fault propagates so the caller can log
/// it (the caller never lets it down the daemon).
async fn aggregate_one(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    now_ms: i64,
    scoped: &ScopedEvent,
) -> Result<bool, sqlx::Error> {
    let Some(fields) = entry_for_event(&scoped.event) else {
        return Ok(false);
    };
    for recipient in recipients_for(pool, scoped).await {
        InboxRepo::insert(
            pool,
            &NewInboxEntry {
                id: idgen.new_ulid(),
                workspace_id: scoped.workspace_id.clone(),
                recipient,
                kind: fields.kind,
                event: fields.event.to_string(),
                subject_id: fields.subject_id.clone(),
                summary: fields.summary.clone(),
                created_at: now_ms,
            },
        )
        .await?;
    }
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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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

    // --- Recipient fan-out (store-backed): who an event is FOR -------------

    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_core::ids::AgentId;
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::inbox::InboxRepo;

    /// Seed a workspace with an owner member and an issue created by
    /// `member:user-1` and assigned to `agent:a1`.
    ///
    /// No `agent` row is needed: the recipient columns are FK-less by design
    /// (migration 0060), exactly like every other polymorphic actor column.
    async fn seed(pool: &sqlx::SqlitePool) {
        for sql in [
            "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-a','ws-a','A',0)",
            "INSERT INTO user (id, email, created_at) VALUES ('user-1','o@example.com',0)",
            "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-a','user-1','owner')",
            "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, \
             assignee_type, assignee_id, created_at) \
             VALUES ('issue-1','ws-a','Fix it','open','member','user-1','agent','a1',0)",
        ] {
            sqlx::query(sql).execute(pool).await.expect(sql);
        }
    }

    fn scoped(event: HangarEvent) -> ScopedEvent {
        ScopedEvent {
            workspace_id: "ws-a".into(),
            event,
        }
    }

    fn comment_by(author: &str) -> HangarEvent {
        HangarEvent::CommentAdded(CommentRow {
            id: CommentId::from_str("c-1").unwrap(),
            issue_id: IssueId::from_str("issue-1").unwrap(),
            author: author.into(),
            body: "lgtm".into(),
            created_at: 0,
        })
    }

    /// Aggregate one event and return every `(recipient, subject_id)` that landed.
    async fn aggregate(store: &Store, event: HangarEvent) -> Vec<(String, String)> {
        let idgen = SystemIdGen;
        aggregate_one(store.pool(), &idgen, 1_000, &scoped(event))
            .await
            .expect("aggregate");
        let mut out = Vec::new();
        for recipient in [
            ActorRef::new(ActorKind::Member, "user-1").unwrap(),
            local_member(),
            ActorRef::new(ActorKind::Agent, "a1").unwrap(),
        ] {
            for e in InboxRepo::list(store.pool(), "ws-a", &recipient, 100).await.unwrap() {
                out.push((e.recipient.to_string(), e.subject_id));
            }
        }
        out
    }

    /// A comment BY the assigned agent notifies the issue's creator, never the
    /// agent that wrote it (self-notification suppressed).
    #[tokio::test]
    async fn comment_by_agent_targets_the_issue_creator_not_the_author() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed(store.pool()).await;

        let landed = aggregate(&store, comment_by("agent:a1")).await;
        assert_eq!(
            landed,
            vec![("member:user-1".to_string(), "c-1".to_string())],
            "exactly the creator, and only once"
        );
    }

    /// The mirror: a comment BY the human notifies the assigned agent, and the
    /// human gets nothing.
    #[tokio::test]
    async fn comment_by_member_targets_the_assignee_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed(store.pool()).await;

        let landed = aggregate(&store, comment_by("member:user-1")).await;
        assert_eq!(
            landed,
            vec![("agent:a1".to_string(), "c-1".to_string())],
            "exactly the assigned agent, never the author"
        );
    }

    /// A queued task lands in the AGENT's own inbox — the row that proves an
    /// agent is a first-class inbox recipient.
    #[tokio::test]
    async fn task_queued_lands_in_the_agents_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed(store.pool()).await;

        let landed = aggregate(
            &store,
            HangarEvent::TaskQueued {
                task_id: TaskId::from_str("t-1").unwrap(),
                issue_id: IssueId::from_str("issue-1").unwrap(),
                agent_id: AgentId::from_str("a1".to_string()).unwrap(),
            },
        )
        .await;
        assert_eq!(
            landed,
            vec![("agent:a1".to_string(), "t-1".to_string())],
            "the agent's own work queue"
        );
    }

    /// An event with no derivable participant (a deleted issue: the row is gone)
    /// still lands — on the workspace OWNER — rather than being dropped.
    #[tokio::test]
    async fn event_with_no_derivable_participant_falls_back_to_the_workspace_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed(store.pool()).await;

        let landed = aggregate(
            &store,
            HangarEvent::IssueDeleted {
                issue_id: IssueId::from_str("issue-gone").unwrap(),
            },
        )
        .await;
        assert_eq!(
            landed,
            vec![("member:user-1".to_string(), "issue-gone".to_string())],
            "the owner is the last-resort recipient, so nothing is dropped"
        );
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
