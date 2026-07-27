//! Store-row → proto-wire-row mappers for the P4 `hangar/*` snapshot RPCs.
//!
//! The daemon owns the data plane; the plugin owns zero domain data and only
//! renders the wire rows it pulls here. Each function reads the store repos and
//! flattens their rich Rust types (polymorphic [`ActorRef`], sqlx row models)
//! into the flat [`ainb_hangar_proto`] wire shapes the screens render from.
//!
//! ## Presence derivation
//!
//! An agent's [`PresenceState`] folds its backing runtime's stored `status`
//! together with the AGE of its `last_seen_at` heartbeat, via the shared
//! [`PresenceState::derive`] (multica `deriveRuntimeHealth` +
//! `deriveAgentAvailability`): a runtime unseen for more than 5 minutes reads
//! `Unstable`, more than 10 minutes `Offline`, and the worse of (stored status,
//! heartbeat age) always wins.
//!
//! The age fold lives on the READ side deliberately. Hangar's presence sweeper
//! runs inside the daemon, so a daemon that dies cannot flip its own row: a
//! status-only passthrough would pin every agent of a crashed daemon at
//! `● online` forever. The sweeper still writes the status (so every other
//! reader sees the truth and the TUI gets an event), but this snapshot is
//! correct with no live daemon and no tick latency. `now_ms` is injected rather
//! than read here so the derivation is deterministic under test.
//!
//! A human member has no runtime, so it is always reported `Online` (a member is
//! "available" in the picker; the real online/away signal for humans lands with
//! presence tracking in a later phase).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ainb_hangar_core::acceptance::AcceptanceCriterion;
use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AgentId, AutopilotId, CommentId, IssueId, SkillId, WorkspaceId};
use ainb_hangar_core::origin::IssueOrigin;
use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_proto::events::{
    ActorRow, AgentSkillLinkRow, AttentionRow, AutopilotRow, AutopilotRunRow, AutopilotVersionRow,
    CommentRow, InboxEntryRow, IssueRow, PresenceState, SkillFile, SkillRow, TaskCardRow, Workload,
};
use ainb_hangar_proto::snapshots::{
    AgentSkillsListResult, SkillDetail, SkillsSyncResult, TimelineEntryRow,
};
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
use ainb_hangar_store::repo::attention::AttentionRepo;
use ainb_hangar_store::repo::autopilot::{
    AutopilotEdit, AutopilotRepo, AutopilotRepoError, UpdateOutcome,
};
use ainb_hangar_store::repo::autopilot_rule_version::AutopilotRuleVersionRepo;
use ainb_hangar_store::repo::autopilot_run::{
    DispatchOutcome, FireError, RunAttribution, RunSource, dispatch_with_admission,
    fire_autopilot_tick_with_attribution,
};
use ainb_hangar_store::repo::comment::{CommentRepo, NewComment};
use ainb_hangar_store::repo::inbox::InboxRepo;
use ainb_hangar_store::repo::issue::{CriterionError, IssueRepo};
use ainb_hangar_store::repo::label::{LabelRepo, LabelRepoError};
use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;
use ainb_hangar_store::repo::run_history::RunHistoryRepo;
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
use ainb_hangar_store::repo::task::TaskRepo;
use ainb_hangar_store::repo::usage::UsageRepo;
use ainb_hangar_store::repo::workspace::{apply_issue_prefix, issue_display_id};
use ainb_hangar_store::service::activity::ActivityService;
use sqlx::{Row, SqlitePool};

/// Every Hangar issue lifecycle state, queried per-state and concatenated so the
/// snapshot carries the whole board. `IssueRepo` lists by `(workspace, state)`,
/// so the snapshot unions the SEVEN canonical states (the
/// [`IssueLifecycle`](ainb_hangar_proto::lifecycle::IssueLifecycle) vocabulary
/// the board buckets through) plus the legacy `open` / `closed` tokens, which
/// `issue_create` and the Beads inbound sync may still write until they adopt the
/// canonical vocabulary. The legacy tokens still bucket forward via
/// `IssueLifecycle::for_state` (`open -> Todo`, `closed -> Done`), so a row in
/// either vocabulary lands in a real column; querying both ensures no row is
/// missed by the per-state union. A `tests` assertion pins this list to a
/// superset of `IssueLifecycle::ALL` so a new canonical status can never be added
/// without the snapshot querying it.
const ISSUE_STATES: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "done",
    // Appended by migration 0049 (multica gap #19). An omission here is
    // fail-HIDDEN, not fail-visible: the union is per-state, so an unqueried
    // token means the row never reaches the client at all rather than landing in
    // the wrong column.
    "blocked",
    "cancelled",
    "open",
    "closed",
];

/// Snapshot every issue in `workspace_id`, mapped to wire [`IssueRow`]s.
///
/// Each row carries its `HGR-<n>` `display_id` (63l.3): the issue's per-workspace
/// creation ordinal ([`IssueRepo::workspace_seq`]) joined to the workspace's
/// `issue_prefix` (or the `HGR` default) via [`issue_display_id`]. The prefix is
/// read once for the whole snapshot, not per row.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if any per-state query fails.
pub async fn issues_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<IssueRow>, sqlx::Error> {
    // Read the workspace's display prefix once; every row's HGR-<n> resolves
    // against the same prefix (NULL → the HGR default at the display layer).
    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let mut out = Vec::new();
    for state in ISSUE_STATES {
        for issue in IssueRepo::list_by_workspace_state(pool, workspace_id, state).await? {
            let id = IssueId::from_str(&issue.id).map_err(|e| sqlx::Error::ColumnDecode {
                index: "id".to_string(),
                source: format!("malformed issue id {:?}: {e}", issue.id).into(),
            })?;
            // 63l.3: the HGR-<n> the issue list + CLI surface, derived from the
            // issue's creation ordinal and the workspace prefix read above.
            let display_id =
                issue_display_row(pool, workspace_id, &issue.id, prefix.as_deref()).await?;
            // P9.2: surface the PR URL captured by P9.1 from this issue's latest
            // completed task's `result.pr_url`, or `None` when no task opened a PR.
            let pr_url = latest_pr_url_for_issue(pool, workspace_id, &issue.id).await?;
            // ch3: the latest completed task's branch, so the task-detail opened
            // from the issue list renders the run-branch line.
            let branch = latest_branch_for_issue(pool, workspace_id, &issue.id).await?;
            // 63d: the card extras (repo/agent/branches + run summary) for the
            // task-detail card.
            let extras = issue_card_fields(pool, &issue.id).await?;
            out.push(IssueRow {
                // multica parity #12: WHY this card is not running, from the newest
                // dispatch_attempt when that attempt was a decline. All `None` on a
                // healthy card, so the row grows by zero keys.
                last_dispatch_reason: extras.last_dispatch_reason,
                last_dispatch_detail: extras.last_dispatch_detail,
                last_dispatch_at: extras.last_dispatch_at,
                // ORIGIN PROVENANCE (0056): echoed from the stored pair so the wire
                // row a snapshot carries and the row an event pushes agree.
                origin_type: issue.origin.as_ref().map(|o| o.kind_db_str().to_string()),
                origin_id: issue.origin.as_ref().and_then(|o| o.id().map(ToString::to_string)),
                id,
                display_id,
                workspace_id: issue.workspace_id,
                title: issue.title,
                description: issue.description,
                state: issue.state,
                assignee: issue.assignee.map(|a| format!("{}:{}", a.kind().as_str(), a.id())),
                creator: format!("{}:{}", issue.creator.kind().as_str(), issue.creator.id()),
                created_at: issue.created_at,
                priority: issue.priority,
                due_date: issue.due_date,
                labels: issue.labels,
                pr_url,
                branch,
                repo_ref: extras.repo_ref,
                agent: extras.agent,
                source_branch: extras.source_branch,
                target_branch: extras.target_branch,
                external_ref: issue.external_ref,
                run_count: extras.run_count,
                last_run_status: extras.last_run_status,
                last_run_at: extras.last_run_at,
                parent_id: extras.parent_id,
                child_total: extras.child_total,
                child_done: extras.child_done,
                acceptance_criteria: criteria_texts(&issue.acceptance_criteria),
                acceptance: issue.acceptance_criteria,
                context_refs: issue.context_refs,
                dependencies: Vec::new(),
            });
        }
    }
    Ok(out)
}

/// The plural criterion TEXTS, mirrored into the pre-#11-rest
/// `IssueRow.acceptance_criteria` field so every existing client keeps working
/// while the structured `IssueRow.acceptance` list carries the ids + checked
/// state. Both fields are always filled from the SAME source.
fn criteria_texts(items: &[AcceptanceCriterion]) -> Vec<String> {
    items.iter().map(|c| c.text.clone()).collect()
}

/// The extra fields a wire [`IssueRow`] carries for the task-detail card (63d):
/// the migration-0042 card-parity fields plus the run-history summary.
#[derive(Debug, Default, Clone)]
struct IssueCardExtras {
    repo_ref: Option<String>,
    agent: Option<String>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    run_count: u32,
    last_run_status: Option<String>,
    last_run_at: Option<i64>,
    /// The issue's parent, when it is a sub-issue (migration 0046); `None` for a
    /// top-level issue.
    parent_id: Option<String>,
    /// This issue's sub-issue roll-up: `(done, total)`. `(0, 0)` when it has no
    /// children. Drives the parent card's `⊟ done/total` badge.
    child_done: u32,
    child_total: u32,
    /// WHY this card is not running (multica parity #12): the stable code +
    /// detail + timestamp of the newest `dispatch_attempt`, filled ONLY when that
    /// attempt was a DECLINE. All `None` on a healthy card, so the wire row grows
    /// by zero keys for one that is running fine.
    last_dispatch_reason: Option<String>,
    last_dispatch_detail: Option<String>,
    last_dispatch_at: Option<i64>,
}

/// Read the task-detail card extras for one issue (63d): the `repo_ref` / `agent`
/// / source+target branches from the issue's migration-0042 columns
/// ([`CardParityRepo`]), plus a one-query run summary (count + latest task's
/// status + created_at) from `agent_task_queue`.
///
/// A single reader so every IssueRow-building path (list, search, update, create)
/// fills the card extras identically. `agent` is the lowercase provider wire
/// token; every field defaults to empty for a card with nothing pinned / never
/// run.
async fn issue_card_fields(
    pool: &SqlitePool,
    issue_id: &str,
) -> Result<IssueCardExtras, sqlx::Error> {
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    let (repo_ref, agent) = CardParityRepo::get_issue_repo_agent(pool, issue_id)
        .await?
        .unwrap_or((None, None));
    let (source_branch, target_branch) = CardParityRepo::get_issue_branches(pool, issue_id)
        .await?
        .unwrap_or((None, None));
    // One round-trip for the run summary: total task count + the newest task's
    // status + created_at (NULL for both when the issue never ran).
    let (count, last_status, last_at): (i64, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*), \
         (SELECT status FROM agent_task_queue WHERE issue_id = ?1 \
            ORDER BY created_at DESC, id DESC LIMIT 1), \
         (SELECT created_at FROM agent_task_queue WHERE issue_id = ?1 \
            ORDER BY created_at DESC, id DESC LIMIT 1) \
         FROM agent_task_queue WHERE issue_id = ?1",
    )
    .bind(issue_id)
    .fetch_one(pool)
    .await?;
    // 0046: this issue's parent link + its sub-issue roll-up, so the wire row can
    // render the parent card's `⊟ done/total` badge and thread reparenting.
    let parent_id: Option<String> =
        sqlx::query_scalar("SELECT parent_issue_id FROM issue WHERE id = ?")
            .bind(issue_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let (child_done, child_total) = IssueRepo::child_progress(pool, issue_id).await?;
    // multica parity #12: the newest admission decision, surfaced ONLY when it
    // was a decline — the field means "why this is not running", so a healthy card
    // carries no extra bytes. `runtime_offline` counts as a decline even though a
    // task row exists: that is exactly the invisible-but-queued case.
    let latest_attempt =
        ainb_hangar_store::repo::dispatch_attempt::DispatchAttemptRepo::latest_for_issue(
            pool, issue_id,
        )
        .await?
        .filter(|a| !a.is_dispatched());
    Ok(IssueCardExtras {
        repo_ref,
        agent: agent.map(|a| a.as_str().to_string()),
        source_branch,
        target_branch,
        run_count: u32::try_from(count).unwrap_or(u32::MAX),
        last_run_status: last_status,
        last_run_at: last_at,
        parent_id,
        child_done: u32::try_from(child_done).unwrap_or(u32::MAX),
        child_total: u32::try_from(child_total).unwrap_or(u32::MAX),
        last_dispatch_reason: latest_attempt.as_ref().map(|a| a.reason.clone()),
        last_dispatch_detail: latest_attempt.as_ref().and_then(|a| a.detail.clone()),
        last_dispatch_at: latest_attempt.as_ref().map(|a| a.created_at),
    })
}

/// The `HGR-<n>` display id for one issue, or `None` when the id resolves to no
/// row in `workspace_id` (a stale id). Joins the issue's per-workspace creation
/// ordinal ([`IssueRepo::workspace_seq`]) to `prefix` (the workspace's
/// `issue_prefix`, or the `HGR` default when `None`) via [`issue_display_id`].
///
/// The single place a wire row's display id is assembled, so the list, search,
/// update, label, and create paths all agree byte-for-byte (63l.3).
async fn issue_display_row(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    prefix: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    Ok(IssueRepo::workspace_seq(pool, workspace_id, issue_id)
        .await?
        .map(|seq| issue_display_id(prefix, seq)))
}

/// Ranked title + description + comment search within `workspace_id`, mapped to
/// wire [`IssueRow`]s in rank order (`hangar/issues_search`, e38.12).
///
/// Delegates the ranking to [`IssueRepo::search_ranked`] (a row matches when the
/// case-insensitive `query` substring appears in the issue title, description, OR
/// any comment body; title hits outrank description hits outrank comment-only
/// hits) and re-wraps each matched [`Issue`] into the same [`IssueRow`] shape
/// `issues_list` emits — including the P9 `pr_url` derivation — so a search hit is
/// byte-identical to the same issue in a list snapshot. Workspace-scoped: a
/// sibling tenant's matching issue is never returned, and an unknown workspace
/// yields an empty result.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the search query fails.
pub async fn issues_search(
    pool: &SqlitePool,
    workspace_id: &str,
    query: &str,
) -> Result<Vec<IssueRow>, sqlx::Error> {
    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let mut out = Vec::new();
    // `search_ranked` already returns rows in rank order (title > desc > comment,
    // then created_at, id), so the wire order is preserved one-for-one.
    for issue in IssueRepo::search_ranked(pool, workspace_id, query).await? {
        let id = IssueId::from_str(&issue.id).map_err(|e| sqlx::Error::ColumnDecode {
            index: "id".to_string(),
            source: format!("malformed issue id {:?}: {e}", issue.id).into(),
        })?;
        let display_id =
            issue_display_row(pool, workspace_id, &issue.id, prefix.as_deref()).await?;
        let pr_url = latest_pr_url_for_issue(pool, workspace_id, &issue.id).await?;
        let branch = latest_branch_for_issue(pool, workspace_id, &issue.id).await?;
        let extras = issue_card_fields(pool, &issue.id).await?;
        out.push(IssueRow {
            // multica parity #12: WHY this card is not running, from the newest
            // dispatch_attempt when that attempt was a decline. All `None` on a
            // healthy card, so the row grows by zero keys.
            last_dispatch_reason: extras.last_dispatch_reason,
            last_dispatch_detail: extras.last_dispatch_detail,
            last_dispatch_at: extras.last_dispatch_at,
            // ORIGIN PROVENANCE (0056): echoed from the stored pair so the wire
            // row a snapshot carries and the row an event pushes agree.
            origin_type: issue.origin.as_ref().map(|o| o.kind_db_str().to_string()),
            origin_id: issue.origin.as_ref().and_then(|o| o.id().map(ToString::to_string)),
            id,
            display_id,
            workspace_id: issue.workspace_id,
            title: issue.title,
            description: issue.description,
            state: issue.state,
            assignee: issue.assignee.map(|a| format!("{}:{}", a.kind().as_str(), a.id())),
            creator: format!("{}:{}", issue.creator.kind().as_str(), issue.creator.id()),
            created_at: issue.created_at,
            priority: issue.priority,
            due_date: issue.due_date,
            labels: issue.labels,
            pr_url,
            branch,
            repo_ref: extras.repo_ref,
            agent: extras.agent,
            source_branch: extras.source_branch,
            target_branch: extras.target_branch,
            external_ref: issue.external_ref,
            run_count: extras.run_count,
            last_run_status: extras.last_run_status,
            last_run_at: extras.last_run_at,
            parent_id: extras.parent_id,
            child_total: extras.child_total,
            child_done: extras.child_done,
            acceptance_criteria: criteria_texts(&issue.acceptance_criteria),
            acceptance: issue.acceptance_criteria,
            context_refs: issue.context_refs,
            dependencies: Vec::new(),
        });
    }
    Ok(out)
}

