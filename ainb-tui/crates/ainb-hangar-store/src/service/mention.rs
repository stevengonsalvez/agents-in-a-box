//! **Comment `@mention` routing** (multica parity #2-rest).
//!
//! One function, [`route`], answers "who did this comment address, and what did
//! we do about each of them". It is the single seam behind BOTH the daemon's
//! `hangar/comment_add` write and its `hangar/comment_mention_preview` dry run —
//! and behind the CLI's store-direct equivalents — precisely so a preview can
//! never disagree with the write it previews (multica step 10, and the
//! visibility-gate-identical-on-both requirement of step 7).
//!
//! # What changed relative to the pre-2-rest path
//!
//! The old `spawn_mention_tasks` returned only the agent ids it managed to
//! enqueue. A refused target was a silent `continue`, a repeat mention was a
//! swallowed unique-constraint violation, and a human `@handle` matched no agent
//! and vanished. Every one of those is now a REPORTED [`MentionRouteRow`] with a
//! [`MentionOutcome`] and, where one applies, a [`DispatchReason`].
//!
//! # The algorithm (each step cites the multica step it mirrors)
//!
//! 1. Parse the body ([`ainb_hangar_core::mention::parse`]). A reply that
//!    mentions nobody INHERITS its parent's mentions, under multica's three
//!    narrowing conditions (`shouldInheritParentMentions`, `comment.go:389`):
//!    the reply has no mentions of its own, its author is not an agent, and the
//!    parent's author is a member.
//! 2. Resolve every parsed target, workspace-scoped.
//! 3. **Explicit beats implicit** (multica step 2): if anything resolved, the
//!    fallback chain is skipped entirely.
//! 4. Otherwise walk the fallback chain, first hit wins (multica step 4):
//!    reply-parent author → thread-root author → issue assignee, each only when
//!    it is an AGENT.
//! 5. Act per target, one row each, never an early return, so one refused handle
//!    can never suppress the others.
//!
//! # Divergences from multica, deliberate
//!
//! * **Members and unresolvable handles are reported.** multica's outcome array
//!   only ever describes agent triggers. See [`MentionOutcome`]'s own docs.
//! * **No `mentioned` activity row.** hangar's `activity_log.action` is a closed
//!   CHECK-constrained set with no mention verb; adding one is a schema change
//!   this item does not need, since the inbox entry + subscription already make
//!   the human-routing leg observable. Left for the activity-vocabulary item.
//! * **An explicit mention bypasses the blocker gate**, matching multica's "no
//!   status gate here — an `@`mention is an explicit action" (`comment.go:~410`).
//!   Only a FALLBACK target is deferred behind unfinished blockers, because
//!   nobody asked for it by name.

use crate::repo::agent::{Agent, AgentRepo};
use crate::repo::card_dependency::CardDependencyRepo;
use crate::repo::comment::CommentRepo;
use crate::repo::inbox::{InboxKind, InboxRepo, NewInboxEntry};
use crate::repo::issue::IssueRepo;
use crate::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};
use crate::repo::member::MemberRepo;
use crate::repo::squad::SquadRepo;
use crate::repo::task::{NewTask, TaskRepo};
use crate::repo::workspace::WorkspaceRepo;
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::dispatch_reason::DispatchReason;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::mention::{
    self, MentionForm, MentionOutcome, MentionSource, MentionTargetKind, ParsedMention,
};
use ainb_hangar_core::origin::IssueOrigin;
use sqlx::SqlitePool;

/// The actor id the local TUI stamps on everything it authors (`member:me`).
///
/// It is a PLACEHOLDER, not a `user.id`: the plugin has no identity of its own
/// and the daemon socket is the local operator's.
pub const LOCAL_OPERATOR_MEMBER_ID: &str = "me";

/// How many characters of the comment body ride along in the inbox summary.
const SUMMARY_CHARS: usize = 120;

/// One routing request: everything [`route`] needs to decide and (optionally) act.
#[derive(Debug, Clone)]
pub struct MentionRouteRequest<'a> {
    /// The resolved workspace row id (never a slug).
    pub workspace_id: &'a str,
    /// The issue the comment belongs to.
    pub issue_id: &'a str,
    /// The COMMITTED comment's id. `None` means there is no comment yet, which
    /// is the preview case; [`route`] then behaves as a dry run regardless of
    /// [`dry_run`](Self::dry_run), because there is nothing to attribute a write
    /// to.
    pub comment_id: Option<&'a str>,
    /// The comment being replied to, if any (drives inheritance + the
    /// reply-parent fallback).
    pub parent_comment_id: Option<&'a str>,
    /// Who wrote the comment.
    pub author: &'a ActorRef,
    /// The comment body.
    pub body: &'a str,
    /// `true` ⇒ resolve and gate identically but write NOTHING.
    pub dry_run: bool,
}

