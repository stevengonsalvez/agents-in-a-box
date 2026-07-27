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

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::idgen::IdGen;
use sqlx::SqlitePool;

use crate::repo::comment::{CommentRepo, NewComment};
use crate::repo::issue::{Issue, IssueRepo};

/// One child's lifecycle move, the input unit of [`cascade_children_done`].
///
/// A batch is just a slice of these. Duplicated `child_id`s collapse (first
/// wins) and a move that is not *into* terminal is dropped before any read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildTransition {
    /// The sub-issue that moved.
    pub child_id: String,
    /// Its state BEFORE the edit committed.
    pub prev_state: String,
    /// Its state AFTER the edit committed.
    pub new_state: String,
}

/// One child named by a fired cascade comment.
///
/// A comment reports >1 child exactly when a batch closed a barrier with several
/// completions in it (or a late low-stage completion closed several stages at
/// once) — that aggregation is the point of parity #3-rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeChild {
    /// The sub-issue id.
    pub id: String,
    /// Its title, as written into the comment (already sanitised).
    pub title: String,
    /// Its stage barrier, `None` for an unstaged child.
    pub stage: Option<i64>,
}

/// A closed stage barrier: the ledger key it claims plus the stage it reports.
///
/// `stage: None` is the implicit single stage of an unstaged sibling set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Barrier {
    stage: Option<i64>,
    key: String,
}

/// The outcome of a FIRED cascade, returned so the daemon can wake the parent.
///
/// A `None` from [`cascade_child_done`] means nothing fired (not a terminal
/// transition, no parent, a guard tripped, the barrier is not closed, or the
/// barrier was ALREADY reported by another completion — see the claim ledger in
/// migration 0065).
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
    /// Every child this ONE comment reports, in `(stage, id)` order. Length > 1
    /// means the batch aggregated several completions into a single comment.
    pub children: Vec<CascadeChild>,
    /// The barriers this comment closed (`None` = the unstaged implicit stage),
    /// in stage order. Length > 1 means several stages closed together.
    pub stages_closed: Vec<Option<i64>>,
}

/// `true` when a state token is terminal for the child-done cascade — `done` OR
/// `cancelled` (mirrors multica's `isTerminalChildStatus`).
fn is_terminal(state: &str) -> bool {
    state == "done" || state == "cancelled"
}

/// Post the child-done comment on the parent iff `child_id`'s transition from
/// `prev_state` to `new_state` CLOSES a stage barrier (migration 0046) that has
/// not already been reported (the claim ledger, migration 0065).
///
/// A one-element wrapper over [`cascade_children_done`] — the single-child and
/// batch paths share ONE implementation, so their guards, comment body and
/// dedupe cannot drift. Returns `Ok(Some(cascade))` when a comment was written,
/// `Ok(None)` when it did not fire. The guards, in multica order:
/// 1. the transition must be *into* terminal (`!is_terminal(prev) &&
///    is_terminal(new)`) — a repeat terminal save never re-fires;
/// 2. the child must have a `parent_issue_id`, and the parent must resolve in the
///    same workspace;
/// 3. the parent must not already be `done`/`cancelled`, must not be `backlog` (a
///    parked parent stays inert), and its assignee must not be a human `member`
///    (no agent to trigger — an *unassigned* parent still gets the comment);
/// 4. the completion must close a barrier (see [`closed_barriers`]) that no
///    earlier completion has already claimed.
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
    let transitions = [ChildTransition {
        child_id: child_id.to_string(),
        prev_state: prev_state.to_string(),
        new_state: new_state.to_string(),
    }];
    let idgen = OneShotIdGen::new(new_id);
    cascade_children_done(pool, workspace_id, &transitions, now_ms, &idgen)
        .await
        .map(|v| v.into_iter().next())
}