/// Ranked cross-entity search within `workspace_id`, mapped to the proto wire
/// [`SearchEntry`]s the command palette renders + jumps from (`hangar/search`,
/// e38.13).
///
/// Delegates the ranking + workspace scoping to
/// [`ainb_hangar_store::repo::search::cross_entity_search`] (a match across the
/// issue title / agent name / skill name / autopilot name, ranked exact > prefix >
/// substring then kind then label) and maps each [`SearchHit`] into the wire
/// [`SearchEntry`], deriving the jump-target `screen` token from the entry kind so
/// the plugin needs no kind→screen table of its own. Order is preserved
/// one-for-one. Workspace-scoped: an unknown workspace yields an empty result.
///
/// [`SearchHit`]: ainb_hangar_store::repo::search::SearchHit
/// [`SearchEntry`]: ainb_hangar_proto::snapshots::SearchEntry
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the underlying search query fails.
pub async fn search(
    pool: &SqlitePool,
    workspace_id: &str,
    query: &str,
) -> Result<Vec<ainb_hangar_proto::snapshots::SearchEntry>, sqlx::Error> {
    use ainb_hangar_proto::snapshots::{SearchEntry, SearchEntryKind};
    use ainb_hangar_store::repo::search::{SearchHitKind, cross_entity_search};
    let hits = cross_entity_search(pool, workspace_id, query).await?;
    Ok(hits
        .into_iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchHitKind::Issue => SearchEntryKind::Issue,
                SearchHitKind::Agent => SearchEntryKind::Agent,
                SearchHitKind::Skill => SearchEntryKind::Skill,
                SearchHitKind::Autopilot => SearchEntryKind::Autopilot,
            };
            SearchEntry {
                kind,
                id: hit.id,
                label: hit.label,
                screen: kind.target_screen().to_string(),
            }
        })
        .collect())
}

/// The PR URL captured into the latest completed task's `result.pr_url` for
/// `issue_id` in `workspace_id`, or `None` when no task on the issue produced a
/// PR (P9.2).
///
/// Reads `result->>'pr_url'` directly via `SQLite`'s JSON1 operator over the
/// `agent_task_queue` rows for the issue, taking the most recently finished
/// task that actually carries a non-NULL `pr_url`. The `WHERE` clause filters to
/// rows whose `result` JSON has the key, so a task with no PR (the byte-identical
/// pre-P9 `result` shape) is skipped, never surfaced as an empty string.
///
/// Scoped to the issue's LATEST run generation (migration 0039): a rerun mints a
/// fresh generation, so a superseded run's PR never leaks onto the current view.
/// A latest-generation run that has not yet opened a PR reads back `None` rather
/// than an older generation's stale URL — consistent with the card-state folds.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
async fn latest_pr_url_for_issue(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let url: Option<String> = sqlx::query_scalar(
        "SELECT result ->> 'pr_url' AS pr_url \
         FROM agent_task_queue \
         WHERE workspace_id = ?1 AND issue_id = ?2 \
           AND result ->> 'pr_url' IS NOT NULL \
           AND generation = (SELECT MAX(generation) FROM agent_task_queue \
                             WHERE issue_id = ?2) \
         ORDER BY COALESCE(finished_at, created_at) DESC, id DESC \
         LIMIT 1",
    )
    .bind(workspace_id)
    .bind(issue_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(url)
}

/// The `ainb/<slug>` worktree branch of the latest completed task for `issue_id`
/// in `workspace_id`, or `None` when no task on the issue committed a branch
/// (tcp ch3).
///
/// Mirrors [`latest_pr_url_for_issue`]: reads the `branch` column of the
/// `agent_task_queue` rows for the issue, taking the most recently finished task
/// that carries a non-empty branch. The `WHERE` clause skips rows with no branch
/// (a run that made no commits), so a branchless issue yields `None`, never an
/// empty string. Surfaces the branch on the task-detail view opened from the
/// ISSUE LIST — a synthetic task that carries no single per-run branch of its own.
///
/// Scoped to the issue's LATEST run generation (migration 0039), exactly like
/// [`latest_pr_url_for_issue`]: a rerun mints a fresh generation, so a superseded
/// run's branch never leaks onto the current view, and a latest-generation run
/// that has committed nothing yet reads back `None`.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
async fn latest_branch_for_issue(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let branch: Option<String> = sqlx::query_scalar(
        "SELECT branch \
         FROM agent_task_queue \
         WHERE workspace_id = ?1 AND issue_id = ?2 \
           AND branch IS NOT NULL AND branch <> '' \
           AND generation = (SELECT MAX(generation) FROM agent_task_queue \
                             WHERE issue_id = ?2) \
         ORDER BY COALESCE(finished_at, created_at) DESC, id DESC \
         LIMIT 1",
    )
    .bind(workspace_id)
    .bind(issue_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(branch)
}

/// Snapshot the assignable actors of `workspace_id` — human members and agents
/// in one polymorphic [`ActorRow`] list.
///
/// Agents lead (they are the common assignee at v1), each carrying the presence
/// derived from its runtime's status + heartbeat age against `now_ms`; members
/// follow. Recent-use ranking is a later-phase concern, so every row is
/// `recent_rank: None` (the picker falls back to its alphabetical body, which is
/// deterministic).
///
/// `now_ms` is injected (production passes
/// [`SystemClock`](ainb_hangar_core::clock::SystemClock)) so the staleness fold
/// is deterministic under test.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the agent / runtime / member queries fail.
pub async fn agents_list(
    pool: &SqlitePool,
    workspace_id: &str,
    now_ms: i64,
) -> Result<Vec<ActorRow>, sqlx::Error> {
    let mut out = Vec::new();

    // Fetch the whole workspace's live task counts ONCE (multica buildPresenceMap)
    // so the per-agent workload dimension is an O(1) map lookup, not an N+1 query.
    let workload_map = TaskRepo::live_workload_by_workspace(pool, workspace_id).await?;

    for agent in AgentRepo::list_by_workspace(pool, workspace_id).await? {
        let (running, queued) = workload_map.get(&agent.id).copied().unwrap_or((0, 0));
        out.push(agent_actor_row_with_counts(pool, &agent, running, queued, now_ms).await?);
    }

    for member in members_of(pool, workspace_id).await? {
        out.push(ActorRow {
            actor_ref: format!("member:{}", member.user_id),
            display_name: member.email,
            subtitle: member.role,
            presence: PresenceState::Online,
            // A human member carries no live task workload — always Idle.
            workload: Workload::Idle,
            is_agent: false,
            recent_rank: None,
            // A human member carries no agent metadata.
            ..ActorRow::default()
        });
    }

    Ok(out)
}

/// Snapshot the human members of `workspace_id` as wire
/// [`MemberWireRow`](ainb_hangar_proto::snapshots::MemberWireRow)s for the
/// settings Members pane (`hangar/members_list`, e38.11).
///
/// Reuses the store's [`MemberRepo`](ainb_hangar_store::repo::member::MemberRepo)
/// (the `member` × `user` join, ordered by email) and maps each row onto its wire
/// shape. Workspace-scoped: a foreign / unknown workspace yields an empty vec.
/// Shared by the list RPC and the refreshed view the set-role / remove mutations
/// answer with, so the pane re-renders identically either way.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the member query fails.
pub async fn members_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<ainb_hangar_proto::snapshots::MemberWireRow>, sqlx::Error> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::MemberRepo;

    // A malformed (empty) workspace id resolves to no members, not an error.
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(Vec::new());
    };
    let members = MemberRepo::list(pool, &ws).await?;
    Ok(members
        .into_iter()
        .map(|m| ainb_hangar_proto::snapshots::MemberWireRow {
            user_id: m.user_id,
            email: m.email,
            role: m.role,
        })
        .collect())
}

/// Snapshot the squads of `workspace_id` as wire
/// [`SquadWireRow`](ainb_hangar_proto::snapshots::SquadWireRow)s for the
/// `ainb hangar squad list` status view (`hangar/squads_list`, e38.17).
///
/// Reuses the store's [`SquadRepo`](ainb_hangar_store::repo::squad::SquadRepo)
/// (squads ordered by name, each with its leader + member actor-refs) and renders
/// every actor-ref to its canonical `member:<id>` / `agent:<id>` string.
/// Workspace-scoped: a foreign / unknown workspace yields an empty vec. Shared by
/// the list RPC and the refreshed view the create / member mutations answer with.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the squad query fails.
pub async fn squads_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<ainb_hangar_proto::snapshots::SquadWireRow>, sqlx::Error> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    // A malformed (empty) workspace id resolves to no squads, not an error.
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(Vec::new());
    };
    let squads = SquadRepo::list(pool, &ws).await?;
    Ok(squads
        .into_iter()
        .map(|s| ainb_hangar_proto::snapshots::SquadWireRow {
            id: s.id,
            name: s.name,
            leader: s.leader.to_string(),
            members: s.members.iter().map(|m| m.actor.to_string()).collect(),
            archived: s.archived,
            archived_at: s.archived_at,
            archived_by: s.archived_by.as_ref().map(ToString::to_string).unwrap_or_default(),
            instructions: s.instructions,
            // Only ROLED memberships ride the wire (parity #25): a roleless
            // squad omits the field entirely, keeping the payload
            // byte-identical to a pre-0053 producer's. `members` above stays
            // the ordering authority — a consumer joins these by `member`.
            member_roles: s
                .members
                .iter()
                .filter(|m| !m.role.is_empty())
                .map(|m| ainb_hangar_proto::snapshots::SquadMemberWireRow {
                    member: m.actor.to_string(),
                    role: m.role.clone(),
                })
                .collect(),
        })
        .collect())
}

/// Snapshot a workspace's user-defined kanban boards for
/// [`HANGAR_BOARDS_LIST`](ainb_hangar_proto::methods::HANGAR_BOARDS_LIST) (P4).
///
/// Reads the boards + columns + card placements from [`BoardRepo`], then enriches
/// each card with its issue title, a short display id, and the issue's LATEST
/// task status (which drives the card-green-on-`done` render). Cards are bucketed
/// into their column; a card whose column was deleted lands in the board's
/// `unmapped` pool (no data loss). A malformed / unknown workspace resolves to no
/// boards, not an error.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if a query fails.
pub async fn boards_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<ainb_hangar_proto::snapshots::BoardWireRow>, sqlx::Error> {
    use ainb_hangar_proto::snapshots::{BoardColumnWireRow, BoardWireRow};
    use ainb_hangar_store::repo::board::BoardRepo;

    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(Vec::new());
    };
    let boards = BoardRepo::list(pool, &ws).await?;
    let mut out = Vec::with_capacity(boards.len());
    for b in boards {
        let mut columns: Vec<BoardColumnWireRow> = b
            .columns
            .iter()
            .map(|c| BoardColumnWireRow {
                id: c.id.clone(),
                name: c.name.clone(),
                ord: c.ord,
                fsm_state: c.fsm_state.clone(),
                auto_move: c.auto_move,
                cards: Vec::new(),
            })
            .collect();
        let mut unmapped = Vec::new();
        for card in &b.cards {
            let wire = enrich_board_card(pool, ws.as_str(), &card.issue_id).await?;
            match card.column_id.as_deref() {
                Some(cid) => match columns.iter_mut().find(|c| c.id == cid) {
                    Some(col) => col.cards.push(wire),
                    // A dangling column_id (shouldn't happen under the FK) parks
                    // the card unmapped rather than dropping it.
                    None => unmapped.push(wire),
                },
                None => unmapped.push(wire),
            }
        }
        out.push(BoardWireRow {
            id: b.id,
            name: b.name,
            auto_move: b.auto_move,
            columns,
            unmapped,
        });
    }
    Ok(out)
}

/// Build the render row for one board card: the issue title + a short display id
/// + the issue's latest task status (`done` turns the card green). A missing
/// issue title falls back to the raw id so a card never renders blank.
async fn enrich_board_card(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<ainb_hangar_proto::snapshots::BoardCardWireRow, sqlx::Error> {
    // The issue title + its persisted card repo/agent (F2/F4) — the F6 card-edit
    // overlay prefills its repo pick + agent chip from the latter two.
    let issue_row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT title, repo_ref, agent_kind FROM issue WHERE id = ? AND workspace_id = ?",
    )
    .bind(issue_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?;
    let (title, repo_ref, agent) = match issue_row {
        Some((t, repo, agent)) => (Some(t), repo, agent),
        None => (None, None, None),
    };
    // The card's live state is the issue's AGGREGATE terminal outcome once its
    // latest run has drained (migration 0039 / tcp 8ln, codex F3): a squad card whose
    // leader FAILED but whose newest member finished `done` must render `failed`, not
    // the newest single task's `done`. While the run is still active (aggregate is
    // None) the card shows its newest task's status (running / queued); a never-run
    // card shows None. The tmux `session_name` still comes off the newest task so the
    // attach-from-card affordance can surface `tmux attach -t <session_name>`.
    let aggregate = TaskRepo::issue_aggregate_terminal_state(pool, workspace_id, issue_id).await?;
    let latest: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT status, session_name FROM agent_task_queue \
         WHERE issue_id = ? AND workspace_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(issue_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?;
    let (latest_state, session_name) = match latest {
        Some((status, session)) => (Some(status), session),
        None => (None, None),
    };
    // Drained → the aggregate token; still active / never-run → the newest task's raw
    // status (or None). This is the F3 fix: the card no longer reads a lone sibling.
    let state = aggregate.or(latest_state);

    // tcp T4 / F7: the card's squad assignment, its per-member task chips (only for
    // a squad card), its unfinished-blocker refs (the 🔒 blocked-state), and its
    // auto-run flag. Each is a small, self-contained fold the board renders.
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
    use ainb_hangar_store::repo::card_parity::CardParityRepo;

    let squad_id = CardParityRepo::get_issue_squad(pool, issue_id).await?;
    let member_states = match squad_id.as_deref() {
        Some(_) => squad_member_chips(pool, workspace_id, issue_id).await?,
        None => Vec::new(),
    };
    let blocked_by = CardDependencyRepo::unfinished_blockers_of(pool, issue_id)
        .await?
        .iter()
        .map(|b| short_display_id(b))
        .collect();
    let auto_run = CardDependencyRepo::get_auto_run(pool, issue_id).await?;
    // multica parity #20: the REVERSE direction (`blocks`) and the non-gating
    // `related` set. Render-only — neither touches `blocked_by` (still UNFINISHED
    // blockers only) nor the card's runnable state.
    let blocks = CardDependencyRepo::blocks_of(pool, issue_id)
        .await?
        .iter()
        .map(|b| short_display_id(b))
        .collect();
    let related = CardDependencyRepo::related_of(pool, issue_id)
        .await?
        .iter()
        .map(|b| short_display_id(b))
        .collect();

    Ok(ainb_hangar_proto::snapshots::BoardCardWireRow {
        issue_id: issue_id.to_string(),
        title: title.unwrap_or_else(|| issue_id.to_string()),
        display_id: short_display_id(issue_id),
        state,
        session_name,
        repo_ref,
        agent,
        squad_id,
        member_states,
        blocked_by,
        auto_run,
        blocks,
        related,
    })
}

/// The per-member task chips for a SQUAD card (tcp T4 / F7): the LATEST task per
/// distinct agent on the card's issue, with the agent's display name + that task's
/// status. A squad run fans out one task per member agent (all on the one issue),
/// so this reads back one chip per member that has a task, ordered by agent name.
async fn squad_member_chips(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Vec<ainb_hangar_proto::snapshots::CardMemberChip>, sqlx::Error> {
    // The latest task per agent WITHIN the issue's latest run generation (migration
    // 0039 / tcp 8ln): a rerun with fewer members must not keep showing a prior
    // generation's member chip, and the correlated subquery picks each agent's most
    // recent task in that generation, joined to the agent name.
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT t.agent_id, a.name, t.status \
         FROM agent_task_queue t \
         JOIN agent a ON a.id = t.agent_id \
         WHERE t.issue_id = ? AND t.workspace_id = ? \
           AND t.generation = (SELECT MAX(g.generation) FROM agent_task_queue g \
                               WHERE g.issue_id = t.issue_id) \
           AND t.id = ( \
             SELECT t2.id FROM agent_task_queue t2 \
             WHERE t2.issue_id = t.issue_id AND t2.agent_id = t.agent_id \
               AND t2.generation = t.generation \
             ORDER BY t2.created_at DESC, t2.id DESC LIMIT 1 \
           ) \
         ORDER BY a.name, t.agent_id",
    )
    .bind(issue_id)
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(agent_id, agent_name, status)| ainb_hangar_proto::snapshots::CardMemberChip {
                agent_id,
                agent_name,
                state: Some(status),
            },
        )
        .collect())
}