/// What happened to ONE addressed target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionRouteRow {
    /// `agent` | `member` | `squad` | `issue` — the family of the target.
    pub target_type: String,
    /// The resolved id, or `""` when nothing resolved.
    pub target_id: String,
    /// The token exactly as typed (a bare handle, or the link's id).
    pub handle: String,
    /// The bucket.
    pub outcome: MentionOutcome,
    /// The precise reason, when the bucket has one.
    pub reason: Option<DispatchReason>,
    /// The task that was written or coalesced into.
    pub task_id: Option<String>,
    /// Free-form human detail. Never an existence oracle (see
    /// [`DispatchReason::InvocationNotAllowed`]).
    pub detail: Option<String>,
    /// Explicit, or which fallback leg produced this target.
    pub source: MentionSource,
}

impl MentionRouteRow {
    /// A `blocked` row with a reason and no target-specific ids.
    fn blocked(
        target_type: &str,
        target_id: &str,
        handle: &str,
        reason: DispatchReason,
        source: MentionSource,
    ) -> Self {
        Self {
            target_type: target_type.to_string(),
            target_id: target_id.to_string(),
            handle: handle.to_string(),
            outcome: MentionOutcome::Blocked,
            reason: Some(reason),
            task_id: None,
            detail: None,
            source,
        }
    }

    /// An `ignored` row: nothing resolved, or a non-actor cross-reference.
    fn ignored(target_type: &str, handle: &str) -> Self {
        Self {
            target_type: target_type.to_string(),
            target_id: String::new(),
            handle: handle.to_string(),
            outcome: MentionOutcome::Ignored,
            reason: None,
            task_id: None,
            detail: None,
            source: MentionSource::Explicit,
        }
    }
}

/// What one parsed mention resolved to.
enum Resolved {
    /// A real agent in this workspace (possibly via a squad's leader).
    Agent(Box<Agent>),
    /// A human member of this workspace, carrying their `user.id`.
    Member(String),
    /// Nothing to act on — the row is emitted verbatim.
    Row(MentionRouteRow),
}

/// Resolve every mention in `req.body`, decide what to do with each, and — when
/// this is not a dry run — do it.
///
/// Returns one [`MentionRouteRow`] per addressed target, in first-seen mention
/// order (fallback rows, which only exist when nothing was mentioned, last).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] only on a store fault that is not attributable to a
/// single target. A per-target fault degrades that target's row to
/// `blocked` / [`DispatchReason::InternalError`] and the rest of the targets are
/// still routed.
#[allow(clippy::too_many_lines)]
pub async fn route(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    req: &MentionRouteRequest<'_>,
) -> Result<Vec<MentionRouteRow>, sqlx::Error> {
    // No committed comment ⇒ nothing a write could be attributed to, so the run
    // is forced dry. This is a safety interlock, not a convenience: it makes
    // "preview writes nothing" impossible to get wrong at a call site.
    let dry_run = req.dry_run || req.comment_id.is_none();
    let now = clock.now_ms();

    let mut parsed = mention::parse(req.body);
    // Step 1 — multica `shouldInheritParentMentions` (`comment.go:389`), with
    // all three of its narrowing conditions: this reply mentions nobody, its
    // author is not an agent, and the parent's author is a member. A human
    // saying "yes please" under a comment that pinged an agent means the ping.
    if parsed.is_empty() && req.author.kind() == ActorKind::Member {
        if let Some(parent_id) = req.parent_comment_id {
            if let Some(parent) = CommentRepo::get(pool, req.workspace_id, parent_id).await? {
                if parent.author.kind() == ActorKind::Member {
                    parsed = mention::parse(&parent.body);
                }
            }
        }
    }

    // Step 2 — resolve, workspace-scoped.
    let mut targets: Vec<(ParsedMention, Resolved)> = Vec::new();
    for m in parsed {
        let resolved = resolve_one(pool, req.workspace_id, &m).await?;
        targets.push((m, resolved));
    }

    // Step 3 — explicit beats implicit (multica step 2). "Resolved" means an
    // actual actor: an `issue`/`all` cross-reference or an unknown handle does
    // NOT suppress the fallback, because the author addressed nobody.
    let any_actor = targets
        .iter()
        .any(|(_, r)| matches!(r, Resolved::Agent(_) | Resolved::Member(_)));

    let mut rows: Vec<MentionRouteRow> = Vec::new();

    if any_actor {
        // A single generation for the whole fan-out (migration 0039): every
        // agent this ONE comment triggers belongs to the same run. Minted once,
        // before the loop, exactly like the squad fan-out. Read-only, so it is
        // safe on the dry-run path too.
        let generation = TaskRepo::next_generation_for_issue(pool, req.issue_id).await?;
        let invoker = effective_invoker(pool, req.workspace_id, req.author).await?;
        for (m, resolved) in targets {
            match resolved {
                Resolved::Agent(agent) => {
                    rows.push(
                        route_agent(
                            pool,
                            idgen,
                            req,
                            &agent,
                            &m.token,
                            MentionSource::Explicit,
                            &invoker,
                            generation,
                            now,
                            dry_run,
                        )
                        .await,
                    );
                }
                Resolved::Member(user_id) => {
                    rows.push(notify_member(pool, idgen, req, &user_id, &m.token, now, dry_run).await);
                }
                Resolved::Row(row) => rows.push(row),
            }
        }
        return Ok(rows);
    }

    // Anything that parsed but resolved to nothing is still reported.
    for (_, resolved) in targets {
        if let Resolved::Row(row) = resolved {
            rows.push(row);
        }
    }

    // Step 4 — the fallback chain, first hit wins (multica step 4).
    let Some((agent, source)) = fallback_target(pool, req).await? else {
        return Ok(rows);
    };
    let generation = TaskRepo::next_generation_for_issue(pool, req.issue_id).await?;
    let invoker = effective_invoker(pool, req.workspace_id, req.author).await?;
    let handle = agent.name.clone();
    rows.push(
        route_agent(
            pool, idgen, req, &agent, &handle, source, &invoker, generation, now, dry_run,
        )
        .await,
    );
    Ok(rows)
}