/// Cascade a WHOLE BATCH of child completions, posting at most ONE aggregated
/// comment per parent (multica parity #3-rest, MUL-4155).
///
/// Three invariants, all durable rather than request-scoped (hangar has no
/// request boundary on the agent-completion path — two sibling tasks genuinely
/// finish concurrently):
///
/// - **B1 one comment per parent per barrier.** Barriers are claimed in the same
///   transaction that writes the comment (`issue_cascade_barrier`, migration
///   0065), so N completions closing one barrier post ONE comment; the losing
///   claimant affects 0 rows and posts nothing.
/// - **B2 computed from FINAL state.** The sibling set is re-read after the whole
///   batch committed, never from per-child intermediate state.
/// - **B3 order independence.** [`closed_barriers`] is a pure function of the
///   final sibling set, so a late low-stage completion closes every stage whose
///   prefix is now terminal. Under the old single-frontier check a stage that
///   completed early had its close DROPPED FOREVER.
///
/// Returns one [`ParentCascade`] per parent that actually received a comment, in
/// first-seen parent order.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on any store fault; nothing is left half-written
/// because the claim + the comment share one transaction.
pub async fn cascade_children_done(
    pool: &SqlitePool,
    workspace_id: &str,
    transitions: &[ChildTransition],
    now_ms: i64,
    idgen: &impl IdGen,
) -> Result<Vec<ParentCascade>, sqlx::Error> {
    // 1. Keep only real non-terminal → terminal moves, first occurrence of each
    //    child, then group the moved children by parent (parentless children and
    //    unknown ids drop out here).
    let mut seen: HashSet<&str> = HashSet::new();
    let mut groups: Vec<(String, Vec<Issue>)> = Vec::new();
    for t in transitions {
        if !seen.insert(t.child_id.as_str()) {
            continue;
        }
        if is_terminal(&t.prev_state) || !is_terminal(&t.new_state) {
            continue;
        }
        let Some(child) = IssueRepo::get_by_id(pool, &t.child_id).await? else {
            continue;
        };
        let Some(parent_id) = child.parent_issue_id.clone() else {
            continue;
        };
        match groups.iter_mut().find(|(p, _)| *p == parent_id) {
            Some((_, moved)) => moved.push(child),
            None => groups.push((parent_id, vec![child])),
        }
    }

    let mut out = Vec::new();
    for (parent_id, mut moved) in groups {
        // 2. Parent guards, run ONCE per parent (unchanged semantics).
        let Some(parent) = IssueRepo::get_by_id(pool, &parent_id).await? else {
            continue;
        };
        if parent.workspace_id != workspace_id {
            continue;
        }
        if is_terminal(&parent.state) || parent.state == "backlog" {
            continue;
        }
        if matches!(
            parent.assignee.as_ref().map(ActorRef::kind),
            Some(ActorKind::Member)
        ) {
            continue;
        }

        // 3. Barriers closed by the FINAL sibling set (B2/B3).
        let children = IssueRepo::list_children(pool, &parent_id).await?;
        let barriers = closed_barriers(&children);
        if barriers.is_empty() {
            continue;
        }

        // 4. ONE transaction: claim the barriers, then write the comment. A
        //    crash between them is impossible and two racing daemons resolve to
        //    exactly one winner.
        let comment_id = idgen.new_ulid();
        let mut tx = pool.begin().await?;
        let mut claimed: Vec<Barrier> = Vec::new();
        for b in &barriers {
            let res = sqlx::query(
                "INSERT OR IGNORE INTO issue_cascade_barrier \
                 (parent_issue_id, workspace_id, stage_key, comment_id, created_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&parent_id)
            .bind(workspace_id)
            .bind(&b.key)
            .bind(&comment_id)
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
            if res.rows_affected() == 1 {
                claimed.push(b.clone());
            }
        }
        if claimed.is_empty() {
            // Every closed barrier was already reported — post nothing.
            tx.rollback().await?;
            continue;
        }

        // 5. The comment reports the children of the NEWLY claimed barriers, in
        //    `(stage, id)` order so the body is byte-identical whatever order the
        //    batch supplied (B3). An unstaged child in an otherwise-staged set
        //    belongs to no barrier and so never triggers a comment, exactly as
        //    before.
        moved.sort_by(|a, b| (a.stage, &a.id).cmp(&(b.stage, &b.id)));
        let reported: Vec<&Issue> = moved
            .iter()
            .filter(|c| claimed.iter().any(|b| b.stage == c.stage))
            .collect();
        if reported.is_empty() {
            tx.rollback().await?;
            continue;
        }

        // 6. Author = the completing actor of the FIRST reported child (assignee
        //    else creator); both are guaranteed `member|agent`.
        let author = reported[0]
            .assignee
            .clone()
            .unwrap_or_else(|| reported[0].creator.clone());
        let (done, total) = children_progress(&children);
        let body = cascade_body(&reported, done, total, &claimed);

        CommentRepo::insert_with(
            &mut *tx,
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
        tx.commit().await?;

        out.push(ParentCascade {
            parent_id,
            parent_assignee: parent.assignee,
            comment_id,
            comment_author: author,
            comment_body: body,
            children_done: done,
            children_total: total,
            children: reported
                .iter()
                .map(|c| CascadeChild {
                    id: c.id.clone(),
                    title: sanitize_child_title(&c.title),
                    stage: c.stage,
                })
                .collect(),
            stages_closed: claimed.iter().map(|b| b.stage).collect(),
        });
    }
    Ok(out)
}

/// The distinctive, queryable comment body.
///
/// The single-child unstaged form is BYTE-IDENTICAL to the pre-#3-rest string so
/// the shipped tripwire and every existing assertion stay green. Stable greppable
/// tokens: `sub-issues complete.` always, `Sub-issue ` vs `Sub-issues `, and
/// `Closed stage`.
fn cascade_body(reported: &[&Issue], done: i64, total: i64, claimed: &[Barrier]) -> String {
    let list = reported
        .iter()
        .map(|c| format!("{} \"{}\"", c.id, sanitize_child_title(&c.title)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut body = if reported.len() == 1 {
        format!("Sub-issue {list} is done. {done}/{total} sub-issues complete.")
    } else {
        format!("Sub-issues {list} are done. {done}/{total} sub-issues complete.")
    };
    let mut stages: Vec<i64> = claimed.iter().filter_map(|b| b.stage).collect();
    stages.sort_unstable();
    if !stages.is_empty() {
        let names = stages.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        if stages.len() == 1 {
            body.push_str(&format!(" Closed stage {names}."));
        } else {
            body.push_str(&format!(" Closed stages {names}."));
        }
    }
    body
}

/// An [`IdGen`] over ONE pre-minted id, so [`cascade_child_done`] can keep its
/// exact signature while delegating to the batch implementation.
///
/// A single transition can only ever reach one parent, so the suffixed fallback
/// is unreachable — it exists so the impl is total rather than panicking.
struct OneShotIdGen {
    base: String,
    used: AtomicUsize,
}

impl OneShotIdGen {
    fn new(base: String) -> Self {
        Self {
            base,
            used: AtomicUsize::new(0),
        }
    }
}

impl IdGen for OneShotIdGen {
    fn new_ulid(&self) -> String {
        let n = self.used.fetch_add(1, Ordering::Relaxed);
        if n == 0 {
            self.base.clone()
        } else {
            format!("{}-{n}", self.base)
        }
    }
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

/// Every stage barrier that is CLOSED for this sibling set — a pure function of
/// FINAL state, independent of which child triggered the call.
///
/// - **Unstaged set** (no sibling carries a `stage`): one implicit barrier,
///   closed iff every child is terminal.
/// - **Staged set**: every stage `n` present such that EVERY staged sibling with
///   `stage <= n` is terminal (frontier prefix). Unstaged siblings in a mixed set
///   are ignored, as before.
///
/// Returning the whole closed prefix (not just the triggering child's stage) is
/// the order-independence fix: when a late stage-1 completion lands after stage 2
/// already finished, BOTH barriers are reported. The single-frontier predicate it
/// replaces dropped stage 2's close forever.
///
/// The `stage_key` embeds the barrier's member count so a sibling set that GROWS
/// after closing forms a NEW barrier and still fires, rather than being
/// suppressed by a stale claim.
fn closed_barriers(children: &[Issue]) -> Vec<Barrier> {
    if children.is_empty() {
        return Vec::new();
    }
    if children.iter().all(|c| c.stage.is_none()) {
        if children.iter().all(|c| is_terminal(&c.state)) {
            return vec![Barrier {
                stage: None,
                key: format!("unstaged:{}", children.len()),
            }];
        }
        return Vec::new();
    }
    let mut stages: Vec<i64> = children.iter().filter_map(|c| c.stage).collect();
    stages.sort_unstable();
    stages.dedup();
    stages
        .into_iter()
        .filter(|n| {
            children
                .iter()
                .filter(|c| c.stage.is_some_and(|cs| cs <= *n))
                .all(|c| is_terminal(&c.state))
        })
        .map(|n| {
            let members = children.iter().filter(|c| c.stage == Some(n)).count();
            Barrier {
                stage: Some(n),
                key: format!("stage:{n}:{members}"),
            }
        })
        .collect()
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
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
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

    /// RED-step shim (§6.0): two same-stage children completing in one batch must
    /// produce ONE aggregated parent comment. Driven here through the single-child
    /// entry point, which is what the batch producers do today.
    #[tokio::test]
    async fn batch_of_two_same_stage_children_posts_one_comment() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws").await;
        seed_issue(pool, "ws", "parent", "open", Some(agent()), None, None).await;
        for c in ["c1", "c2"] {
            seed_issue(pool, "ws", c, "done", Some(agent()), Some("parent"), Some(1)).await;
        }
        for (i, c) in ["c1", "c2"].iter().enumerate() {
            cascade_child_done(pool, "ws", c, "open", "done", 10, format!("cm-{i}"))
                .await
                .unwrap();
        }
        assert_eq!(
            parent_comments(pool, "ws", "parent").await.len(),
            1,
            "one barrier close = one aggregated comment"
        );
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