/// A short, stable card display id: the last 6 chars of the issue id (char-safe),
/// or the whole id when it is already short.
pub(crate) fn short_display_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 6 {
        return id.to_string();
    }
    id.chars().skip(n - 6).collect()
}

/// Snapshot the skills of `workspace_id`, mapped to wire [`SkillRow`]s.
///
/// `used` reflects whether any agent references the skill (via the `agent_skill`
/// join); `updated_at` is `0` at P4 (the curated-source stamp lands with the
/// importer in P6).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the skill / join queries fail.
pub async fn skills_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<SkillRow>, sqlx::Error> {
    let used_ids = used_skill_ids(pool).await?;
    let mut out = Vec::new();
    for skill in SkillRepo::list_by_workspace(pool, workspace_id).await? {
        out.push(SkillRow {
            used: used_ids.contains(&skill.id),
            slug: skill.id.clone(),
            name: skill.name,
            updated_at: 0,
        });
    }
    Ok(out)
}

/// Map one store [`Agent`](ainb_hangar_store::repo::agent::Agent) onto its picker
/// [`ActorRow`], deriving presence from the backing runtime's status + heartbeat
/// age and the workload dimension from the agent's OWN live task counts.
///
/// Shared by the e38.15 agent-CRUD wrappers ([`agent_update`] / [`agent_archive`])
/// so the row a mutation answers with is byte-identical to the same agent's
/// `agents_list` row. Fetches the single-agent live counts itself (the batch
/// [`agents_list`] path instead resolves them once and calls
/// [`agent_actor_row_with_counts`] directly, avoiding an N+1 query).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the runtime or task-count lookup fails.
async fn agent_actor_row(
    pool: &SqlitePool,
    agent: &ainb_hangar_store::repo::agent::Agent,
    now_ms: i64,
) -> Result<ActorRow, sqlx::Error> {
    let (running, queued) = TaskRepo::live_workload_for_agent(pool, &agent.id).await?;
    agent_actor_row_with_counts(pool, agent, running, queued, now_ms).await
}

/// Build one agent's picker [`ActorRow`] from its store row plus already-resolved
/// live task counts (`running`, `queued`), deriving presence from the runtime via
/// [`PresenceState::derive`] (status folded with heartbeat age, measured against
/// the injected `now_ms`) and workload via [`Workload::derive`]. The counts-taking
/// seam lets [`agents_list`] batch the workload query once for the whole
/// workspace. A missing runtime maps to `Offline` (the runtime FK is required,
/// but a deleted-out-of-band runtime must not panic the snapshot).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the runtime lookup fails.
async fn agent_actor_row_with_counts(
    pool: &SqlitePool,
    agent: &ainb_hangar_store::repo::agent::Agent,
    running: i64,
    queued: i64,
    now_ms: i64,
) -> Result<ActorRow, sqlx::Error> {
    let presence = match AgentRuntimeRepo::get(pool, &agent.runtime_id).await? {
        Some(rt) => PresenceState::derive(&rt.status, rt.last_seen_at, now_ms),
        None => PresenceState::Offline,
    };
    Ok(ActorRow {
        actor_ref: format!("agent:{}", agent.id),
        display_name: agent.name.clone(),
        subtitle: "agent".to_string(),
        presence,
        workload: Workload::derive(running, queued),
        is_agent: true,
        recent_rank: None,
        // Migration 0050 metadata. Both are omitted from the wire when empty, so
        // a metadata-less agent serialises exactly as it did pre-0050.
        description: agent.description.clone(),
        avatar: agent.avatar_url.clone().unwrap_or_default(),
        // Migration 0052 archive audit. Both are omitted from the wire when
        // unset, so an active (or pre-0052-archived) agent serialises exactly as
        // it did before.
        archived_at: agent.archived_at,
        archived_by: agent.archived_by.as_ref().map(ToString::to_string).unwrap_or_default(),
        // Parity #30. This is the ONLY place a per-agent env reaches the wire,
        // and it carries KEY NAMES + a count — never a value. All three are
        // omitted when the agent has no env, so an env-less agent serialises
        // exactly as it did pre-#30.
        agent_env_key_count: u32::try_from(agent.agent_env.len()).unwrap_or(u32::MAX),
        agent_env_keys: agent.agent_env.keys().map(ToString::to_string).collect(),
        agent_env_redacted: !agent.agent_env.is_empty(),
    })
}

/// Edit one agent's config knobs, scoped to `workspace_id`, then re-read the row
/// as a wire [`ActorRow`] (`hangar/agent_update`, e38.15).
///
/// `update` is the already-validated partial edit (the daemon maps the wire
/// params before this call). The write is workspace-scoped at the SQL boundary,
/// so a foreign-tenant agent id touches no row. Returns `Some(row)` with the
/// refreshed agent when exactly one row was edited, `None` when the
/// `(id, workspace)` pair matched nothing (the not-found / cross-tenant case the
/// caller surfaces as an error). The re-read reuses [`agent_actor_row`] so the
/// response row matches an `agents_list` snapshot of the agent.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault, or a malformed stored row on the
/// re-read.
pub async fn agent_update(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_id: &str,
    update: &ainb_hangar_store::repo::agent::AgentConfigUpdate,
    now_ms: i64,
) -> Result<Option<ActorRow>, sqlx::Error> {
    let touched = AgentRepo::update_config(pool, workspace_id, agent_id, update).await?;
    if !touched {
        return Ok(None);
    }
    match AgentRepo::get(pool, agent_id).await? {
        Some(agent) => Ok(Some(agent_actor_row(pool, &agent, now_ms).await?)),
        None => Ok(None),
    }
}

/// Archive or un-archive one agent, scoped to `workspace_id`, then re-read the
/// row as a wire [`ActorRow`] (`hangar/agent_archive`, e38.15).
///
/// Workspace-scoped at the SQL boundary: a foreign-tenant agent id flips no row.
/// Returns `Some(row)` with the refreshed agent when the flip landed, `None` when
/// the `(id, workspace)` pair matched nothing (the not-found / cross-tenant case
/// the caller surfaces as an error).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault, or a malformed stored row on the
/// re-read.
pub async fn agent_archive(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_id: &str,
    archived: bool,
    archived_by: Option<&str>,
    now_ms: i64,
) -> Result<Option<ActorRow>, sqlx::Error> {
    let by = effective_archiver(pool, workspace_id, archived_by).await?;
    let touched =
        AgentRepo::set_archived(pool, workspace_id, agent_id, archived, by.as_ref(), now_ms)
            .await?;
    if !touched {
        return Ok(None);
    }
    match AgentRepo::get(pool, agent_id).await? {
        Some(agent) => Ok(Some(agent_actor_row(pool, &agent, now_ms).await?)),
        None => Ok(None),
    }
}

/// Resolve the actor recorded as `archived_by` (migration 0052, parity #26):
///
/// 1. an explicitly supplied user id (trimmed; blank counts as absent), else
/// 2. the workspace OWNER — the single-operator default, mirroring the gap-#8
///    invoker resolution — else
/// 3. `None`, an honestly unattributed archive (an owner-less workspace).
///
/// One helper so the agent and squad handlers can never drift apart on who gets
/// blamed for an archive.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the owner lookup fails.
async fn effective_archiver(
    pool: &SqlitePool,
    workspace_id: &str,
    supplied: Option<&str>,
) -> Result<Option<ActorRef>, sqlx::Error> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::workspace::WorkspaceRepo;

    if let Some(id) = supplied.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(ActorRef::new(ActorKind::Member, id).ok());
    }
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(None);
    };
    let owner = WorkspaceRepo::owner_id(pool, &ws).await?;
    Ok(owner.and_then(|id| ActorRef::new(ActorKind::Member, id).ok()))
}

/// Archive or un-archive one squad, scoped to `workspace_id`, recording WHO and
/// WHEN, then re-read the workspace's ACTIVE squads
/// (`hangar/squad_archive`, parity #26).
///
/// Workspace-scoped through [`SquadRepo::set_archived`]'s tenant guard: a
/// foreign-tenant squad id flips no row and resolves to `None` (the not-found
/// case the caller surfaces as an error). The refreshed list is active-only, so
/// a just-archived squad is absent from the response — which is the observable
/// outcome the caller renders.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault or a malformed stored row.
pub async fn squad_archive(
    pool: &SqlitePool,
    workspace_id: &str,
    squad_id: &str,
    archived: bool,
    archived_by: Option<&str>,
    now_ms: i64,
) -> Result<Option<Vec<ainb_hangar_proto::snapshots::SquadWireRow>>, sqlx::Error> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::{SquadRepo, SquadRepoError};

    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(None);
    };
    let by = effective_archiver(pool, workspace_id, archived_by).await?;
    match SquadRepo::set_archived(pool, &ws, squad_id, archived, by.as_ref(), now_ms).await {
        Ok(()) => {}
        // `DuplicateName` is structurally impossible here (an archive never
        // renames), but folding it into the not-found rejection keeps a future
        // store change from panicking a live daemon connection.
        Err(SquadRepoError::NotFound | SquadRepoError::DuplicateName) => return Ok(None),
        Err(SquadRepoError::Db(e)) => return Err(e),
    }
    squads_list(pool, workspace_id).await.map(Some)
}

/// A workspace member joined with its user record (for the picker row).
struct MemberRow {
    user_id: String,
    email: String,
    role: String,
}

/// Join `member` × `user` to materialise the human actors of a workspace.
async fn members_of(pool: &SqlitePool, workspace_id: &str) -> Result<Vec<MemberRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT m.user_id AS user_id, u.email AS email, m.role AS role \
         FROM member m JOIN user u ON u.id = m.user_id \
         WHERE m.workspace_id = ? ORDER BY u.email",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(MemberRow {
                user_id: r.try_get("user_id")?,
                email: r.try_get("email")?,
                role: r.try_get("role")?,
            })
        })
        .collect()
}

/// The set of skill ids referenced by at least one agent (drives `used`).
async fn used_skill_ids(
    pool: &SqlitePool,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT DISTINCT skill_id FROM agent_skill")
        .fetch_all(pool)
        .await?;
    Ok(ids.into_iter().collect())
}

// ──────────────────────────────────────────────────────────────────────────
// P6.5 — skill detail / sync / attach / detach handlers.
//
// Every one resolves the subscribed `workspace` (already mapped slug→id by the
// dispatcher) and threads it into the secured `SkillRepo` by-id methods, so a
// skill or agent id minted in another tenant can never be read or mutated here.
// ──────────────────────────────────────────────────────────────────────────

/// Fetch one skill's full detail (`hangar/skill_get`), scoped to `workspace`.
///
/// Returns `None` when the id resolves to no skill in `workspace` (a foreign id
/// reads as absent, never another tenant's body). The wire [`SkillDetail`]
/// carries the SKILL.md body plus the path-ordered file list the detail pane's
/// file tree renders.
///
/// # Errors
///
/// Returns a [`SkillRepoError`] on a store failure or a corrupt stored row.
pub async fn skill_get(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    skill_id: &SkillId,
) -> Result<Option<SkillDetail>, SkillRepoError> {
    let Some(skill) = SkillRepo::get(pool, workspace, skill_id).await? else {
        return Ok(None);
    };
    Ok(Some(SkillDetail {
        slug: skill.id.as_str().to_string(),
        name: skill.name.as_str().to_string(),
        description: skill.description,
        body: skill.content,
        files: skill.files.into_iter().map(|f| SkillFile { path: f.path }).collect(),
    }))
}

/// Import the curated toolkit skills into `workspace` (`hangar/skills_sync`).
///
/// `source` is the resolved source directory (the caller maps an absent
/// `source_path` to [`crate::skills_sync::default_source_dir`]). Returns the
/// imported names + count for the plugin's "Imported N skills" toast.
///
/// # Errors
///
/// Returns a [`crate::skills_sync::SyncError`] when the source can't be walked,
/// a `SKILL.md` is malformed, or a store write fails (all-or-nothing).
pub async fn skills_sync(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    source: &std::path::Path,
) -> Result<SkillsSyncResult, crate::skills_sync::SyncError> {
    let report = crate::skills_sync::skills_sync_from(pool, workspace, source).await?;
    let imported: Vec<String> = report.imported.into_iter().map(|(name, _id)| name).collect();
    let count = imported.len();
    Ok(SkillsSyncResult { imported, count })
}

/// Attach a skill to an agent within `workspace` (`hangar/skill_attach`).
///
/// Delegates to the secured [`SkillRepo::attach_to_agent`], which verifies both
/// ids belong to `workspace` before touching `agent_skill` (the tenant guard);
/// a cross-workspace id pair surfaces as [`SkillRepoError::CrossWorkspace`].
///
/// # Errors
///
/// Returns [`SkillRepoError::CrossWorkspace`] when either id is foreign, or
/// [`SkillRepoError::Db`] on a store failure.
pub async fn skill_attach(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    agent: &AgentId,
    skill: &SkillId,
) -> Result<(), SkillRepoError> {
    SkillRepo::attach_to_agent(pool, workspace, agent, skill).await
}

/// Detach a skill from an agent within `workspace` (`hangar/skill_detach`).
///
/// Idempotent + workspace-scoped, mirroring [`skill_attach`].
///
/// # Errors
///
/// Returns [`SkillRepoError::CrossWorkspace`] when either id is foreign, or
/// [`SkillRepoError::Db`] on a store failure.
pub async fn skill_detach(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    agent: &AgentId,
    skill: &SkillId,
) -> Result<(), SkillRepoError> {
    SkillRepo::detach_from_agent(pool, workspace, agent, skill).await
}

/// Flip one agent↔skill link's enablement (`hangar/skill_set_enabled`, parity
/// #24).
///
/// Orthogonal to attach/detach: the junction row survives, it just stops being
/// live. Answers `false` when the pair is not attached (a no-op, not an error)
/// so the caller can tell "toggled" from "no such link". Workspace-scoped by the
/// same guard [`skill_attach`] uses.
///
/// # Errors
///
/// Returns [`SkillRepoError::CrossWorkspace`] when either id is foreign, or
/// [`SkillRepoError::Db`] on a store failure.
pub async fn skill_set_enabled(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    agent: &AgentId,
    skill: &SkillId,
    enabled: bool,
) -> Result<bool, SkillRepoError> {
    SkillRepo::set_enabled(pool, workspace, agent, skill, enabled).await
}

/// List one agent's skill links with their enablement
/// (`hangar/agent_skills_list`, parity #24).
///
/// Returns EVERY link — a disabled one is still attached and still listed, just
/// flagged — so the skill-manager can render the `(disabled)` marker. A foreign
/// agent id yields an empty list.
///
/// # Errors
///
/// Returns [`SkillRepoError::Db`] on a store failure or
/// [`SkillRepoError::Name`] on a corrupt stored name.
pub async fn agent_skills_list(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    agent: &AgentId,
) -> Result<AgentSkillsListResult, SkillRepoError> {
    let links = SkillRepo::agent_skill_links(pool, workspace, agent).await?;
    Ok(AgentSkillsListResult {
        links: links
            .into_iter()
            .map(|l| AgentSkillLinkRow {
                skill_id: l.skill_id.as_str().to_string(),
                name: l.name.as_str().to_string(),
                enabled: l.enabled,
            })
            .collect(),
    })
}

// ──────────────────────────────────────────────────────────────────────────
// P7.5 — autopilot manager: list / runs / fire-now / set-enabled handlers.
//
// Every one resolves the subscribed `workspace` (already mapped slug→id by the
// dispatcher) and threads it into the workspace-scoped `AutopilotRepo`
// by-id methods, so an autopilot id minted in another tenant can never be read,
// fired, or toggled here.
// ──────────────────────────────────────────────────────────────────────────

