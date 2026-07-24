//! Child-done → parent cascade (migration 0046, multica parity).
//!
//! The **sqlite side** of the sub-issue cascade, called by BOTH the daemon (TUI
//! writes) and the CLI (`ainb hangar issue update`). Mirrors multica's
//! `issue_child_done.go`: when a sub-issue transitions non-terminal → terminal
//! AND that completion CLOSES its stage barrier, a system-style comment is posted
//! on the parent recording the finished child and the roll-up progress.
//!
//! What lives here vs. the daemon:
//! - **Here (store):** the transition + barrier guards, and the comment write —
//!   everything callable without a running daemon, so the CLI and the daemon share
//!   one implementation and the behaviour is unit-testable in-memory.
//! - **Daemon only:** waking the parent's assignee agent (`run_card`), because
//!   only the daemon owns the launch machinery. This service returns a
//!   [`ParentCascade`] describing WHO to wake; the daemon acts on it.
//!
//! # Why the comment is authored by a real actor, not `type='system'`
//!
//! The `comment` table's `author_type` CHECK is `member|agent` only (migration
//! 0003); adding a `system` author would need a full table rebuild (SQLite can't
//! `ALTER` a CHECK). We avoid that: the cascade comment is authored by the child's
//! completing actor (its `assignee` if set, else its `creator` — both guaranteed
//! `member|agent`) and identified by a distinctive body string, so no comment
//! schema change is needed.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use sqlx::SqlitePool;

use crate::repo::comment::{CommentRepo, NewComment};
use crate::repo::issue::{Issue, IssueRepo};

/// The outcome of a FIRED cascade, returned so the daemon can wake the parent.
///
/// A `None` from [`cascade_child_done`] means nothing fired (not a terminal
/// transition, no parent, a guard tripped, or the barrier is not closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentCascade {
    /// The parent issue that received the comment.
    pub parent_id: String,
    /// The parent's assignee, when it has one. `None` = unassigned parent (the
    /// comment was still posted, but there is no agent to wake). A `member`
    /// assignee never reaches here — the guard skips the whole cascade for a
    /// human-owned parent (there is no agent to trigger).
    pub parent_assignee: Option<ActorRef>,
    /// The id of the comment written on the parent.
    pub comment_id: String,
    /// The actor the cascade comment was authored as (the child's completing
    /// actor). Lets the daemon build a `CommentAdded` wire row without a re-read.
    pub comment_author: ActorRef,
    /// The cascade comment body (same distinctive string that was written).
    pub comment_body: String,
    /// How many of the parent's sub-issues are now terminal.
    pub children_done: i64,
    /// The parent's total sub-issue count.
    pub children_total: i64,
}

/// `true` when a state token is terminal for the child-done cascade — `done` OR
/// `cancelled` (mirrors multica's `isTerminalChildStatus`).
fn is_terminal(state: &str) -> bool {
    state == "done" || state == "cancelled"
}