/// Resolve one parsed mention to an actor in `workspace_id`.
async fn resolve_one(
    pool: &SqlitePool,
    workspace_id: &str,
    m: &ParsedMention,
) -> Result<Resolved, sqlx::Error> {
    match (m.form, m.kind) {
        // `mention://issue/...` and `mention://all/all` are CROSS-REFERENCES,
        // never triggers — multica filters them out of the trigger set
        // (`comment.go:293`). hangar reports them as `ignored` rather than
        // dropping them, so the caller can see they were understood.
        (MentionForm::Link, Some(MentionTargetKind::Issue)) => {
            Ok(Resolved::Row(MentionRouteRow::ignored("issue", &m.token)))
        }
        (MentionForm::Link, Some(MentionTargetKind::All)) => {
            Ok(Resolved::Row(MentionRouteRow::ignored("all", &m.token)))
        }
        (MentionForm::Link, Some(MentionTargetKind::Agent)) => {
            match AgentRepo::get(pool, &m.token).await? {
                // The workspace check is what stops a foreign tenant's agent id
                // being addressable by pasting it into a link.
                Some(a) if a.workspace_id == workspace_id => Ok(Resolved::Agent(Box::new(a))),
                _ => Ok(Resolved::Row(MentionRouteRow::ignored("agent", &m.token))),
            }
        }
        (MentionForm::Link, Some(MentionTargetKind::Member)) => {
            resolve_member(pool, workspace_id, &m.token).await
        }
        (MentionForm::Link, Some(MentionTargetKind::Squad)) => {
            resolve_squad(pool, workspace_id, &m.token).await
        }
        // A bare handle is UNTYPED: agent first (that is today's shipped
        // behaviour and every existing bare-mention test depends on it), then
        // member, then nothing.
        _ => {
            let agents = AgentRepo::list_by_workspace(pool, workspace_id).await?;
            if let Some(agent) = agents.into_iter().find(|a| a.name == m.token) {
                return Ok(Resolved::Agent(Box::new(agent)));
            }
            resolve_member(pool, workspace_id, &m.token).await
        }
    }
}

/// Resolve a member handle / id, or report it ignored.
async fn resolve_member(
    pool: &SqlitePool,
    workspace_id: &str,
    token: &str,
) -> Result<Resolved, sqlx::Error> {
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(Resolved::Row(MentionRouteRow::ignored("member", token)));
    };
    match MemberRepo::resolve_handle(pool, &ws, token).await? {
        Some(member) => Ok(Resolved::Member(member.user_id)),
        None => Ok(Resolved::Row(MentionRouteRow::ignored("member", token))),
    }
}