/// How many recent runs to inspect when deriving an autopilot's `LAST RUN`
/// columns for the list snapshot.
const LAST_RUN_LOOKBACK: u32 = 1;

/// Snapshot the autopilots of `workspace`, mapped to wire [`AutopilotRow`]s
/// (`hangar/autopilots_list`, P7.5).
///
/// Each row carries the latest run's status + start instant (the `LAST RUN`
/// columns), pulled via the workspace-scoped [`AutopilotRepo::list_runs`] so the
/// derivation can never leak another tenant's history.
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] on a store failure or a corrupt stored id.
pub async fn autopilots_list(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
) -> Result<Vec<AutopilotRow>, AutopilotRepoError> {
    let autopilots = AutopilotRepo::list(pool, workspace).await?;
    // ONE query for the whole workspace's newest rule versions (multica parity
    // #14) — deliberately not N+1 per autopilot. Labels are resolved from the
    // same actor-label cache the version pane uses.
    let latest_versions = AutopilotRuleVersionRepo::latest_by_autopilot(pool, workspace).await?;
    let mut publisher_labels = ActorLabelCache::default();
    let mut versions: HashMap<String, (i64, Option<String>)> = HashMap::new();
    for (autopilot_id, version, published_by) in latest_versions {
        let label = match published_by.as_deref() {
            Some(actor) => Some(publisher_labels.label(pool, actor).await),
            None => None,
        };
        versions.insert(autopilot_id, (version, label));
    }
    let mut out = Vec::with_capacity(autopilots.len());
    for ap in autopilots {
        let id = AutopilotId::from_str(ap.id.clone()).map_err(|_| AutopilotRepoError::EmptyId)?;
        let id_str = ap.id.clone();
        let last = AutopilotRepo::list_runs(pool, workspace, &id, LAST_RUN_LOOKBACK)
            .await?
            .into_iter()
            .next();
        out.push(AutopilotRow {
            id: ap.id,
            workspace_id: ap.workspace_id,
            agent_id: ap.agent_id,
            name: ap.name,
            cron_expr: ap.cron_expr,
            next_tick_at: ap.next_tick_at,
            enabled: ap.enabled,
            last_run_status: last.as_ref().map(|r| r.status.clone()),
            last_run_at: last.as_ref().map(|r| r.started_at),
            api_trigger_enabled: ap.api_trigger_enabled,
            // `None` for an UNVERSIONED rule (pre-0061, never edited): the
            // ledger was deliberately not backfilled, so there is nothing
            // honest to report.
            rule_version: versions.get(&id_str).map(|(v, _)| *v),
            last_published_by: versions.get(&id_str).and_then(|(_, l)| l.clone()),
        });
    }
    Ok(out)
}

/// A tiny per-call cache resolving an actor ref (`member:<user.id>`) to a
/// human-readable label.
///
/// The plugin owns zero domain data, so the daemon does the `user` join. An
/// unresolvable ref renders the RAW actor ref — never a fabricated name.
#[derive(Default)]
struct ActorLabelCache {
    seen: HashMap<String, String>,
}

impl ActorLabelCache {
    async fn label(&mut self, pool: &SqlitePool, actor: &str) -> String {
        if let Some(hit) = self.seen.get(actor) {
            return hit.clone();
        }
        let resolved = match actor.parse::<ActorRef>() {
            Ok(a) if a.kind() == ActorKind::Member => {
                sqlx::query_scalar::<_, String>("SELECT email FROM user WHERE id = ?")
                    .bind(a.id())
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten()
            }
            Ok(a) => sqlx::query_scalar::<_, String>("SELECT name FROM agent WHERE id = ?")
                .bind(a.id())
                .fetch_optional(pool)
                .await
                .ok()
                .flatten(),
            Err(_) => None,
        }
        .unwrap_or_else(|| actor.to_string());
        self.seen.insert(actor.to_string(), resolved.clone());
        resolved
    }
}

/// Snapshot one autopilot's recent runs, latest-first (`hangar/autopilot_runs`,
/// P7.5), scoped to `workspace`.
///
/// A foreign autopilot id yields an empty run set (the repo join verifies the
/// workspace), never another tenant's history.
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] on a store failure.
pub async fn autopilot_runs(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    limit: u32,
) -> Result<Vec<AutopilotRunRow>, AutopilotRepoError> {
    let runs = AutopilotRepo::list_runs(pool, workspace, autopilot_id, limit).await?;
    Ok(runs
        .into_iter()
        .map(|r| AutopilotRunRow {
            id: r.id,
            autopilot_id: r.autopilot_id,
            started_at: r.started_at,
            completed_at: r.completed_at,
            status: r.status,
            source: r.source,
            failure_reason: r.failure_reason,
            accountable_actor: r.accountable_actor,
            attribution: r.attribution,
        })
        .collect())
}

/// Fire one autopilot's tick now (`hangar/autopilot_fire_now`, P7.5), scoped to
/// `workspace`.
///
/// Resolves the autopilot within `workspace` (a foreign id resolves to `None`
/// and fires nothing), then runs the P7.4 single-tx enqueue path. Returns `true`
/// when a tick was fired, `false` when the id resolved to no autopilot in this
/// tenant.
///
/// # Errors
///
/// Returns [`AutopilotFireError::Repo`] on a store failure resolving the row, or
/// [`AutopilotFireError::Fire`] when the enqueue transaction fails (e.g. the
/// autopilot's agent was deleted).
pub async fn autopilot_fire_now(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    actor: Option<&ActorRef>,
) -> Result<bool, AutopilotFireError> {
    let Some(autopilot) = AutopilotRepo::get(pool, workspace, autopilot_id).await? else {
        return Ok(false);
    };
    // A MANUAL fire: the operator's explicit override, stamped as such on the
    // run so the history can tell it from a scheduled or api-triggered tick.
    //
    // ATTRIBUTION FORK (multica parity #14): a NAMED human clicking "run now" is
    // `direct_human` — them, not the rule's owner. Without a named human it
    // falls back to `rule_owner`, exactly like an unattended fire.
    let attribution = match actor {
        Some(a) => RunAttribution::DirectHuman(a.clone()),
        None => RunAttribution::RuleOwner,
    };
    fire_autopilot_tick_with_attribution(pool, clock, &autopilot, RunSource::Manual, &attribution)
        .await?;
    Ok(true)
}

/// EDIT one autopilot's config (`hangar/autopilot_update`, multica parity #14),
/// scoped to `workspace`.
///
/// Thin pass-through to [`AutopilotRepo::update_as`], which does the real work:
/// revalidate a new cron before any write, apply the patch, then publish a rule
/// version IFF the edit was substantive. A rename lands but mints no version.
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] — [`AutopilotRepoError::Cron`] for a
/// malformed `cron_expr` (nothing written), otherwise a store failure.
pub async fn autopilot_update(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    edit: &AutopilotEdit,
    actor: Option<&ActorRef>,
) -> Result<UpdateOutcome, AutopilotRepoError> {
    AutopilotRepo::update_as(pool, clock, workspace, autopilot_id, edit, actor).await
}

/// Read one autopilot's rule-version ledger, newest-first
/// (`hangar/autopilot_versions`, multica parity #14), scoped to `workspace`.
///
/// Each row carries both the raw `published_by` actor ref AND a
/// `published_by_label` resolved daemon-side (the `user` join), so the plugin
/// owns zero domain data. An unresolvable ref renders the raw actor ref, never a
/// fabricated name.
///
/// An unversioned (pre-0061, never-edited) rule yields an empty list — the
/// ledger was deliberately not backfilled.
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] on a store failure.
pub async fn autopilot_versions(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    limit: u32,
) -> Result<Vec<AutopilotVersionRow>, AutopilotRepoError> {
    let rows = AutopilotRuleVersionRepo::list(pool, workspace, autopilot_id, limit).await?;
    let mut labels = ActorLabelCache::default();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let published_by_label = match r.published_by.as_deref() {
            Some(actor) => Some(labels.label(pool, actor).await),
            None => None,
        };
        out.push(AutopilotVersionRow {
            id: r.id,
            autopilot_id: r.autopilot_id,
            version: r.version,
            change_kind: r.change_kind,
            published_by: r.published_by,
            published_by_label,
            config_summary: r.config_summary,
            created_at: r.created_at,
        });
    }
    Ok(out)
}

/// What [`autopilot_trigger_api`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiTriggerOutcome {
    /// No such autopilot in this workspace. A foreign id leaks nothing.
    NotFound,
    /// The autopilot exists but has not armed `api_trigger_enabled`. NOTHING is
    /// written: the trigger does not exist, so there is nothing to skip (a
    /// `skipped` run records an ADMISSION decision, not a missing trigger).
    Disabled,
    /// The admission gate declined the dispatch; a terminal `skipped` run
    /// records it.
    Skipped {
        /// The recorded `skipped` run.
        run_id: String,
        /// The admission reason.
        reason: String,
    },
    /// The dispatch was admitted.
    Fired {
        /// The new run.
        run_id: String,
        /// The task enqueued against it.
        task_id: String,
    },
}

/// Fire one autopilot through its bare programmatic `api` trigger
/// (`hangar/autopilot_trigger_api`, migration 0057 / multica parity item 15),
/// scoped to `workspace`.
///
/// The `api` trigger is the third trigger surface after cron and webhook: no
/// cron expression, no HMAC — a caller with normal API access fires the
/// autopilot directly (multica `handler/autopilot.go:441`, where
/// `kind IN ('schedule','webhook','api')` and only `schedule` requires a cron
/// expression).
///
/// Two guards, in order:
///
/// 1. the autopilot must exist IN THIS WORKSPACE ([`ApiTriggerOutcome::NotFound`]),
/// 2. it must have armed `api_trigger_enabled` ([`ApiTriggerOutcome::Disabled`],
///    writing nothing).
///
/// Then it goes through the SAME [`dispatch_with_admission`] gate the scheduler
/// uses, so an api fire at the concurrency limit under the `skip` policy is
/// declined and recorded as a terminal `skipped` run stamped `source = 'api'`.
///
/// # Errors
///
/// Returns [`AutopilotFireError::Repo`] on a store failure resolving the row, or
/// [`AutopilotFireError::Fire`] when the dispatch fails (e.g. the autopilot's
/// agent was deleted).
pub async fn autopilot_trigger_api(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
) -> Result<ApiTriggerOutcome, AutopilotFireError> {
    let Some(autopilot) = AutopilotRepo::get(pool, workspace, autopilot_id).await? else {
        return Ok(ApiTriggerOutcome::NotFound);
    };
    if !autopilot.api_trigger_enabled {
        return Ok(ApiTriggerOutcome::Disabled);
    }
    Ok(
        match dispatch_with_admission(pool, clock, &autopilot, RunSource::Api).await? {
            DispatchOutcome::Fired {
                run_id, task_id, ..
            } => ApiTriggerOutcome::Fired {
                run_id: run_id.to_string(),
                task_id: task_id.to_string(),
            },
            DispatchOutcome::Skipped { run_id, reason, .. } => ApiTriggerOutcome::Skipped {
                run_id: run_id.to_string(),
                reason,
            },
        },
    )
}

/// Arm or disarm one autopilot's `api` trigger
/// (`hangar/autopilot_set_api_trigger`), scoped to `workspace`.
///
/// Returns `false` when the id resolved to no autopilot in this tenant (nothing
/// written).
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] on a store failure.
pub async fn autopilot_set_api_trigger(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    enabled: bool,
    actor: Option<&ActorRef>,
) -> Result<bool, AutopilotRepoError> {
    AutopilotRepo::set_api_trigger_enabled_as(pool, clock, workspace, autopilot_id, enabled, actor)
        .await
}

/// Enable or disable one autopilot (`hangar/autopilot_set_enabled`, P7.5),
/// scoped to `workspace`.
///
/// `enabled = false` calls the workspace-scoped [`AutopilotRepo::disable`];
/// `true` calls [`AutopilotRepo::enable`] (which recomputes `next_tick_at` from
/// now). A foreign id touches no row in either case.
///
/// # Errors
///
/// Returns an [`AutopilotRepoError`] on a store failure (or a corrupt-row cron
/// re-parse failure on enable).
pub async fn autopilot_set_enabled(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    enabled: bool,
    actor: Option<&ActorRef>,
) -> Result<(), AutopilotRepoError> {
    // Pausing / resuming is a SUBSTANTIVE publish on the rule (multica parity
    // #14): it changes whether the rule fires unattended, so it re-stamps who is
    // accountable for its runs.
    if enabled {
        AutopilotRepo::enable_as(pool, clock, workspace, autopilot_id, actor).await
    } else {
        AutopilotRepo::disable_as(pool, clock, workspace, autopilot_id, actor).await
    }
}

// ──────────────────────────────────────────────────────────────────────────
// P8.4 — Kanban board: tasks list + card-move transition handlers.
//
// Both resolve the subscribed `workspace` (already mapped slug→id by the
// dispatcher) and thread it into the workspace-scoped `TaskRepo`, so a task id
// minted in another tenant can never be read or moved here.
// ──────────────────────────────────────────────────────────────────────────

/// Snapshot every task in `workspace`, mapped to wire [`TaskCardRow`]s for the
/// Kanban board (`hangar/tasks_list`, P8.4 + tcp T2).
///
/// Carries every lifecycle status (terminal rows included) so the board can
/// bucket the six statuses into its four columns; a foreign workspace yields an
/// empty set.
///
/// tcp T2 surfacing: each card also carries the run's `branch` (the durable
/// `ainb/<slug>` recorded at finalize when the run committed), the `pr_url`
/// captured into the task's `result` (P9.1), and — ONLY for a card that has a
/// `pr_url` — the PR's CI + merge status fetched through the injectable
/// `provider` (the production `gh` subprocess, or a test fake / stub). Cards
/// without a PR incur no `gh` call.
///
/// tcp le3 — bounded total fetch: the DISTINCT PR urls are fetched CONCURRENTLY
/// (a [`tokio::task::JoinSet`], each fetch owning an `Arc`-clone of the provider),
/// so a board with N uncached PR'd cards costs ~one fetch timeout in wall-clock,
/// not N serial ones. Repeated urls collapse to a single fetch. The provider is
/// shared by `Arc` because a spawned task must own `'static` state.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store failure or a corrupt stored id.
pub async fn tasks_list(
    pool: &SqlitePool,
    workspace_id: &str,
    provider: Arc<dyn crate::pr_status::PrStatusProvider>,
) -> Result<Vec<TaskCardRow>, sqlx::Error> {
    let tasks = TaskRepo::list_by_workspace(pool, workspace_id).await?;

    // Fetch every DISTINCT PR url once, concurrently — the le3 bound. A card
    // without a PR contributes no url, so a PR-less board spawns nothing.
    let distinct_urls: HashSet<String> =
        tasks.iter().filter_map(|t| task_pr_url(t.result.as_deref())).collect();
    let statuses = fetch_pr_statuses(provider, distinct_urls).await;

    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks {
        let id = ainb_hangar_core::ids::TaskId::from_str(&t.id).map_err(|e| {
            sqlx::Error::ColumnDecode {
                index: "id".to_string(),
                source: format!("malformed task id {:?}: {e}", t.id).into(),
            }
        })?;
        let pr_url = task_pr_url(t.result.as_deref());
        // Every distinct url was fetched above, so a card WITH a PR always finds
        // its status; a card without a PR carries none.
        let pr_status = pr_url.as_deref().and_then(|url| statuses.get(url).copied());
        out.push(TaskCardRow {
            id,
            workspace_id: t.workspace_id,
            agent_id: t.agent_id,
            issue_id: t.issue_id,
            status: t.status,
            priority: t.priority,
            created_at: t.created_at,
            branch: t.branch,
            pr_url,
            pr_status,
        });
    }
    Ok(out)
}

/// Cap on the number of PR-status fetches in flight at once. Each fetch is a `gh`
/// subprocess hitting the GitHub API, so an unbounded fan-out over a large board
/// would fork-storm the host and trip GitHub's secondary rate limit. A workspace
/// with hundreds of PR'd cards therefore drains through this many-at-a-time
/// window rather than all-at-once; the wall-clock cost stays ~`ceil(N / cap)`
/// fetch timeouts, still far cheaper than N serial fetches.
const PR_FETCH_CONCURRENCY: usize = 8;