/// Post the child-done comment on the parent iff `child_id`'s transition from
/// `prev_state` to `new_state` CLOSES a stage barrier (migration 0046).
///
/// Returns `Ok(Some(cascade))` when a comment was written, `Ok(None)` when the
/// cascade did not fire. The guards, in multica order:
/// 1. the transition must be *into* terminal (`!is_terminal(prev) &&
///    is_terminal(new)`) — a repeat terminal save never re-fires;
/// 2. the child must have a `parent_issue_id`, and the parent must resolve in the
///    same workspace;
/// 3. the parent must not already be `done`/`cancelled`, must not be `backlog` (a
///    parked parent stays inert), and its assignee must not be a human `member`
///    (no agent to trigger — an *unassigned* parent still gets the comment);
/// 4. the completion must close the barrier: for an unstaged sibling set, every
///    child is terminal; for a staged set, every staged sibling at-or-below the
///    completing child's stage is terminal (frontier closure; unstaged siblings
///    ignored).
///
/// `now_ms` / `new_id` are injected so the write is deterministic under test.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on any store fault. A caller that wants best-effort
/// semantics swallows the error itself; this returns `Result` so tests can assert.
pub async fn cascade_child_done(
    pool: &SqlitePool,
    workspace_id: &str,
    child_id: &str,
    prev_state: &str,
    new_state: &str,
    now_ms: i64,
    new_id: String,
) -> Result<Option<ParentCascade>, sqlx::Error> {
    // 1. Only a non-terminal → terminal transition fires. A repeat `done` save,
    //    or a move between two non-terminal states, is a no-op.
    if is_terminal(prev_state) || !is_terminal(new_state) {
        return Ok(None);
    }

    // 2. The child must be a sub-issue; load it and its parent (workspace-scoped).
    let Some(child) = IssueRepo::get_by_id(pool, child_id).await? else {
        return Ok(None);
    };
    let Some(parent_id) = child.parent_issue_id.clone() else {
        return Ok(None);
    };
    let Some(parent) = IssueRepo::get_by_id(pool, &parent_id).await? else {
        return Ok(None);
    };
    // Tenant guard: the parent must live in the same workspace as the caller's
    // scope. A cross-workspace parent link (which the create path forbids) never
    // cascades.
    if parent.workspace_id != workspace_id {
        return Ok(None);
    }

    // 3. Parent-state guards.
    //    - already terminal: nothing to wake, no progress to record.
    //    - `backlog`: a parked parent stays inert (multica MUL-3497/#4320). Hangar
    //      has no `backlog` issue state today, so this is a defensive no-op that
    //      keeps parity if one is ever added.
    if is_terminal(&parent.state) || parent.state == "backlog" {
        return Ok(None);
    }
    //    - a human `member` parent has no agent to trigger → skip the whole
    //      cascade (multica MUL-2538). An *unassigned* parent is NOT skipped.
    if matches!(
        parent.assignee.as_ref().map(ActorRef::kind),
        Some(ActorKind::Member)
    ) {
        return Ok(None);
    }

    // 4. Stage barrier: only fire when this completion closes the lowest unfinished
    //    stage.
    let children = IssueRepo::list_children(pool, &parent_id).await?;
    if !stage_barrier_closed(&children, &child) {
        return Ok(None);
    }

    // 5. Comment author = the child's completing actor (assignee, else creator);
    //    both are guaranteed `member|agent`, so no `system` author is needed.
    let author = child.assignee.clone().unwrap_or_else(|| child.creator.clone());

    // 6. Roll-up counts + distinctive, queryable body.
    let (done, total) = children_progress(&children);
    let title = sanitize_child_title(&child.title);
    let body = format!(
        "Sub-issue {id} \"{title}\" is done. {done}/{total} sub-issues complete.",
        id = child.id,
    );

    // 7. Write the comment on the PARENT (workspace-scoped insert).
    let comment_id = new_id;
    CommentRepo::insert(
        pool,
        workspace_id,
        &NewComment {
            id: comment_id.clone(),
            issue_id: parent_id.clone(),
            author: author.clone(),
            body: body.clone(),
            created_at: now_ms,
        },
    )
    .await?;

    Ok(Some(ParentCascade {
        parent_id,
        parent_assignee: parent.assignee,
        comment_id,
        comment_author: author,
        comment_body: body,
        children_done: done,
        children_total: total,
    }))
}

/// Roll-up counts over a parent's sub-issues: `(done, total)` where `done` counts
/// terminal (`done`/`cancelled`) children.
fn children_progress(children: &[Issue]) -> (i64, i64) {
    let done = children.iter().filter(|c| is_terminal(&c.state)).count();
    (
        i64::try_from(done).unwrap_or(i64::MAX),
        i64::try_from(children.len()).unwrap_or(i64::MAX),
    )
}

/// Whether `child`'s completion closes the lowest unfinished stage barrier among
/// its siblings (`children` = the full sibling set, `child` included).
///
/// - **Unstaged set** (no sibling has a `stage`): one implicit stage — closed only
///   when EVERY child is terminal (fires once, when the last child finishes).
/// - **Staged set**: the completing child carries a stage `S`; the barrier is
///   closed when every staged sibling with `stage <= S` is terminal (frontier
///   closure). Unstaged siblings are ignored. A child with no stage in an
///   otherwise-staged set has no barrier of its own → never closes one.
fn stage_barrier_closed(children: &[Issue], child: &Issue) -> bool {
    let any_staged = children.iter().any(|c| c.stage.is_some());
    if !any_staged {
        // Implicit single stage: fire when the whole set is terminal.
        return children.iter().all(|c| is_terminal(&c.state));
    }
    // Staged set: the completing child must itself carry a stage.
    let Some(s) = child.stage else {
        return false;
    };
    // Frontier closure: every staged sibling at-or-below this stage is terminal.
    children
        .iter()
        .filter(|c| c.stage.is_some_and(|cs| cs <= s))
        .all(|c| is_terminal(&c.state))
}