/// Resolve a squad to its LEADER agent (multica step 6).
///
/// A squad whose leader is a human — or which has no leader at all — has nothing
/// to dispatch to, so it is `blocked` / [`DispatchReason::TargetUnavailable`]
/// rather than silently ignored: the author DID address someone.
async fn resolve_squad(
    pool: &SqlitePool,
    workspace_id: &str,
    token: &str,
) -> Result<Resolved, sqlx::Error> {
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(Resolved::Row(MentionRouteRow::ignored("squad", token)));
    };
    let leader = SquadRepo::leader_agent_id(pool, &ws, token).await?;
    let Some(leader_id) = leader else {
        return Ok(Resolved::Row(MentionRouteRow::blocked(
            "squad",
            token,
            token,
            DispatchReason::TargetUnavailable,
            MentionSource::Explicit,
        )));
    };
    match AgentRepo::get(pool, &leader_id).await? {
        Some(a) if a.workspace_id == workspace_id => Ok(Resolved::Agent(Box::new(a))),
        _ => Ok(Resolved::Row(MentionRouteRow::blocked(
            "squad",
            token,
            token,
            DispatchReason::TargetUnavailable,
            MentionSource::Explicit,
        ))),
    }
}

/// Walk multica's implicit fallback chain and return the first AGENT it hits.
///
/// reply-parent author → thread-root author → issue assignee. Only an agent
/// counts: falling back onto a human would notify somebody nobody addressed.
async fn fallback_target(
    pool: &SqlitePool,
    req: &MentionRouteRequest<'_>,
) -> Result<Option<(Agent, MentionSource)>, sqlx::Error> {
    if let Some(parent_id) = req.parent_comment_id {
        if let Some(parent) = CommentRepo::get(pool, req.workspace_id, parent_id).await? {
            if parent.author.kind() == ActorKind::Agent {
                if let Some(agent) = agent_in_workspace(pool, req.workspace_id, parent.author.id()).await?
                {
                    return Ok(Some((agent, MentionSource::ReplyParent)));
                }
            }
        }
        if let Some(root) = CommentRepo::thread_root(pool, req.workspace_id, parent_id).await? {
            if root.author.kind() == ActorKind::Agent {
                if let Some(agent) = agent_in_workspace(pool, req.workspace_id, root.author.id()).await?
                {
                    return Ok(Some((agent, MentionSource::ThreadRoot)));
                }
            }
        }
    }
    let Some(issue) = IssueRepo::get_by_id(pool, req.issue_id).await? else {
        return Ok(None);
    };
    if issue.workspace_id != req.workspace_id {
        return Ok(None);
    }
    let Some(assignee) = issue.assignee else {
        return Ok(None);
    };
    if assignee.kind() != ActorKind::Agent {
        return Ok(None);
    }
    Ok(agent_in_workspace(pool, req.workspace_id, assignee.id())
        .await?
        .map(|a| (a, MentionSource::Assignee)))
}

/// An agent by id, but only when it belongs to `workspace_id`.
async fn agent_in_workspace(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_id: &str,
) -> Result<Option<Agent>, sqlx::Error> {
    Ok(AgentRepo::get(pool, agent_id)
        .await?
        .filter(|a| a.workspace_id == workspace_id))
}