/// Fetch the [`PrStatus`](ainb_hangar_proto::pr_status::PrStatus) of every url in
/// `urls` concurrently but BOUNDED to [`PR_FETCH_CONCURRENCY`] in-flight fetches,
/// returning a `url → status` map (tcp le3).
///
/// Each url is fetched on its own [`tokio::task::JoinSet`] task holding an
/// `Arc`-clone of the shared `provider`; a shared [`Semaphore`] caps how many run
/// at once, so a board with N uncached PR'd cards costs ~`ceil(N / cap)` fetch
/// timeouts rather than either N serial ones or an N-wide `gh` fork storm.
/// A task that panics (a JoinError) is dropped from the map — that url's card then
/// renders no PR badge — never a propagated panic. An empty `urls` spawns nothing
/// and returns an empty map.
///
/// [`Semaphore`]: tokio::sync::Semaphore
async fn fetch_pr_statuses(
    provider: Arc<dyn crate::pr_status::PrStatusProvider>,
    urls: HashSet<String>,
) -> HashMap<String, ainb_hangar_proto::pr_status::PrStatus> {
    let limiter = Arc::new(tokio::sync::Semaphore::new(PR_FETCH_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let provider = Arc::clone(&provider);
        let limiter = Arc::clone(&limiter);
        set.spawn(async move {
            // Hold a permit for the fetch's duration, capping in-flight `gh`
            // subprocesses. The semaphore is never closed, so acquire cannot fail.
            let _permit =
                limiter.acquire_owned().await.expect("pr-fetch semaphore is never closed");
            let status = provider.fetch(&url).await;
            (url, status)
        });
    }
    let mut out = HashMap::with_capacity(set.len());
    while let Some(joined) = set.join_next().await {
        if let Ok((url, status)) = joined {
            out.insert(url, status);
        }
    }
    out
}

/// Extract the captured `pr_url` from a task's stored `result` JSON blob (P9.1),
/// or `None` when the run recorded no result or opened no PR.
///
/// Pure + total: a `None` result, unparseable JSON, or a `result` with no
/// (non-empty) `pr_url` key all yield `None` — never a false or empty URL.
fn task_pr_url(result_json: Option<&str>) -> Option<String> {
    let raw = result_json?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .get("pr_url")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Cap on the inbox entries one `hangar/inbox_list` snapshot returns. The inbox
/// is a digest, not an archive — the newest two hundred notifications are ample
/// for the screen, and the bound keeps a long-lived workspace's snapshot small.
const INBOX_LIST_LIMIT: i64 = 200;

/// Snapshot ONE ACTOR's aggregated inbox + their unread count in a workspace
/// (`hangar/inbox_list`, e38.14; per-recipient since store migration 0060).
///
/// Reads the durable `inbox_entry` rows the daemon's aggregator folds live
/// issue / comment / task events into and ADDRESSES to a single actor,
/// newest-first and capped at [`INBOX_LIST_LIMIT`], plus that recipient's count
/// of unread (`read_at IS NULL`) entries. Scoped on both axes: a foreign /
/// unknown workspace yields an empty list + zero unread, and another actor's
/// entries are never returned.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if either store query fails.
pub async fn inbox_list(
    pool: &SqlitePool,
    workspace_id: &str,
    recipient: &ActorRef,
) -> Result<(Vec<InboxEntryRow>, i64), sqlx::Error> {
    let entries = InboxRepo::list(pool, workspace_id, recipient, INBOX_LIST_LIMIT).await?;
    let unread = InboxRepo::unread_count(pool, workspace_id, recipient).await?;
    let rows = entries
        .into_iter()
        .map(|e| InboxEntryRow {
            id: e.id,
            kind: e.kind.as_str().to_string(),
            event: e.event,
            subject_id: e.subject_id,
            summary: e.summary,
            recipient: e.recipient.to_string(),
            created_at: e.created_at,
            read_at: e.read_at,
        })
        .collect();
    Ok((rows, unread))
}

/// Snapshot the OPEN control-plane attention rows for a scope
/// (`attention/list` / `attention/subscribe`, spec P2).
///
/// Three scopes (matching the store repo):
/// - `fleet = true` → EVERY open row across every workspace + the no-workspace
///   host sessions (the converged control centre's host-wide feed);
///   `workspace_id` is ignored.
/// - `fleet = false`, `workspace_id = Some(ws)` → that workspace's open rows.
/// - `fleet = false`, `workspace_id = None` → the open rows owned by NO
///   workspace (hand-started host sessions).
///
/// Rows are oldest-first (the longest-waiting request is the most urgent). The
/// caller has already resolved any wire workspace id to the real row id.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the list query fails.
pub async fn attention_list(
    pool: &SqlitePool,
    workspace_id: Option<&str>,
    fleet: bool,
) -> Result<Vec<AttentionRow>, sqlx::Error> {
    let rows = if fleet {
        AttentionRepo::list_fleet(pool).await?
    } else {
        AttentionRepo::list_open(pool, workspace_id).await?
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(attention_row_to_wire(pool, row).await?);
    }
    Ok(out)
}

/// Flatten one store `attention` row into its wire render shape, RESOLVING the
/// routing channels now for a row that carries no stamped set.
///
/// A row raised at or after migration 0037 stamps its resolved channels once at
/// emit time (compute-once-at-emit), and that stamp is used verbatim. But a
/// LEGACY row raised BEFORE 0037 persists the empty default (`''`); treating that
/// as "no channels" would silently drop an in-flight ASK from before the upgrade
/// off EVERY push channel. So an empty stamp is treated as "unstamped → resolve
/// the rules now" via [`NotifyRuleRepo::resolve`]: a genuinely board-only kind
/// (`waiting`, or a board-only override) resolves back to the empty set
/// (unchanged), while a legacy ASK / approval / escalation resolves to its real
/// push channels and pages exactly as it did before T5.
async fn attention_row_to_wire(
    pool: &SqlitePool,
    row: ainb_hangar_store::repo::attention::AttentionRow,
) -> Result<AttentionRow, sqlx::Error> {
    let channels = if row.channels.is_empty() {
        NotifyRuleRepo::resolve(pool, row.kind, row.workspace_id.as_deref()).await?
    } else {
        row.channels
    };
    Ok(AttentionRow {
        id: row.id,
        session_id: row.session_id,
        cwd: row.cwd,
        workspace_id: row.workspace_id,
        kind: row.kind.as_str().to_string(),
        payload: row.payload,
        degraded: row.degraded,
        created_at: row.created_at,
        channels,
    })
}

/// Snapshot a workspace's token/cost usage rollup (`hangar/usage_rollup`,
/// e38.35).
///
/// Reads the durable `task_usage` rows the daemon's run loop records at each
/// task's finalize seam: the grand totals (summed tokens in/out + cost + run
/// count) plus the per-agent breakdown (the same totals grouped by agent,
/// heaviest cost first). Workspace-scoped: a foreign / unknown workspace yields
/// all-zero totals + an empty per-agent list.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if either aggregate query fails.
pub async fn usage_rollup(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<ainb_hangar_proto::snapshots::UsageRollupResult, sqlx::Error> {
    let totals = UsageRepo::workspace_totals(pool, workspace_id).await?;
    let agents = UsageRepo::rollup_by_agent(pool, workspace_id).await?;
    Ok(ainb_hangar_proto::snapshots::UsageRollupResult {
        total_input_tokens: totals.input_tokens,
        total_output_tokens: totals.output_tokens,
        total_cost_usd: totals.cost_usd,
        total_runs: totals.runs,
        agents: agents
            .into_iter()
            .map(|a| ainb_hangar_proto::snapshots::AgentUsageRow {
                agent_id: a.agent_id,
                input_tokens: a.input_tokens,
                output_tokens: a.output_tokens,
                cost_usd: a.cost_usd,
                runs: a.runs,
            })
            .collect(),
    })
}

/// Snapshot a workspace's per-run observability timeline (`hangar/run_history`,
/// P10 / D19).
///
/// Reads the durable `run_history` rows the daemon's run loop appends at each
/// run's finalize seam: the newest `limit` finished runs, each carrying provider
/// / session / profile / outcome / duration and token-cost. Workspace-scoped: a
/// foreign / unknown workspace yields an empty timeline.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the timeline query fails.
pub async fn run_history(
    pool: &SqlitePool,
    workspace_id: &str,
    limit: i64,
) -> Result<ainb_hangar_proto::snapshots::RunHistoryResult, sqlx::Error> {
    let rows = RunHistoryRepo::list_by_workspace(pool, workspace_id, limit).await?;
    Ok(ainb_hangar_proto::snapshots::RunHistoryResult {
        runs: rows
            .into_iter()
            .map(|r| ainb_hangar_proto::snapshots::RunHistoryRow {
                run_id: r.run_id,
                task_id: r.task_id,
                session_id: r.session_id,
                provider: r.provider,
                profile: r.profile,
                started_at: r.started_at,
                finished_at: r.finished_at,
                outcome: r.outcome,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                cost_usd: r.cost_usd,
                diff_add: r.diff_add,
                diff_del: r.diff_del,
            })
            .collect(),
    })
}

/// Mark every currently-unread inbox entry ADDRESSED TO `recipient` in
/// `workspace` as read, returning `(marked, unread_after)`
/// (`hangar/inbox_mark_read`, e38.14; per-recipient since store migration 0060).
///
/// `marked` is how many of that recipient's rows the sweep flipped (their unread
/// count before); `unread_after` is THEIR unread count once the sweep commits,
/// which is `0`. A sibling actor's unread rows are neither swept nor counted.
/// Idempotent — a re-sweep flips nothing. The daemon resolves + rejects a
/// mistyped workspace before this call, so a missing workspace never reaches
/// here.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the sweep or the follow-up count fails.
pub async fn inbox_mark_read(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace_id: &str,
    recipient: &ActorRef,
) -> Result<(i64, i64), sqlx::Error> {
    let marked = InboxRepo::mark_all_read(pool, workspace_id, recipient, clock.now_ms()).await?;
    let unread = InboxRepo::unread_count(pool, workspace_id, recipient).await?;
    Ok((i64::try_from(marked).unwrap_or(i64::MAX), unread))
}

/// Move one task to `to_status`, scoped to `workspace` (`hangar/task_transition`,
/// P8.4). Backs the Kanban card-move; the daemon parses + validates the wire
/// status token before this call.
///
/// Returns `true` when exactly one row moved, `false` when the task id resolved
/// to no task in this tenant (a foreign id moves nothing).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault (e.g. the DB `CHECK` constraint).
pub async fn task_transition(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace_id: &str,
    task_id: &str,
    to_status: TaskStatus,
) -> Result<bool, sqlx::Error> {
    TaskRepo::transition_status(pool, workspace_id, task_id, to_status, clock.now_ms()).await
}

/// Edit one issue's mutable fields, scoped to `workspace_id`, then re-read the
/// row as a wire [`IssueRow`] (`hangar/issue_update`, e38.8).
///
/// `update` is the already-validated partial edit (the daemon maps the wire
/// params — including the assignee actor-ref parse — before this call). The
/// write is workspace-scoped at the SQL boundary, so a foreign-tenant issue id
/// touches no row. Returns `Some(row)` with the refreshed issue when exactly one
/// row was edited, `None` when the `(id, workspace)` pair matched nothing (the
/// not-found / cross-tenant case the caller surfaces as an error).
///
/// The re-read reuses the same `IssueRow` shape `issues_list` emits (including
/// the P9 `pr_url` derivation) so the response row and the pushed
/// `IssueUpdated` event are byte-identical to a list snapshot of the row.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault, or a malformed stored id on the
/// re-read.
pub async fn issue_update(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    update: &ainb_hangar_store::repo::issue::IssueFieldUpdate,
) -> Result<Option<IssueRow>, sqlx::Error> {
    let touched = IssueRepo::update_fields(pool, workspace_id, issue_id, update).await?;
    if !touched {
        return Ok(None);
    }
    // Re-read the edited row and map it exactly as issues_list does, so the
    // response + event row match a list snapshot byte-for-byte.
    issue_row(pool, workspace_id, issue_id).await
}

/// Read one issue as a wire [`IssueRow`], scoped to `workspace_id`, mapped exactly
/// as `issues_list` maps it (so a response + pushed event row match a list
/// snapshot byte-for-byte). Returns `None` when the `(id, workspace_id)` pair
/// resolves to no issue (an unknown id or a foreign tenant).
///
/// Shared by [`issue_update`] (its post-write re-read) and the F6 card-edit
/// handler's repo/agent-only path, which changes no `issue` column the field
/// UPDATE covers yet still must resolve the row to answer + announce.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault or a malformed stored id.
pub async fn issue_row(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Option<IssueRow>, sqlx::Error> {
    let Some(issue) = IssueRepo::get_by_id(pool, issue_id).await? else {
        return Ok(None);
    };
    // Scope to the workspace so a foreign-tenant id never leaks a row.
    if issue.workspace_id != workspace_id {
        return Ok(None);
    }
    let id = IssueId::from_str(&issue.id).map_err(|e| sqlx::Error::ColumnDecode {
        index: "id".to_string(),
        source: format!("malformed issue id {:?}: {e}", issue.id).into(),
    })?;
    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let display_id = issue_display_row(pool, workspace_id, &issue.id, prefix.as_deref()).await?;
    let pr_url = latest_pr_url_for_issue(pool, workspace_id, &issue.id).await?;
    let branch = latest_branch_for_issue(pool, workspace_id, &issue.id).await?;
    let extras = issue_card_fields(pool, &issue.id).await?;
    // multica parity #20: the DETAIL path (and only it) carries the typed link
    // graph — a list snapshot leaves `dependencies` empty on purpose, because
    // filling it there would be an N-query fan-out per row.
    let dependencies = issue_link_rows(pool, workspace_id, &issue.id).await?;
    Ok(Some(IssueRow {
        // multica parity #12: WHY this card is not running, from the newest
        // dispatch_attempt when that attempt was a decline. All `None` on a
        // healthy card, so the row grows by zero keys.
        last_dispatch_reason: extras.last_dispatch_reason,
        last_dispatch_detail: extras.last_dispatch_detail,
        last_dispatch_at: extras.last_dispatch_at,
        // ORIGIN PROVENANCE (0056): echoed from the stored pair so the wire
        // row a snapshot carries and the row an event pushes agree.
        origin_type: issue.origin.as_ref().map(|o| o.kind_db_str().to_string()),
        origin_id: issue.origin.as_ref().and_then(|o| o.id().map(ToString::to_string)),
        id,
        display_id,
        workspace_id: issue.workspace_id,
        title: issue.title,
        description: issue.description,
        state: issue.state,
        assignee: issue.assignee.map(|a| format!("{}:{}", a.kind().as_str(), a.id())),
        creator: format!("{}:{}", issue.creator.kind().as_str(), issue.creator.id()),
        created_at: issue.created_at,
        priority: issue.priority,
        due_date: issue.due_date,
        labels: issue.labels,
        pr_url,
        branch,
        repo_ref: extras.repo_ref,
        agent: extras.agent,
        source_branch: extras.source_branch,
        target_branch: extras.target_branch,
        external_ref: issue.external_ref,
        run_count: extras.run_count,
        last_run_status: extras.last_run_status,
        last_run_at: extras.last_run_at,
        parent_id: extras.parent_id,
        child_total: extras.child_total,
        child_done: extras.child_done,
        acceptance_criteria: criteria_texts(&issue.acceptance_criteria),
        acceptance: issue.acceptance_criteria,
        context_refs: issue.context_refs,
        dependencies,
    }))
}

/// One issue's TYPED links (multica parity #20), in render order: `blocked_by`
/// first (each flagged `satisfied` once that blocker has finished, so the detail
/// card can show ✓ vs 🔒), then the reverse `blocks` direction, then the
/// non-gating `related` set.
///
/// Every row is stated from the SUBJECT issue's point of view and carries the
/// OTHER issue's display id / title / state, resolved through the same
/// [`issue_display_row`] helper the list snapshot uses, so a link renders
/// identically to the issue it points at.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
pub async fn issue_link_rows(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Vec<ainb_hangar_proto::events::IssueLinkRow>, sqlx::Error> {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let blockers = CardDependencyRepo::blockers_of(pool, issue_id).await?;
    let unfinished = CardDependencyRepo::unfinished_blockers_of(pool, issue_id).await?;
    let blocks = CardDependencyRepo::blocks_of(pool, issue_id).await?;
    let related = CardDependencyRepo::related_of(pool, issue_id).await?;

    let mut out = Vec::with_capacity(blockers.len() + blocks.len() + related.len());
    for (kind, ids) in [
        ("blocked_by", &blockers),
        ("blocks", &blocks),
        ("related", &related),
    ] {
        for other in ids {
            let display_id =
                issue_display_row(pool, workspace_id, other, prefix.as_deref()).await?;
            let row = IssueRepo::get_by_id(pool, other).await?;
            out.push(ainb_hangar_proto::events::IssueLinkRow {
                kind: kind.to_string(),
                issue_id: other.clone(),
                display_id,
                title: row.as_ref().map(|r| r.title.clone()).unwrap_or_default(),
                state: row.as_ref().map(|r| r.state.clone()).unwrap_or_default(),
                // Only a blocker can be satisfied; `blocks` / `related` never gate.
                satisfied: kind == "blocked_by" && !unfinished.contains(other),
            });
        }
    }
    Ok(out)
}

/// Attach a label to an issue, scoped to `workspace_id`, then re-read the issue
/// as a wire [`IssueRow`] (`hangar/issue_label_attach`, e38.10).
///
/// Delegates to the secured [`LabelRepo::attach`], which verifies the issue
/// belongs to `workspace_id` before touching the join (the tenant guard),
/// resolve-or-creates the label by `(workspace, name)`, and keeps the
/// `issue.labels` JSON cache in sync — so the re-read row carries the new label
/// in its `labels` chip list. A foreign-tenant issue id surfaces as
/// [`LabelRepoError::IssueNotFound`], which the caller maps to a not-found error.
///
/// Returns `Some(row)` with the refreshed issue (mirroring the `issues_list`
/// shape so the response + pushed `IssueUpdated` event are byte-identical to a
/// list snapshot), or `None` if the row vanished between mutation and re-read
/// (a should-not-happen race the caller treats as not-found).
///
/// # Errors
///
/// Returns [`LabelRepoError::IssueNotFound`] when the issue is foreign, or
/// [`LabelRepoError::Db`] on a store fault.
pub async fn issue_label_attach(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    issue_id: &str,
    name: &str,
    color: Option<&str>,
) -> Result<Option<IssueRow>, LabelRepoError> {
    LabelRepo::attach(pool, workspace, issue_id, name, color).await?;
    Ok(read_issue_row(pool, workspace.as_str(), issue_id).await?)
}

/// Detach a label from an issue, scoped to `workspace_id`, then re-read the issue
/// as a wire [`IssueRow`] (`hangar/issue_label_detach`, e38.10).
///
/// Idempotent + workspace-scoped, mirroring [`issue_label_attach`]: delegates to
/// [`LabelRepo::detach`], which keeps the `issue.labels` JSON cache in sync so the
/// re-read row drops the chip. Detaching an absent label is a no-op (the row
/// re-reads unchanged), not an error.
///
/// # Errors
///
/// Returns [`LabelRepoError::IssueNotFound`] when the issue is foreign, or
/// [`LabelRepoError::Db`] on a store fault.
pub async fn issue_label_detach(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    issue_id: &str,
    name: &str,
) -> Result<Option<IssueRow>, LabelRepoError> {
    LabelRepo::detach(pool, workspace, issue_id, name).await?;
    Ok(read_issue_row(pool, workspace.as_str(), issue_id).await?)
}

/// Tick / untick one acceptance criterion on an issue, scoped to `workspace`,
/// then re-read the issue as a wire [`IssueRow`]
/// (`hangar/issue_criterion_set`, multica parity #11-rest).
///
/// Delegates the whole read-modify-write to [`IssueRepo::set_criterion_checked`]
/// so the CLI and the daemon share exactly one mutator. `criterion` is either
/// the criterion id or a 1-based ordinal.
///
/// # Errors
///
/// Propagates [`CriterionError`] — a foreign issue, an unknown criterion, a lost
/// update, or a store fault.
pub async fn issue_criterion_set(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    workspace: &WorkspaceId,
    issue_id: &str,
    criterion: &str,
    checked: bool,
    at: i64,
    actor: Option<&str>,
) -> Result<Option<IssueRow>, CriterionError> {
    IssueRepo::set_criterion_checked(
        pool,
        idgen,
        workspace.as_str(),
        issue_id,
        criterion,
        checked,
        at,
        actor,
    )
    .await?;
    Ok(read_issue_row(pool, workspace.as_str(), issue_id).await?)
}

/// Re-read one issue as a wire [`IssueRow`], mapped exactly as `issues_list`
/// emits it (including the P9 `pr_url` derivation) so a re-read row is
/// byte-identical to a list snapshot of the same issue. `None` when the id
/// resolves to no row.
///
/// Shared by the label attach/detach paths after they mutate the join — both
/// answer with the refreshed row.
async fn read_issue_row(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
) -> Result<Option<IssueRow>, sqlx::Error> {
    let Some(issue) = IssueRepo::get_by_id(pool, issue_id).await? else {
        return Ok(None);
    };
    let id = IssueId::from_str(&issue.id).map_err(|e| sqlx::Error::ColumnDecode {
        index: "id".to_string(),
        source: format!("malformed issue id {:?}: {e}", issue.id).into(),
    })?;
    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let display_id = issue_display_row(pool, workspace_id, &issue.id, prefix.as_deref()).await?;
    let pr_url = latest_pr_url_for_issue(pool, workspace_id, &issue.id).await?;
    let branch = latest_branch_for_issue(pool, workspace_id, &issue.id).await?;
    let extras = issue_card_fields(pool, &issue.id).await?;
    // multica parity #20: the DETAIL path (and only it) carries the typed link
    // graph — a list snapshot leaves `dependencies` empty on purpose, because
    // filling it there would be an N-query fan-out per row.
    let dependencies = issue_link_rows(pool, workspace_id, &issue.id).await?;
    Ok(Some(IssueRow {
        // multica parity #12: WHY this card is not running, from the newest
        // dispatch_attempt when that attempt was a decline. All `None` on a
        // healthy card, so the row grows by zero keys.
        last_dispatch_reason: extras.last_dispatch_reason,
        last_dispatch_detail: extras.last_dispatch_detail,
        last_dispatch_at: extras.last_dispatch_at,
        // ORIGIN PROVENANCE (0056): echoed from the stored pair so the wire
        // row a snapshot carries and the row an event pushes agree.
        origin_type: issue.origin.as_ref().map(|o| o.kind_db_str().to_string()),
        origin_id: issue.origin.as_ref().and_then(|o| o.id().map(ToString::to_string)),
        id,
        display_id,
        workspace_id: issue.workspace_id,
        title: issue.title,
        description: issue.description,
        state: issue.state,
        assignee: issue.assignee.map(|a| format!("{}:{}", a.kind().as_str(), a.id())),
        creator: format!("{}:{}", issue.creator.kind().as_str(), issue.creator.id()),
        created_at: issue.created_at,
        priority: issue.priority,
        due_date: issue.due_date,
        labels: issue.labels,
        pr_url,
        branch,
        repo_ref: extras.repo_ref,
        agent: extras.agent,
        source_branch: extras.source_branch,
        target_branch: extras.target_branch,
        external_ref: issue.external_ref,
        run_count: extras.run_count,
        last_run_status: extras.last_run_status,
        last_run_at: extras.last_run_at,
        parent_id: extras.parent_id,
        child_total: extras.child_total,
        child_done: extras.child_done,
        acceptance_criteria: criteria_texts(&issue.acceptance_criteria),
        acceptance: issue.acceptance_criteria,
        context_refs: issue.context_refs,
        dependencies,
    }))
}

/// The lifecycle state a merged PR auto-transitions its backing issue to
/// (e38.34). The same `"done"` token the Beads inbound reconcile lands.
const PR_MERGED_DONE_STATE: &str = "done";

/// Refresh the CI + merge status of `issue_id`'s bound PR, auto-moving the issue
/// to Done when the PR is merged (`hangar/pr_status_refresh`, e38.34).
///
/// Resolves the issue's latest task `result.pr_url` (the P9.1 capture), fetches
/// its [`PrStatus`] through the injectable `provider` (the real
/// [`crate::pr_status::GhPrStatusProvider`] in production, a fake in tests — never
/// real `gh` under test), and — **only** when the PR is merged AND the issue is
/// not already in the `done` state — moves the issue to `done` via
/// [`IssueRepo::update_state`] (the same primitive the Beads inbound reconcile
/// uses). The done-stamp now keys on the PR actually merging, not on a `bd`-side
/// close sync.
///
/// Returns the fetched status plus `Some(row)` with the re-read issue **iff** this
/// call performed the transition (so the caller can push the `IssueUpdated` event
/// and the plugin can reflect the column move); `None` for the second element when
/// no transition happened (an open / closed / un-merged PR, or no bound PR — in
/// which case the status is the all-`Unknown` degrade value).
///
/// An issue with no bound PR resolves an all-`Unknown` status + no transition (a
/// read, never an error). A `gh` fetch failure already degrades inside the
/// provider, so this path never errors on the fetch itself.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault (the pr-url resolve, the state
/// update, or the issue re-read).
pub async fn refresh_pr_status(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    provider: &dyn crate::pr_status::PrStatusProvider,
) -> Result<(ainb_hangar_proto::pr_status::PrStatus, Option<IssueRow>), sqlx::Error> {
    // No bound PR → all-unknown status, no transition (a read, never `gh`).
    let Some(pr_url) = latest_pr_url_for_issue(pool, workspace_id, issue_id).await? else {
        return Ok((ainb_hangar_proto::pr_status::PrStatus::default(), None));
    };
    let status = provider.fetch(&pr_url).await;
    if !status.is_merged() {
        return Ok((status, None));
    }
    // The PR is merged. Read the current issue to skip a no-op re-stamp (and to
    // avoid emitting an `IssueUpdated` for an already-done issue — idempotent).
    let Some(issue) = IssueRepo::get_by_id(pool, issue_id).await? else {
        return Ok((status, None));
    };
    if issue.state == PR_MERGED_DONE_STATE {
        return Ok((status, None));
    }
    let prev_state = issue.state.clone();
    IssueRepo::update_state(pool, issue_id, PR_MERGED_DONE_STATE).await?;
    // multica parity #13: a daemon-driven transition is a `system` activity row
    // carrying `via` so the narrative distinguishes "a human moved this" from
    // "the merged PR moved this". Best-effort — never fails the transition.
    ActivityService::record(
        pool,
        &SystemIdGen,
        &SystemClock,
        workspace_id,
        issue_id,
        &ActivityActor::System,
        ActivityAction::StatusChanged,
        serde_json::json!({
            "from": prev_state,
            "to": PR_MERGED_DONE_STATE,
            "via": "pr_merged",
        }),
    )
    .await;
    // Re-read the now-Done row so the caller can push `IssueUpdated`.
    let row = read_issue_row(pool, workspace_id, issue_id).await?;
    Ok((status, row))
}

/// Everything the caller supplies for one `hangar/issue_create` — the daemon
/// mints the id, stamps `created_at`, and picks the `open` state itself.
///
/// Exists so [`issue_create`] stays under the argument-count lint as the create
/// surface grows: every new authored attribute lands here instead of on the
/// signature. Borrowed throughout — the struct is built and consumed within one
/// handler call.
#[derive(Debug, Clone, Copy)]
pub struct IssueCreateInput<'a> {
    /// The tenant-isolation guard: the resolved workspace the issue belongs to.
    pub workspace_id: &'a str,
    /// The issue title (already validated non-blank at the handler boundary).
    pub title: &'a str,
    /// Optional free-form body text.
    pub description: Option<&'a str>,
    /// The creating actor (already parsed from its `kind:id` wire form).
    pub creator: &'a ActorRef,
    /// Optional upstream-issue link (migration 0043).
    pub external_ref: Option<&'a str>,
    /// Optional parent issue, making this a sub-issue (migration 0046).
    pub parent_issue_id: Option<&'a str>,
    /// Ordered acceptance criteria (migration 0048).
    pub acceptance_criteria: &'a [String],
    /// Ordered context references (migration 0048).
    pub context_refs: &'a [String],
    /// Urgency `0..3` (P3..P0, HIGHER = MORE URGENT, migration 0014); `0` is the
    /// schema default. Range-validated at the handler boundary.
    pub priority: i64,
    /// Optional deadline as epoch ms at UTC midnight (migration 0014).
    pub due_date: Option<i64>,
    /// Label NAMES to attach (migration 0016): each resolve-or-created in the
    /// workspace and joined to the new issue.
    pub labels: &'a [String],
    /// ORIGIN PROVENANCE for the created issue (migration 0056, multica parity
    /// #21), already validated against the closed allow-list at the handler
    /// boundary. `None` => the create stamps `manual` - a human authored it.
    pub origin: Option<&'a IssueOrigin>,
}

/// Create one new issue in `workspace_id`, then return it as a wire [`IssueRow`]
/// (`hangar/issue_create`, e38.29).
///
/// Mints a fresh ULID via `idgen`, stamps `created_at` from `clock`, and inserts
/// through [`IssueRepo::insert`] in the `open` lifecycle state. The new issue is
/// unassigned (the create flow captures attributes, never an assignee);
/// `creator` is the already-parsed actor-ref. Authored labels are attached
/// through the 0016 join and read back from it, so the re-wrapped [`IssueRow`]
/// mirrors the `issues_list` shape (a freshly-created issue has no completed
/// task, so `pr_url` is always `None`) — the response row and the pushed
/// `IssueCreated` event stay byte-identical to a list snapshot of the row.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault (e.g. a `workspace_id` FK
/// violation), or a malformed minted id on the re-wrap (impossible for a ULID).
pub async fn issue_create(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    input: &IssueCreateInput<'_>,
) -> Result<IssueRow, sqlx::Error> {
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    use ainb_hangar_store::repo::issue::NewIssue;

    let &IssueCreateInput {
        workspace_id,
        title,
        description,
        creator,
        external_ref,
        parent_issue_id,
        acceptance_criteria,
        context_refs,
        priority,
        due_date,
        labels,
        origin,
    } = input;
    let id = idgen.new_ulid();
    let created_at = clock.now_ms();
    // #11-rest: the create wire stays TEXT-only; the daemon mints the stable
    // per-criterion ids server-side so every criterion is addressable from the
    // moment the issue exists. Blank elements are dropped by the constructor.
    let minted_criteria: Vec<AcceptanceCriterion> = acceptance_criteria
        .iter()
        .filter_map(|text| AcceptanceCriterion::new(idgen, text))
        .collect();
    // e38.21: apply the workspace's issue_prefix to the new title so the prefix
    // actually takes effect on a created issue (the stored title, the response
    // row, and the pushed IssueCreated event all carry it). An unconfigured
    // workspace leaves the title verbatim (the v1 behaviour).
    let prefix = workspace_issue_prefix(pool, workspace_id).await?;
    let title = apply_issue_prefix(prefix.as_deref(), title);
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: id.clone(),
            workspace_id: workspace_id.to_string(),
            title: title.clone(),
            description: description.map(ToString::to_string),
            state: "open".to_string(),
            assignee: None,
            creator: creator.clone(),
            created_at,
            priority,
            due_date,
            // 0016: labels are written through the `label` / `issue_label` join
            // below, never straight into the JSON cache — the join is the source
            // of truth and `LabelRepo::attach` re-derives the cache from it.
            labels: Vec::new(),
            acceptance_criteria: minted_criteria.clone(),
            context_refs: context_refs.to_vec(),
            parent_issue_id: parent_issue_id.map(ToString::to_string),
            stage: None,
        },
    )
    .await?;
    // 0043: persist the optional upstream-issue link AFTER the insert (the same
    // post-insert card-parity pattern as source/target branches). A `None` /
    // blank ref is a no-op, so a link-less create leaves `external_ref` NULL.
    CardParityRepo::set_issue_external_ref(pool, workspace_id, &id, external_ref).await?;
    // 0056: stamp the ORIGIN PROVENANCE with the same post-insert pattern. An
    // absent origin is stamped `manual` (never left NULL) so from here on
    // `origin_type IS NULL` means exactly one thing: "created before provenance
    // existed". multica leaves human creates NULL; recording them explicitly
    // makes the column a complete record without needing a backfill.
    let stamped_origin = origin.cloned().unwrap_or_else(IssueOrigin::manual);
    IssueRepo::set_origin(pool, workspace_id, &id, &stamped_origin).await?;
    // 0016: attach the authored labels through the join (resolve-or-create per
    // name, `ON CONFLICT DO NOTHING` on the join row, `issue.labels` re-derived
    // inside the same transaction). Attach is idempotent, so a repeated name is
    // one join row. The re-read below is what the response + pushed event carry,
    // so they match a later `issues_list` snapshot byte-for-byte.
    let stored_labels = if labels.is_empty() {
        Vec::new()
    } else {
        let ws = WorkspaceId::from_str(workspace_id.to_string()).map_err(|e| {
            sqlx::Error::ColumnDecode {
                index: "workspace_id".to_string(),
                source: format!("malformed workspace id {workspace_id:?}: {e}").into(),
            }
        })?;
        for name in labels {
            LabelRepo::attach(pool, &ws, &id, name, None).await.map_err(|e| match e {
                LabelRepoError::Db(db) => db,
                // Unreachable: the issue was just inserted into THIS workspace.
                LabelRepoError::IssueNotFound => sqlx::Error::RowNotFound,
            })?;
        }
        LabelRepo::labels_for_issue(pool, &ws, &id).await?
    };
    let issue_id = IssueId::from_str(id.clone()).map_err(|e| sqlx::Error::ColumnDecode {
        index: "id".to_string(),
        source: format!("malformed issue id {id:?}: {e}").into(),
    })?;
    // 63l.3: the just-inserted issue is the workspace's newest, so its display
    // ordinal is the post-insert count; resolve it the same way the list does so
    // the response + pushed IssueCreated event carry the HGR-<n> a later snapshot
    // shows.
    let display_id = issue_display_row(pool, workspace_id, &id, prefix.as_deref()).await?;
    Ok(IssueRow {
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        // ORIGIN PROVENANCE (0056): echo exactly what was just stamped, so the
        // response row and the pushed IssueCreated event stay byte-identical to
        // a later list snapshot of the same issue.
        origin_type: Some(stamped_origin.kind_db_str().to_string()),
        origin_id: stamped_origin.id().map(ToString::to_string),
        id: issue_id,
        display_id,
        workspace_id: workspace_id.to_string(),
        title,
        description: description.map(ToString::to_string),
        state: "open".to_string(),
        assignee: None,
        creator: format!("{}:{}", creator.kind().as_str(), creator.id()),
        created_at,
        // The urgency + deadline the create captured (0014), echoed so the
        // response row and the pushed IssueCreated event match a list snapshot.
        priority,
        due_date,
        // Read back from the 0016 join (ORDER BY name), exactly what a later
        // `issues_list` shows — never the caller's unsorted input.
        labels: stored_labels,
        pr_url: None,
        // A freshly-created issue has no tasks yet, so no committed branch, and no
        // repo / agent / branches pinned until the follow-up issue_update (63d).
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        // The upstream link the create captured (trimmed to `None` when blank),
        // so the response + pushed IssueCreated event carry it.
        external_ref: external_ref.map(str::to_string),
        // A freshly-created issue has never run.
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        // 0046: the parent link the create captured (a sub-issue), or `None` for a
        // top-level issue. A fresh issue has no children yet, so the roll-up is 0/0.
        parent_id: parent_issue_id.map(str::to_string),
        child_total: 0,
        child_done: 0,
        // The lists the create captured (blank elements already dropped at the
        // handler boundary), echoed on the response + pushed IssueCreated event.
        acceptance_criteria: criteria_texts(&minted_criteria),
        acceptance: minted_criteria,
        context_refs: context_refs.to_vec(),
        dependencies: Vec::new(),
    })
}

/// Read a workspace's configured `issue_prefix` by id (e38.21).
///
/// `None` when the workspace has no prefix configured (the migration-0020 NULL
/// default) or the workspace does not exist — the create flow then leaves the
/// title verbatim. Read directly (not via `WorkspaceRepo::get_config`) so the
/// create path pays for one column, not the full config decode.
async fn workspace_issue_prefix(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let prefix: Option<String> =
        sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(prefix)
}

/// Append one comment to an issue, scoped to `workspace_id`, then return it as a
/// wire [`CommentRow`] (`hangar/comment_add`, e38.5).
///
/// Mints a fresh ULID via `idgen`, stamps `created_at` from `clock`, and inserts
/// through [`CommentRepo`], which scopes the write by `(issue_id, workspace_id)`
/// at the SQL boundary (a join to `issue`). Returns `Some(row)` with the
/// persisted comment when the insert landed, `None` when the `(issue, workspace)`
/// pair matched no issue — the not-found / cross-tenant case the caller surfaces
/// as an error. The `author` is the already-parsed actor-ref.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault, or a malformed minted id on the
/// re-wrap (impossible for a ULID).
pub async fn comment_add(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    workspace_id: &str,
    issue_id: &str,
    author: &ActorRef,
    body: &str,
) -> Result<Option<CommentRow>, sqlx::Error> {
    let id = idgen.new_ulid();
    let created_at = clock.now_ms();
    let landed = CommentRepo::insert(
        pool,
        workspace_id,
        &NewComment {
            id: id.clone(),
            issue_id: issue_id.to_string(),
            author: author.clone(),
            body: body.to_string(),
            created_at,
        },
    )
    .await?;
    if !landed {
        return Ok(None);
    }
    let comment_id = CommentId::from_str(id.clone()).map_err(|e| sqlx::Error::ColumnDecode {
        index: "id".to_string(),
        source: format!("malformed comment id {id:?}: {e}").into(),
    })?;
    let issue = IssueId::from_str(issue_id).map_err(|e| sqlx::Error::ColumnDecode {
        index: "issue_id".to_string(),
        source: format!("malformed issue id {issue_id:?}: {e}").into(),
    })?;
    Ok(Some(CommentRow {
        id: comment_id,
        issue_id: issue,
        author: format!("{}:{}", author.kind().as_str(), author.id()),
        body: body.to_string(),
        created_at,
    }))
}

/// Resolve the `@handle` mentions in a just-committed comment to agents in
/// `workspace_id`, and enqueue an issue task for each that matches (e38.7).
///
/// This is the comment-triggered task-spawn path: the `comment_add` handler
/// calls it AFTER the comment commits, so a user `@`-mentioning an agent in a
/// comment spawns that agent's task on the comment's issue. The trigger fires
/// after the write so a spawn-side failure can never roll back (or lose) the
/// comment — matching the bead's "fires from inside `comment_add` after the
/// comment commits" contract and side-stepping the reference's mid-write expansion race.
///
/// Every resolved target is GATED (gap #8, multica
/// `resolveMentionedAgentCommentTriggers`): the comment's `author` is mapped to an
/// effective invoker ([`effective_mention_invoker`]) and an agent that invoker may
/// not invoke is skipped exactly like an unresolvable handle — no task row, and no
/// error. The gate runs BEFORE any other per-agent state is consulted, so a caller
/// who cannot invoke an agent learns nothing about it from the trigger; and it is a
/// per-target skip, so a denied `@handle` never suppresses the others in the same
/// comment.
///
/// Resolution is by agent **name**, **workspace-scoped**: the candidate set is
/// [`AgentRepo::list_by_workspace`] for the comment's workspace, so a foreign
/// tenant's agent sharing the handle never gets a task. An unknown handle resolves
/// to no agent and is silently ignored — never an error. Each matched agent is
/// enqueued through the ordinary [`TaskRepo::insert`] path (no claim/dispatch
/// logic is duplicated), bound to the comment's `issue_id` with the agent's
/// `runtime_id` (`NOT NULL` + FK-enforced, so a resolved agent always has one).
/// A duplicate enqueue while the agent still holds a *pending* (`queued` /
/// `dispatched`) task on this issue — the per-`(issue, agent)` unique index — is
/// coalesced, not an error, so re-mentioning is idempotent. Once that task has
/// advanced to `running` the index no longer guards it, so a fresh mention
/// enqueues a new task (intended: a follow-up mention after work started is a
/// new request).
///
/// Returns the agent ids that actually got a task enqueued (for the caller's
/// logging / future event push), empty when no mention resolved.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] only on an unexpected store fault — the expected
/// duplicate-pending-task case is coalesced inline by the unique index, not
/// surfaced, so a single repeated mention never poisons the whole comment.
pub async fn spawn_mention_tasks(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    workspace_id: &str,
    issue_id: &str,
    comment_id: &str,
    author: &ainb_hangar_core::actor::ActorRef,
    body: &str,
) -> Result<Vec<String>, sqlx::Error> {
    use crate::mentions::parse_mentions;
    use ainb_hangar_store::repo::agent::AgentRepo;
    use ainb_hangar_store::repo::task::NewTask;

    let handles = parse_mentions(body);
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    // gap #8 — the EFFECTIVE invoker the invocation gate judges each mention target
    // by (multica `resolveMentionedAgentCommentTriggers`, `comment.go:2323`/`:2365`).
    let (invoker_kind, invoker_id) = effective_mention_invoker(pool, workspace_id, author).await?;
    // The workspace's agents are the only resolution candidates: a foreign
    // tenant's agent sharing a handle is never in this list, so it cannot be
    // mention-triggered here.
    let agents = AgentRepo::list_by_workspace(pool, workspace_id).await?;
    let now = clock.now_ms();
    // One mention event is one run GENERATION (migration 0039, tcp 8ln): every agent
    // fanned out from this comment shares it, and it scopes the card-state folds to
    // this run so a prior run's terminal rows on the issue do not poison it. Minted
    // once, before the fan-out loop, exactly like the squad fan-out.
    let generation = TaskRepo::next_generation_for_issue(pool, issue_id).await?;
    // 0056 ORIGIN PROVENANCE: every task this comment spawns is stamped
    // `('comment_mention', <comment.id>)` — hangar's structural analogue of
    // multica's `quick_create` provenance. The dispatcher hands it to the agent
    // child, so an issue the agent creates mid-run is attributable back to the
    // comment that asked for it.
    let mention_origin = IssueOrigin::comment_mention(comment_id).map_err(|e| {
        sqlx::Error::Protocol(format!(
            "comment {comment_id} is unusable as an origin id: {e}"
        ))
    })?;
    let mut spawned = Vec::new();
    for handle in &handles {
        // Resolve by name; an unknown handle simply matches no agent (ignored).
        let Some(agent) = agents.iter().find(|a| &a.name == handle) else {
            continue;
        };
        // gap #8 — the invocation gate, FIRST: multica checks invocability before
        // any other per-agent state is read, so a caller who cannot invoke an agent
        // never learns its archived / runtime state from the trigger's behaviour
        // (`comment.go:2364`, enumeration-safety). A refusal is a per-target
        // `continue`, never an early return: one denied `@handle` must not suppress
        // the other mentions in the same comment. Denied ⇒ NO task row.
        if !AgentRepo::can_invoke(pool, agent, invoker_kind, invoker_id.as_deref()).await? {
            tracing::debug!(
                agent = %agent.id,
                handle = %handle,
                "mention dispatch refused: invocation not allowed"
            );
            continue;
        }
        let task = NewTask {
            id: idgen.new_ulid(),
            workspace_id: workspace_id.to_string(),
            runtime_id: agent.runtime_id.clone(),
            agent_id: agent.id.clone(),
            issue_id: Some(issue_id.to_string()),
            work_dir: None,
            // A mention is a direct, user-initiated ask: default urgency (P3),
            // drained FIFO among equals — the same default the autopilot path uses.
            priority: 0,
            created_at: now,
            autopilot_run_id: None,
            generation,
        };
        match TaskRepo::insert(pool, &task).await {
            Ok(_) => {
                // Stamped per spawned task, INSIDE the per-handle loop and AFTER
                // the invocation gate: a refused mention writes no row and
                // therefore no provenance.
                TaskRepo::set_origin(pool, &task.id, &mention_origin).await?;
                spawned.push(agent.id.clone());
            }
            // The agent already has a pending task on this issue (the per-(issue,
            // agent) unique index): coalesce, don't error. The trigger is
            // idempotent — re-mentioning an already-queued agent is a no-op.
            Err(e) if is_unique_violation(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(spawned)
}

/// The actor id the local TUI stamps on everything it authors (`member:me`).
///
/// It is a PLACEHOLDER, not a `user.id`: the plugin has no identity of its own and
/// the daemon socket is the local operator's. `handle_issue_create` mints the same
/// ref for wizard-created issues.
pub(crate) const LOCAL_OPERATOR_MEMBER_ID: &str = "me";

/// Resolve a comment author to the EFFECTIVE invoking identity the gap #8
/// invocation gate judges its `@`-mention targets by.
///
/// - a `member` author naming a REAL user id → that user (the multi-user case the
///   allow-list exists for);
/// - the local-operator placeholder [`LOCAL_OPERATOR_MEMBER_ID`] → the workspace
///   OWNER, exactly like `run_card`'s "no explicit invoker ⇒ owner" default. The
///   local TUI stamps `member:me` on every comment it composes, so without this the
///   gate would deny the single operator access to their own private agents — a
///   regression with no security gain, since that socket IS the operator's. An
///   owner-less workspace resolves to `""`, which matches no `owner_id` and fails
///   closed, the same shape `run_card` has;
/// - an `agent` author → `(Agent, None)`: hangar has no `originator_user_id`
///   column (multica 184/185 is a separate gap), so an agent-authored mention is
///   the UNATTRIBUTED A2A case — it fails closed for `private` and `member`-target
///   agents and admits only a `public_to workspace` target (`workspaceBroad`).
async fn effective_mention_invoker(
    pool: &SqlitePool,
    workspace_id: &str,
    author: &ainb_hangar_core::actor::ActorRef,
) -> Result<(ainb_hangar_core::actor::ActorKind, Option<String>), sqlx::Error> {
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_core::ids::WorkspaceId;

    if author.kind() != ActorKind::Member {
        return Ok((ActorKind::Agent, None));
    }
    if author.id() != LOCAL_OPERATOR_MEMBER_ID {
        return Ok((ActorKind::Member, Some(author.id().to_string())));
    }
    let owner = match WorkspaceId::from_str(workspace_id.to_string()) {
        Ok(ws) => ainb_hangar_store::repo::workspace::WorkspaceRepo::owner_id(pool, &ws)
            .await?
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    Ok((ActorKind::Member, Some(owner)))
}

/// Whether a `sqlx` error is a UNIQUE-constraint violation (the per-`(issue,
/// agent)` pending-task index coalescing a duplicate mention enqueue).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// Snapshot the registered runtimes of `workspace` for the daemon-health pane.
///
/// Maps each `agent_runtime` row to a wire [`RuntimeHealthRow`] for
/// `hangar/daemon_health` (P8.5).
///
/// Reads the `agent_runtime` table (workspace-scoped) and folds each row's raw
/// liveness `status` into a `connected` boolean (`"online"` → connected). `pid`
/// is the daemon process id hosting the pane (every runtime in a single-daemon
/// deployment shares it). A foreign workspace yields an empty set.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
pub async fn runtime_health(
    pool: &SqlitePool,
    workspace_id: &str,
    pid: u32,
) -> Result<Vec<ainb_hangar_proto::settings::RuntimeHealthRow>, sqlx::Error> {
    let runtimes = AgentRuntimeRepo::list_by_workspace(pool, workspace_id).await?;
    Ok(runtimes
        .into_iter()
        .map(|r| ainb_hangar_proto::settings::RuntimeHealthRow {
            provider: r.provider,
            connected: r.status == "online",
            pid,
        })
        .collect())
}

/// Count the tasks of `workspace` that are currently executing (`dispatched` or
/// `running`) for the daemon-health pane's concurrency figure
/// (`hangar/daemon_health`, P8.5).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
pub async fn concurrent_task_count(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<u32, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_queue \
         WHERE workspace_id = ?1 AND status IN ('dispatched','running')",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Error surface for [`autopilot_fire_now`]: a store fault resolving the
/// autopilot row, or a fire-path failure (the single-tx enqueue).
#[derive(Debug, thiserror::Error)]
pub enum AutopilotFireError {
    /// A store fault while resolving the autopilot to fire.
    #[error(transparent)]
    Repo(#[from] AutopilotRepoError),
    /// The fire/enqueue transaction failed (agent deleted, FK violation).
    #[error(transparent)]
    Fire(#[from] FireError),
}

#[cfg(test)]
mod issue_states_contract_tests {
    use super::ISSUE_STATES;
    use ainb_hangar_proto::lifecycle::IssueLifecycle;

    /// The snapshot's per-state query union covers every canonical lifecycle
    /// status — so the single source of truth (`IssueLifecycle`) can never gain a
    /// status the board silently fails to query (63l.3). The legacy `open` /
    /// `closed` tokens are an additive tolerance on top.
    #[test]
    fn issue_states_is_a_superset_of_every_canonical_status() {
        for status in IssueLifecycle::ALL {
            assert!(
                ISSUE_STATES.contains(&status.as_str()),
                "ISSUE_STATES must query the canonical status {:?} ({})",
                status,
                status.as_str()
            );
        }
        // The transition-period legacy tokens are queried too, so a not-yet-
        // remapped row is never missed.
        assert!(ISSUE_STATES.contains(&"open"), "legacy open queried");
        assert!(ISSUE_STATES.contains(&"closed"), "legacy closed queried");
    }
}

#[cfg(test)]
mod mention_spawn_tests {
    use super::spawn_mention_tasks;
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_store::Store;

    /// The seed fixture's OWNER member (`user-1` owns `agent-1`), the author every
    /// pre-gap-#8 case uses — it must keep spawning exactly as before.
    fn owner() -> ActorRef {
        ActorRef::new(ActorKind::Member, "user-1").unwrap()
    }

    /// How many tasks `agent-1` has queued/started on `issue-3` in the fixture
    /// workspace.
    async fn count_for_issue3(store: &Store) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_queue \
             WHERE agent_id = 'agent-1' AND issue_id = 'issue-3'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap()
    }

    /// A body mentioning the seeded `claude-agent` (id `agent-1`) enqueues one
    /// task on the issue and reports the spawned agent id.
    #[tokio::test]
    async fn spawns_one_task_per_resolved_mention() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &owner(),
            "@claude-agent please do X",
        )
        .await
        .unwrap();

        assert_eq!(spawned, vec!["agent-1".to_string()]);
        assert_eq!(count_for_issue3(&store).await, 1);
    }

    /// Re-mentioning an agent that already has a pending task on the issue
    /// coalesces (the per-(issue, agent) unique index) — the second call spawns
    /// nothing and is not an error, so the trigger is idempotent.
    #[tokio::test]
    async fn re_mention_coalesces_and_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let first = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &owner(),
            "@claude-agent do X",
        )
        .await
        .unwrap();
        assert_eq!(first, vec!["agent-1".to_string()]);

        // A second mention while the first task is still pending must coalesce.
        let second = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &owner(),
            "@claude-agent again",
        )
        .await
        .unwrap();
        assert!(
            second.is_empty(),
            "a duplicate pending task is coalesced, not re-spawned"
        );
        assert_eq!(
            count_for_issue3(&store).await,
            1,
            "still exactly one pending task on the issue"
        );
    }

    /// A handle that mentions the same agent twice in one body collapses to a
    /// single enqueue (the parser de-dupes, and a second insert would coalesce
    /// anyway).
    #[tokio::test]
    async fn duplicate_handle_in_one_body_spawns_one_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &owner(),
            "@claude-agent and also @claude-agent",
        )
        .await
        .unwrap();

        assert_eq!(spawned, vec!["agent-1".to_string()]);
        assert_eq!(count_for_issue3(&store).await, 1);
    }

    /// A body with no resolvable mention (unknown handle + plain text) enqueues
    /// nothing and reports an empty spawn set.
    #[tokio::test]
    async fn unknown_handle_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &owner(),
            "@nobody hello and @ghost too",
        )
        .await
        .unwrap();

        assert!(spawned.is_empty());
        assert_eq!(count_for_issue3(&store).await, 0);
    }

    /// Add `user_id` to the fixture workspace as a plain (non-owner) member.
    async fn seed_plain_member(store: &Store, user_id: &str) {
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 0)")
            .bind(user_id)
            .bind(format!("{user_id}@example.com"))
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'member')")
            .bind(crate::seed::WS_ID)
            .bind(user_id)
            .execute(store.pool())
            .await
            .unwrap();
    }

    /// Seed a second agent in the fixture workspace (on the fixture runtime),
    /// private by default and owned by the workspace owner.
    async fn seed_second_agent(store: &Store, id: &str, name: &str) {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, ?, 'runtime-1', 'workspace', 'user-1')",
        )
        .bind(id)
        .bind(crate::seed::WS_ID)
        .bind(name)
        .execute(store.pool())
        .await
        .unwrap();
    }

    /// Allow-list `user_id` on `agent_id` (`public_to` + a `member` target).
    async fn allow_member(store: &Store, agent_id: &str, user_id: &str) {
        use ainb_hangar_store::repo::agent::AgentRepo;
        use ainb_hangar_store::repo::agent_invocation_target::AgentInvocationTargetRepo;

        AgentRepo::set_permission_mode(store.pool(), agent_id, "public_to")
            .await
            .unwrap();
        AgentInvocationTargetRepo::add(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            agent_id,
            "member",
            user_id,
            None,
        )
        .await
        .unwrap();
    }

    /// gap #8 — MENTION GATE: a plain workspace member mentioning a PRIVATE agent
    /// they do not own spawns NOTHING, and no task row lands on the issue. The
    /// comment itself is unaffected (this function only owns the trigger).
    #[tokio::test]
    async fn mention_by_a_non_owner_member_of_a_private_agent_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        seed_plain_member(&store, "bob").await;

        let bob = ActorRef::new(ActorKind::Member, "bob").unwrap();
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &bob,
            "@claude-agent please do X",
        )
        .await
        .unwrap();

        assert!(
            spawned.is_empty(),
            "a non-owner member may not mention-dispatch a private agent"
        );
        assert_eq!(count_for_issue3(&store).await, 0, "no task row is written");

        // CONTROL: allow-list bob → the very same mention now spawns exactly one.
        allow_member(&store, "agent-1", "bob").await;
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &bob,
            "@claude-agent please do X",
        )
        .await
        .unwrap();
        assert_eq!(spawned, vec!["agent-1".to_string()]);
        assert_eq!(count_for_issue3(&store).await, 1);
    }

    /// gap #8 — the gate is PER-TARGET: one denied `@handle` in a comment must not
    /// suppress an allowed one in the same comment (multica's loop-local `continue`).
    #[tokio::test]
    async fn a_denied_mention_does_not_suppress_an_allowed_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        seed_plain_member(&store, "bob").await;
        seed_second_agent(&store, "agent-priv", "private-bot").await;
        // bob may invoke `claude-agent` but NOT `private-bot`.
        allow_member(&store, "agent-1", "bob").await;

        let bob = ActorRef::new(ActorKind::Member, "bob").unwrap();
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &bob,
            "@private-bot and @claude-agent please do X",
        )
        .await
        .unwrap();

        assert_eq!(
            spawned,
            vec!["agent-1".to_string()],
            "only the allowed target spawns; the denied one is skipped, not fatal"
        );
        let denied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_queue WHERE agent_id = 'agent-priv'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(denied, 0, "the denied target got no task row");
        assert_eq!(count_for_issue3(&store).await, 1);
    }

    /// gap #8 — an AGENT-authored mention carries no resolved human originator
    /// (hangar has no `originator_user_id`), so it is the unattributed A2A case:
    /// it fails closed against a private agent, and is admitted only once the
    /// target is `public_to` with a WORKSPACE target (multica's `workspaceBroad`).
    #[tokio::test]
    async fn an_agent_authored_mention_of_a_private_agent_is_refused() {
        use ainb_hangar_store::repo::agent::AgentRepo;
        use ainb_hangar_store::repo::agent_invocation_target::AgentInvocationTargetRepo;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();
        seed_second_agent(&store, "agent-2", "peer-bot").await;

        let peer = ActorRef::new(ActorKind::Agent, "agent-2").unwrap();
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &peer,
            "@claude-agent take this over",
        )
        .await
        .unwrap();
        assert!(
            spawned.is_empty(),
            "unattributed A2A must fail closed against a private agent"
        );
        assert_eq!(count_for_issue3(&store).await, 0);

        // Flip the target to `public_to` + a WORKSPACE target → workspaceBroad admits.
        AgentRepo::set_permission_mode(store.pool(), "agent-1", "public_to")
            .await
            .unwrap();
        AgentInvocationTargetRepo::add(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            "agent-1",
            "workspace",
            crate::seed::WS_ID,
            None,
        )
        .await
        .unwrap();
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &peer,
            "@claude-agent take this over",
        )
        .await
        .unwrap();
        assert_eq!(
            spawned,
            vec!["agent-1".to_string()],
            "a public_to WORKSPACE target admits unattributed automation"
        );
        assert_eq!(count_for_issue3(&store).await, 1);
    }

    /// gap #8 REGRESSION GUARD for the single-operator TUI: the plugin stamps the
    /// `member:me` placeholder (not a real `user.id`) on every comment it composes.
    /// That placeholder resolves to the WORKSPACE OWNER, so the operator keeps
    /// mention-dispatching their own private agents.
    #[tokio::test]
    async fn the_local_operator_placeholder_still_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        crate::seed::seed_p4_fixture(store.pool()).await.unwrap();

        let me = ActorRef::new(ActorKind::Member, super::LOCAL_OPERATOR_MEMBER_ID).unwrap();
        let spawned = spawn_mention_tasks(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            crate::seed::WS_ID,
            "issue-3",
            "c-1",
            &me,
            "@claude-agent please do X",
        )
        .await
        .unwrap();

        assert_eq!(spawned, vec!["agent-1".to_string()]);
        assert_eq!(count_for_issue3(&store).await, 1);
    }
}