/// Strip the `](mention://` marker from a child title before embedding it in the
/// parent comment, so a title that happens to contain a mention link cannot inject
/// a live mention into the system comment (mirrors multica's
/// `sanitizeChildTitleForSystemComment`).
fn sanitize_child_title(title: &str) -> String {
    title.replace("](mention://", "]( mention://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::repo::comment::CommentRepo;
    use crate::repo::issue::{IssueRepo, NewIssue};
    use ainb_hangar_core::actor::{ActorKind, ActorRef};

    fn agent() -> ActorRef {
        ActorRef::new(ActorKind::Agent, "agent-1").unwrap()
    }
    fn member() -> ActorRef {
        ActorRef::new(ActorKind::Member, "user-1").unwrap()
    }

    async fn seed_ws(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_issue(
        pool: &SqlitePool,
        ws: &str,
        id: &str,
        state: &str,
        assignee: Option<ActorRef>,
        parent: Option<&str>,
        stage: Option<i64>,
    ) {
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.into(),
                workspace_id: ws.into(),
                title: format!("Issue {id}"),
                description: None,
                state: state.into(),
                assignee,
                creator: member(),
                created_at: 1,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: parent.map(ToString::to_string),
                stage,
            },
        )
        .await
        .unwrap();
    }

    async fn parent_comments(pool: &SqlitePool, ws: &str, parent: &str) -> Vec<String> {
        CommentRepo::list_by_issue(pool, ws, parent)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.body)
            .collect()
    }

    /// A single unstaged child completing → one parent comment carrying `1/1`.
    #[tokio::test]
    async fn single_unstaged_child_fires_once_with_rollup() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "child",
            "done",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;

        let res = cascade_child_done(pool, "ws", "child", "open", "done", 10, "cm-1".into())
            .await
            .unwrap();
        let cascade = res.expect("cascade must fire on the last unstaged child");
        assert_eq!(cascade.parent_id, "parent");
        assert_eq!((cascade.children_done, cascade.children_total), (1, 1));

        let bodies = parent_comments(pool, "ws", "parent").await;
        assert_eq!(bodies.len(), 1, "exactly one comment");
        assert!(
            bodies[0].contains("1/1"),
            "body carries the roll-up: {}",
            bodies[0]
        );
        assert!(bodies[0].contains("Sub-issue") && bodies[0].contains("is done"));
    }

    /// Two unstaged children: the FIRST done fires nothing; the SECOND (the last)
    /// fires one comment (barrier = last child).
    #[tokio::test]
    async fn two_unstaged_children_fire_only_on_last() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "c1",
            "done",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;
        seed_issue(
            pool,
            "ws",
            "c2",
            "open",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;

        // First child done — c2 still open → NO comment.
        let r1 = cascade_child_done(pool, "ws", "c1", "open", "done", 10, "cm-1".into())
            .await
            .unwrap();
        assert!(r1.is_none(), "not the last child; no comment");
        assert!(parent_comments(pool, "ws", "parent").await.is_empty());

        // Second child done → ONE comment (2/2).
        IssueRepo::update_state(pool, "c2", "done").await.unwrap();
        let r2 = cascade_child_done(pool, "ws", "c2", "open", "done", 20, "cm-2".into())
            .await
            .unwrap();
        let c = r2.expect("last child closes the implicit barrier");
        assert_eq!((c.children_done, c.children_total), (2, 2));
        let bodies = parent_comments(pool, "ws", "parent").await;
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("2/2"));
    }

    /// A repeat `done`-save on an already-done child does not re-fire (transition
    /// guard: prev is already terminal).
    #[tokio::test]
    async fn repeat_terminal_save_does_not_refire() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "child",
            "done",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;

        // done → done: prev already terminal.
        let r = cascade_child_done(pool, "ws", "child", "done", "done", 10, "cm-1".into())
            .await
            .unwrap();
        assert!(r.is_none(), "a repeat terminal save must not re-fire");
        assert!(parent_comments(pool, "ws", "parent").await.is_empty());
    }

    /// A parent that is already `done`, or `member`-assigned, gets NO comment.
    #[tokio::test]
    async fn terminal_or_member_parent_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;

        // done parent.
        seed_issue(pool, "ws", "p-done", "done", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "c-done",
            "done",
            Some(agent()),
            Some("p-done"),
            None,
        )
        .await;
        let r = cascade_child_done(pool, "ws", "c-done", "open", "done", 10, "x1".into())
            .await
            .unwrap();
        assert!(r.is_none(), "a terminal parent is inert");
        assert!(parent_comments(pool, "ws", "p-done").await.is_empty());

        // member parent — no agent to trigger.
        seed_issue(pool, "ws", "p-mem", "open", Some(member()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "c-mem",
            "done",
            Some(agent()),
            Some("p-mem"),
            None,
        )
        .await;
        let r = cascade_child_done(pool, "ws", "c-mem", "open", "done", 10, "x2".into())
            .await
            .unwrap();
        assert!(r.is_none(), "a member-assigned parent is skipped");
        assert!(parent_comments(pool, "ws", "p-mem").await.is_empty());
    }

    /// An UNASSIGNED parent still gets the comment (the assignee guard only skips
    /// human members) — the returned cascade has `parent_assignee: None`.
    #[tokio::test]
    async fn unassigned_parent_still_gets_comment() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", None, None, None).await;
        seed_issue(
            pool,
            "ws",
            "child",
            "done",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;

        let r = cascade_child_done(pool, "ws", "child", "open", "done", 10, "cm-1".into())
            .await
            .unwrap();
        let c = r.expect("an unassigned parent still gets the comment");
        assert!(c.parent_assignee.is_none(), "nobody to wake");
        assert_eq!(parent_comments(pool, "ws", "parent").await.len(), 1);
    }

    /// Staged children (stage 1 ×2, stage 2 ×1): completing one stage-1 child fires
    /// nothing; completing the second stage-1 child fires ONE comment (stage-1
    /// barrier closed); the stage-2 child later fires its OWN comment.
    #[tokio::test]
    async fn staged_barriers_fire_per_closed_stage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "s1a",
            "open",
            Some(agent()),
            Some("parent"),
            Some(1),
        )
        .await;
        seed_issue(
            pool,
            "ws",
            "s1b",
            "open",
            Some(agent()),
            Some("parent"),
            Some(1),
        )
        .await;
        seed_issue(
            pool,
            "ws",
            "s2",
            "open",
            Some(agent()),
            Some("parent"),
            Some(2),
        )
        .await;

        // First stage-1 child done → stage 1 not yet closed.
        IssueRepo::update_state(pool, "s1a", "done").await.unwrap();
        let r = cascade_child_done(pool, "ws", "s1a", "open", "done", 10, "c1".into())
            .await
            .unwrap();
        assert!(r.is_none(), "stage 1 still has an open sibling");
        assert!(parent_comments(pool, "ws", "parent").await.is_empty());

        // Second stage-1 child done → stage-1 barrier closes → one comment.
        IssueRepo::update_state(pool, "s1b", "done").await.unwrap();
        let r = cascade_child_done(pool, "ws", "s1b", "open", "done", 20, "c2".into())
            .await
            .unwrap();
        assert!(r.is_some(), "stage-1 barrier closed");
        assert_eq!(parent_comments(pool, "ws", "parent").await.len(), 1);

        // Stage-2 child done → its own barrier closes → a second comment.
        IssueRepo::update_state(pool, "s2", "done").await.unwrap();
        let r = cascade_child_done(pool, "ws", "s2", "open", "done", 30, "c3".into())
            .await
            .unwrap();
        assert!(r.is_some(), "stage-2 barrier closed");
        assert_eq!(parent_comments(pool, "ws", "parent").await.len(), 2);
    }

    /// Deleting a parent orphans its children (`parent_issue_id` → NULL) and does
    /// not block — belt-and-braces UPDATE + `ON DELETE SET NULL`.
    #[tokio::test]
    async fn delete_parent_orphans_children() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        seed_issue(
            pool,
            "ws",
            "child",
            "open",
            Some(agent()),
            Some("parent"),
            None,
        )
        .await;

        IssueRepo::delete_cascade(pool, "ws", "parent").await.unwrap();

        let child = IssueRepo::get_by_id(pool, "child").await.unwrap().unwrap();
        assert_eq!(child.parent_issue_id, None, "child orphaned, not deleted");
    }
}