/// Gate and (unless dry) act on ONE agent target, always producing exactly one
/// row. Gates run in multica's order — the self-loop skip and then the
/// invocation gate BEFORE any agent state is read, so a caller who may not
/// invoke an agent never learns its archived / runtime state from the trigger's
/// behaviour.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn route_agent(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    req: &MentionRouteRequest<'_>,
    agent: &Agent,
    handle: &str,
    source: MentionSource,
    invoker: &(ActorKind, Option<String>),
    generation: i64,
    now: i64,
    dry_run: bool,
) -> MentionRouteRow {
    let blocked = |reason| MentionRouteRow::blocked("agent", &agent.id, handle, reason, source);

    // 1. Self-loop (multica step 8). First, because it reads no agent state and
    //    therefore leaks nothing.
    if req.author.kind() == ActorKind::Agent && req.author.id() == agent.id {
        return blocked(DispatchReason::SelfTriggerSuppressed);
    }

    // 2. The invocation gate, IDENTICAL on preview and on write (multica step 7)
    //    so a preview can never leak a private agent's readiness.
    match AgentRepo::can_invoke(pool, agent, invoker.0, invoker.1.as_deref()).await {
        Ok(true) => {}
        Ok(false) => return blocked(DispatchReason::InvocationNotAllowed),
        Err(_) => return blocked(DispatchReason::InternalError),
    }

    // 3. Nothing to dispatch to.
    if agent.archived || agent.runtime_id.is_empty() {
        return blocked(DispatchReason::TargetUnavailable);
    }

    // 4. Blockers, FALLBACK targets only. An explicit mention is a deliberate
    //    human act and bypasses this (multica `comment.go:~410`); an implicit
    //    fallback is parked until `board::auto_run_dependent` promotes it.
    if source != MentionSource::Explicit {
        match CardDependencyRepo::unfinished_blockers_of(pool, req.issue_id).await {
            Ok(b) if !b.is_empty() => {
                return MentionRouteRow {
                    outcome: MentionOutcome::Deferred,
                    reason: Some(DispatchReason::Deferred),
                    ..blocked(DispatchReason::Deferred)
                };
            }
            Ok(_) => {}
            Err(_) => return blocked(DispatchReason::InternalError),
        }
    }

    // 5. Merge-into-pending (multica step 9). Detecting it up front is what
    //    turns a swallowed unique-constraint violation into a REPORTED outcome.
    match TaskRepo::pending_for_issue_agent(pool, req.workspace_id, req.issue_id, &agent.id).await {
        Ok(Some(pending)) => {
            if !dry_run {
                if let Some(comment_id) = req.comment_id {
                    // Re-point the pending task at the NEWER comment so the
                    // agent reads the latest ask when it claims.
                    let _ = TaskRepo::set_trigger_comment(pool, &pending.id, comment_id).await;
                }
            }
            return MentionRouteRow {
                target_type: "agent".into(),
                target_id: agent.id.clone(),
                handle: handle.to_string(),
                outcome: MentionOutcome::Coalesced,
                reason: Some(DispatchReason::Coalesced),
                task_id: Some(pending.id),
                detail: None,
                source,
            };
        }
        Ok(None) => {}
        Err(_) => return blocked(DispatchReason::InternalError),
    }

    if dry_run {
        // The preview reports what the write WOULD do, having run every gate.
        return MentionRouteRow {
            target_type: "agent".into(),
            target_id: agent.id.clone(),
            handle: handle.to_string(),
            outcome: MentionOutcome::Queued,
            reason: Some(DispatchReason::Queued),
            task_id: None,
            detail: None,
            source,
        };
    }

    // 6. Enqueue.
    let task = NewTask {
        id: idgen.new_ulid(),
        workspace_id: req.workspace_id.to_string(),
        runtime_id: agent.runtime_id.clone(),
        agent_id: agent.id.clone(),
        issue_id: Some(req.issue_id.to_string()),
        work_dir: None,
        // A mention is a direct, user-initiated ask: default urgency (P3),
        // drained FIFO among equals.
        priority: 0,
        created_at: now,
        autopilot_run_id: None,
        generation,
    };
    match TaskRepo::insert(pool, &task).await {
        Ok(_) => {}
        // A racing enqueue lost to the per-(issue, agent) unique index: that is
        // exactly the coalesce case, reported as such rather than errored.
        Err(e) if is_unique_violation(&e) => {
            return MentionRouteRow {
                target_type: "agent".into(),
                target_id: agent.id.clone(),
                handle: handle.to_string(),
                outcome: MentionOutcome::Coalesced,
                reason: Some(DispatchReason::Coalesced),
                task_id: None,
                detail: None,
                source,
            };
        }
        Err(_) => return blocked(DispatchReason::InternalError),
    }

    if let Some(comment_id) = req.comment_id {
        // 0056 ORIGIN PROVENANCE, stamped only on a task that actually landed.
        if let Ok(origin) = IssueOrigin::comment_mention(comment_id) {
            let _ = TaskRepo::set_origin(pool, &task.id, &origin).await;
        }
        // 0067: the comment that summoned this run, so the agent reads the
        // actual ask and threads its reply under it (multica task.go:443).
        let _ = TaskRepo::set_trigger_comment(pool, &task.id, comment_id).await;
    }
    // multica parity #22: being @-mentioned subscribes you.
    if let Ok(actor) = ActorRef::new(ActorKind::Agent, agent.id.clone()) {
        let _ = IssueSubscriberRepo::add(
            pool,
            req.workspace_id,
            req.issue_id,
            &actor,
            SubscribeReason::Mentioned,
            now,
        )
        .await;
    }

    MentionRouteRow {
        target_type: "agent".into(),
        target_id: agent.id.clone(),
        handle: handle.to_string(),
        outcome: MentionOutcome::Queued,
        reason: Some(DispatchReason::Queued),
        task_id: Some(task.id),
        detail: None,
        source,
    }
}