#[cfg(test)]
mod pr_fetch_bound_tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use ainb_hangar_proto::pr_status::{CiRollup, PrStatus};

    use super::{PR_FETCH_CONCURRENCY, fetch_pr_statuses};
    use crate::pr_status::PrStatusProvider;

    /// A provider whose every fetch sleeps `delay` — the stand-in for a slow (or
    /// wedged, at the timeout) `gh` round-trip. Counts its calls so the test can
    /// prove each DISTINCT url is fetched exactly once.
    struct SlowProvider {
        delay: Duration,
        calls: Arc<AtomicUsize>,
    }

    impl PrStatusProvider for SlowProvider {
        fn fetch<'a>(
            &'a self,
            _pr_url: &'a str,
        ) -> Pin<Box<dyn std::future::Future<Output = PrStatus> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let delay = self.delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                PrStatus {
                    ci: CiRollup::Pass,
                    ..Default::default()
                }
            })
        }
    }

    /// A provider that tracks the PEAK number of fetches in flight at once. Each
    /// fetch bumps a live counter (recording a new max), sleeps, then releases it,
    /// so the test can read exactly how wide the fan-out got.
    struct PeakConcurrencyProvider {
        delay: Duration,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl PrStatusProvider for PeakConcurrencyProvider {
        fn fetch<'a>(
            &'a self,
            _pr_url: &'a str,
        ) -> Pin<Box<dyn std::future::Future<Output = PrStatus> + Send + 'a>> {
            // The increment runs when `fetch` is invoked — which the fan-out only
            // does AFTER acquiring a permit — so the live count tracks permits held.
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            let delay = self.delay;
            let in_flight = Arc::clone(&self.in_flight);
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                PrStatus {
                    ci: CiRollup::Pass,
                    ..Default::default()
                }
            })
        }
    }

    /// N distinct slow urls are fetched CONCURRENTLY: total wall-clock is ~one
    /// `delay`, NOT N × delay (the le3 bound). A serial loop over the same urls
    /// would take at least N × delay, so a comfortably-below-that ceiling can only
    /// pass when the fetches overlap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn distinct_urls_are_fetched_concurrently_not_serially() {
        const N: usize = 12;
        let delay = Duration::from_millis(300);
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(SlowProvider {
            delay,
            calls: Arc::clone(&calls),
        });

        let urls: std::collections::HashSet<String> =
            (0..N).map(|i| format!("https://github.com/o/r/pull/{i}")).collect();

        let started = Instant::now();
        let statuses = fetch_pr_statuses(provider, urls).await;
        let elapsed = started.elapsed();

        assert_eq!(statuses.len(), N, "every distinct url yields a status");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            N,
            "each distinct url fetched exactly once"
        );
        // Serial would be N * 300ms = 3.6s. Concurrent is ~300ms; a 1.5s ceiling
        // absorbs scheduler jitter while still failing a serial regression.
        assert!(
            elapsed < Duration::from_millis(1500),
            "N slow fetches ran concurrently (bounded to ~one delay), took {elapsed:?}"
        );
    }

    /// A board with far MORE distinct PR'd cards than the concurrency cap never
    /// runs more than [`PR_FETCH_CONCURRENCY`] fetches at once — the guard against
    /// a `gh` fork storm / GitHub secondary-rate-limit trip on a large workspace.
    /// The peak must also exceed 1, proving the fetches still overlap (not a
    /// serial regression). Every distinct url is still fetched exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fan_out_is_bounded_to_the_concurrency_cap() {
        const N: usize = PR_FETCH_CONCURRENCY * 3;
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(PeakConcurrencyProvider {
            delay: Duration::from_millis(80),
            in_flight,
            peak: Arc::clone(&peak),
        });

        let urls: std::collections::HashSet<String> =
            (0..N).map(|i| format!("https://github.com/o/r/pull/{i}")).collect();

        let statuses = fetch_pr_statuses(provider, urls).await;

        assert_eq!(statuses.len(), N, "every distinct url yields a status");
        let peak = peak.load(Ordering::SeqCst);
        assert!(
            peak <= PR_FETCH_CONCURRENCY,
            "in-flight fetches never exceed the cap ({peak} > {PR_FETCH_CONCURRENCY})"
        );
        assert!(
            peak > 1,
            "fetches still overlap (peak {peak} proves it is not serial)"
        );
    }

    /// The same url appearing on many cards collapses to ONE fetch (the caller
    /// dedups to a `HashSet` before this runs), so a board full of one PR's cards
    /// never fans out to many `gh` spawns.
    #[tokio::test]
    async fn empty_url_set_spawns_nothing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(SlowProvider {
            delay: Duration::from_millis(10),
            calls: Arc::clone(&calls),
        });
        let statuses = fetch_pr_statuses(provider, std::collections::HashSet::new()).await;
        assert!(statuses.is_empty());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no urls → no fetches spawned"
        );
    }
}