/// A member mention NOTIFIES, it never triggers (multica step 3).
///
/// On a write that is an inbox entry addressed to the human plus a `mentioned`
/// subscription, which together are what make "@mentioning a person routes to
/// that person" observable. 0060 already made `inbox_entry` actor-polymorphic,
/// so no schema work was needed for this leg.
async fn notify_member(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    req: &MentionRouteRequest<'_>,
    user_id: &str,
    handle: &str,
    now: i64,
    dry_run: bool,
) -> MentionRouteRow {
    let row = MentionRouteRow {
        target_type: "member".into(),
        target_id: user_id.to_string(),
        handle: handle.to_string(),
        outcome: MentionOutcome::Notified,
        reason: None,
        task_id: None,
        detail: None,
        source: MentionSource::Explicit,
    };
    if dry_run {
        return row;
    }
    let Ok(recipient) = ActorRef::new(ActorKind::Member, user_id.to_string()) else {
        return MentionRouteRow::blocked(
            "member",
            user_id,
            handle,
            DispatchReason::InternalError,
            MentionSource::Explicit,
        );
    };
    let entry = NewInboxEntry {
        id: idgen.new_ulid(),
        workspace_id: req.workspace_id.to_string(),
        recipient: recipient.clone(),
        kind: InboxKind::Issue,
        event: "mention".to_string(),
        subject_id: req.issue_id.to_string(),
        summary: summarise(req.body),
        created_at: now,
    };
    if InboxRepo::insert(pool, &entry).await.is_err() {
        return MentionRouteRow::blocked(
            "member",
            user_id,
            handle,
            DispatchReason::InternalError,
            MentionSource::Explicit,
        );
    }
    let _ = IssueSubscriberRepo::add(
        pool,
        req.workspace_id,
        req.issue_id,
        &recipient,
        SubscribeReason::Mentioned,
        now,
    )
    .await;
    row
}

/// The first [`SUMMARY_CHARS`] CHARACTERS (not bytes) of the body, so a
/// multi-byte body can never be sliced mid-character.
fn summarise(body: &str) -> String {
    body.chars().take(SUMMARY_CHARS).collect()
}

/// Resolve a comment author to the EFFECTIVE invoking identity the invocation
/// gate judges its targets by.
///
/// - a `member` author naming a REAL user id → that user;
/// - the local-operator placeholder [`LOCAL_OPERATOR_MEMBER_ID`] → the workspace
///   OWNER, exactly like `run_card`'s "no explicit invoker ⇒ owner" default. The
///   local TUI stamps `member:me` on every comment it composes, so without this
///   the gate would deny the single operator access to their own private agents
///   — a regression with no security gain, since that socket IS the operator's.
///   An owner-less workspace resolves to `""`, which matches no `owner_id` and
///   fails closed;
/// - an `agent` author → `(Agent, None)`: hangar has no `originator_user_id`
///   column, so an agent-authored mention is the UNATTRIBUTED A2A case — it
///   fails closed for `private` and `member`-target agents.
///
/// Moved verbatim from `ainb_hangar_daemon::rpc::snapshots` so the CLI's
/// store-direct path applies the identical rule.
async fn effective_invoker(
    pool: &SqlitePool,
    workspace_id: &str,
    author: &ActorRef,
) -> Result<(ActorKind, Option<String>), sqlx::Error> {
    if author.kind() != ActorKind::Member {
        return Ok((ActorKind::Agent, None));
    }
    if author.id() != LOCAL_OPERATOR_MEMBER_ID {
        return Ok((ActorKind::Member, Some(author.id().to_string())));
    }
    let owner = match WorkspaceId::from_str(workspace_id.to_string()) {
        Ok(ws) => WorkspaceRepo::owner_id(pool, &ws).await?.unwrap_or_default(),
        Err(_) => String::new(),
    };
    Ok((ActorKind::Member, Some(owner)))
}

/// Whether a `sqlx` error is a UNIQUE-constraint violation (the per-`(issue,
/// agent)` pending-task index coalescing a racing enqueue).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}