/// Merge one issue's activity rows and comments into the wire timeline,
/// **oldest first**, sorted by `(created_at, id)` — multica's `mergeTimeline`
/// (parity #13).
///
/// Comments are NOT duplicated as `activity_log` rows; they are merged here at
/// READ time so the comment body stays the single source of truth. Each side is
/// fetched independently and capped at `limit`; the merged list keeps the NEWEST
/// `limit` entries and renders them oldest-first.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault.
pub async fn issue_timeline(
    pool: &SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    limit: i64,
) -> Result<Vec<TimelineEntryRow>, sqlx::Error> {
    use ainb_hangar_proto::snapshots::{TIMELINE_KIND_ACTIVITY, TIMELINE_KIND_COMMENT};
    use ainb_hangar_store::repo::activity::ActivityRepo;
    use ainb_hangar_store::repo::comment::CommentRepo;

    let activities = ActivityRepo::list_for_issue(pool, issue_id, limit).await?;
    let comments = CommentRepo::list_by_issue(pool, workspace_id, issue_id).await?;

    let mut entries: Vec<TimelineEntryRow> = Vec::with_capacity(activities.len() + comments.len());
    for a in activities {
        // Re-assert the tenant: the per-issue query keys on the issue id, and the
        // activity row carries its own workspace column.
        if a.workspace_id != workspace_id {
            continue;
        }
        let details = a.details_json();
        entries.push(TimelineEntryRow {
            kind: TIMELINE_KIND_ACTIVITY.to_string(),
            id: a.id,
            actor_type: a.actor_type,
            actor_id: a.actor_id,
            created_at: a.created_at,
            action: Some(a.action),
            // An empty details object adds no information — omit it so the frame
            // stays small and an activity with nothing to say looks like one.
            details: (!details.as_object().is_some_and(serde_json::Map::is_empty))
                .then_some(details),
            body: None,
        });
    }
    for c in comments {
        entries.push(TimelineEntryRow {
            kind: TIMELINE_KIND_COMMENT.to_string(),
            id: c.id,
            actor_type: Some(c.author.kind().as_str().to_string()),
            actor_id: Some(c.author.id().to_string()),
            created_at: c.created_at,
            action: None,
            details: None,
            body: Some(c.body),
        });
    }

    // `(created_at, id)` ascending — the id is a deterministic tiebreak for two
    // entries recorded in the same millisecond.
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    // Keep the NEWEST window when the merged list overflows, then render it
    // oldest-first (multica's flat contract).
    let cap = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
    if entries.len() > cap {
        entries.drain(..entries.len() - cap);
    }
    Ok(entries)
}
