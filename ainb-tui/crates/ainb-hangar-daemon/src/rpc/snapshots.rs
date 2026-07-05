//! Store-row → proto-wire-row mappers for the P4 `hangar/*` snapshot RPCs.
//!
//! The daemon owns the data plane; the plugin owns zero domain data and only
//! renders the wire rows it pulls here. Each function reads the store repos and
//! flattens their rich Rust types (polymorphic [`ActorRef`], sqlx row models)
//! into the flat [`ainb_hangar_proto`] wire shapes the screens render from.
//!
//! ## Presence derivation
//!
//! An agent's [`PresenceState`] is derived from its backing
//! [`AgentRuntime::status`]: `"online"` → `Online`, `"unstable"` → `Unstable`,
//! anything else (`"offline"`, unseen) → `Offline`. A human member has no
//! runtime, so it is always reported `Online` (a member is "available" in the
//! picker; the real online/away signal for humans lands with presence tracking
//! in a later phase).

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::ids::{AgentId, AutopilotId, CommentId, IssueId, SkillId, WorkspaceId};
use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_proto::events::{
    ActorRow, AttentionRow, AutopilotRow, AutopilotRunRow, CommentRow, InboxEntryRow, IssueRow,
    PresenceState, SkillFile, SkillRow, TaskCardRow,
};
use ainb_hangar_proto::snapshots::{SkillDetail, SkillsSyncResult};
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::attention::AttentionRepo;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, AutopilotRepoError};
use ainb_hangar_store::repo::autopilot_run::{FireError, fire_autopilot_tick};
use ainb_hangar_store::repo::comment::{CommentRepo, NewComment};
use ainb_hangar_store::repo::inbox::InboxRepo;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::label::{LabelRepo, LabelRepoError};
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
use ainb_hangar_store::repo::task::TaskRepo;
use ainb_hangar_store::repo::run_history::RunHistoryRepo;
use ainb_hangar_store::repo::usage::UsageRepo;
use ainb_hangar_store::repo::workspace::{apply_issue_prefix, issue_display_id};
use sqlx::{Row, SqlitePool};

/// Every Hangar issue lifecycle state, queried per-state and concatenated so the
/// snapshot carries the whole board. `IssueRepo` lists by `(workspace, state)`,
/// so the snapshot unions the FIVE canonical states (the
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
            out.push(IssueRow {
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
            });
        }
    }
    Ok(out)
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
        out.push(IssueRow {
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

/// Snapshot the assignable actors of `workspace_id` — human members and agents
/// in one polymorphic [`ActorRow`] list.
///
/// Agents lead (they are the common assignee at v1), each carrying the presence
/// derived from its runtime; members follow. Recent-use ranking is a later-phase
/// concern, so every row is `recent_rank: None` (the picker falls back to its
/// alphabetical body, which is deterministic).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the agent / runtime / member queries fail.
pub async fn agents_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<ActorRow>, sqlx::Error> {
    let mut out = Vec::new();

    for agent in AgentRepo::list_by_workspace(pool, workspace_id).await? {
        out.push(agent_actor_row(pool, &agent).await?);
    }

    for member in members_of(pool, workspace_id).await? {
        out.push(ActorRow {
            actor_ref: format!("member:{}", member.user_id),
            display_name: member.email,
            subtitle: member.role,
            presence: PresenceState::Online,
            is_agent: false,
            recent_rank: None,
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
            members: s.members.iter().map(ToString::to_string).collect(),
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
    // The issue's most recent task's status — the card's live state — plus the
    // tmux session name an interactive run recorded on it (ccc / D6), so the
    // attach-from-card affordance can surface `tmux attach -t <session_name>`.
    let latest: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT status, session_name FROM agent_task_queue \
         WHERE issue_id = ? AND workspace_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(issue_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?;
    let (state, session_name) = match latest {
        Some((status, session)) => (Some(status), session),
        None => (None, None),
    };

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
    // The latest task per agent on this issue (the correlated subquery picks each
    // agent's most recent task), joined to the agent name.
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT t.agent_id, a.name, t.status \
         FROM agent_task_queue t \
         JOIN agent a ON a.id = t.agent_id \
         WHERE t.issue_id = ? AND t.workspace_id = ? \
           AND t.id = ( \
             SELECT t2.id FROM agent_task_queue t2 \
             WHERE t2.issue_id = t.issue_id AND t2.agent_id = t.agent_id \
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
        .map(|(agent_id, agent_name, status)| ainb_hangar_proto::snapshots::CardMemberChip {
            agent_id,
            agent_name,
            state: Some(status),
        })
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

/// Map an `agent_runtime.status` string onto a wire [`PresenceState`].
fn presence_from_status(status: &str) -> PresenceState {
    match status {
        "online" => PresenceState::Online,
        "unstable" => PresenceState::Unstable,
        _ => PresenceState::Offline,
    }
}

/// Map one store [`Agent`](ainb_hangar_store::repo::agent::Agent) onto its picker
/// [`ActorRow`], deriving presence from the backing runtime's status.
///
/// Shared by [`agents_list`] and the e38.15 agent-CRUD wrappers
/// ([`agent_update`] / [`agent_archive`]) so the row a mutation answers with is
/// byte-identical to the same agent's `agents_list` row. A missing runtime maps
/// to `Offline` (the runtime FK is required, but a deleted-out-of-band runtime
/// must not panic the snapshot).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the runtime lookup fails.
async fn agent_actor_row(
    pool: &SqlitePool,
    agent: &ainb_hangar_store::repo::agent::Agent,
) -> Result<ActorRow, sqlx::Error> {
    let presence = match AgentRuntimeRepo::get(pool, &agent.runtime_id).await? {
        Some(rt) => presence_from_status(&rt.status),
        None => PresenceState::Offline,
    };
    Ok(ActorRow {
        actor_ref: format!("agent:{}", agent.id),
        display_name: agent.name.clone(),
        subtitle: "agent".to_string(),
        presence,
        is_agent: true,
        recent_rank: None,
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
) -> Result<Option<ActorRow>, sqlx::Error> {
    let touched = AgentRepo::update_config(pool, workspace_id, agent_id, update).await?;
    if !touched {
        return Ok(None);
    }
    match AgentRepo::get(pool, agent_id).await? {
        Some(agent) => Ok(Some(agent_actor_row(pool, &agent).await?)),
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
) -> Result<Option<ActorRow>, sqlx::Error> {
    let touched = AgentRepo::set_archived(pool, workspace_id, agent_id, archived).await?;
    if !touched {
        return Ok(None);
    }
    match AgentRepo::get(pool, agent_id).await? {
        Some(agent) => Ok(Some(agent_actor_row(pool, &agent).await?)),
        None => Ok(None),
    }
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
    let mut out = Vec::with_capacity(autopilots.len());
    for ap in autopilots {
        let id = AutopilotId::from_str(ap.id.clone()).map_err(|_| AutopilotRepoError::EmptyId)?;
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
        });
    }
    Ok(out)
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
) -> Result<bool, AutopilotFireError> {
    let Some(autopilot) = AutopilotRepo::get(pool, workspace, autopilot_id).await? else {
        return Ok(false);
    };
    fire_autopilot_tick(pool, clock, &autopilot).await?;
    Ok(true)
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
) -> Result<(), AutopilotRepoError> {
    if enabled {
        AutopilotRepo::enable(pool, clock, workspace, autopilot_id).await
    } else {
        AutopilotRepo::disable(pool, workspace, autopilot_id).await
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
/// without a PR incur no `gh` call, so the fetch cost is bounded to the handful
/// of PR'd cards on a board (this snapshot fires on subscribe / workspace-switch
/// and on a run finishing, not per render frame).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store failure or a corrupt stored id.
pub async fn tasks_list(
    pool: &SqlitePool,
    workspace_id: &str,
    provider: &dyn crate::pr_status::PrStatusProvider,
) -> Result<Vec<TaskCardRow>, sqlx::Error> {
    let tasks = TaskRepo::list_by_workspace(pool, workspace_id).await?;
    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks {
        let id = ainb_hangar_core::ids::TaskId::from_str(&t.id).map_err(|e| {
            sqlx::Error::ColumnDecode {
                index: "id".to_string(),
                source: format!("malformed task id {:?}: {e}", t.id).into(),
            }
        })?;
        let pr_url = task_pr_url(t.result.as_deref());
        // Only a card that captured a PR pays for a status fetch.
        let pr_status = match pr_url.as_deref() {
            Some(url) => Some(provider.fetch(url).await),
            None => None,
        };
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

/// Snapshot a workspace's aggregated inbox + its unread count
/// (`hangar/inbox_list`, e38.14).
///
/// Reads the durable `inbox_entry` rows the daemon's aggregator folds live
/// issue / comment / task events into, newest-first and capped at
/// [`INBOX_LIST_LIMIT`], plus the count of unread (`read_at IS NULL`) entries.
/// Workspace-scoped: a foreign / unknown workspace yields an empty list + zero
/// unread.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if either store query fails.
pub async fn inbox_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<(Vec<InboxEntryRow>, i64), sqlx::Error> {
    let entries = InboxRepo::list(pool, workspace_id, INBOX_LIST_LIMIT).await?;
    let unread = InboxRepo::unread_count(pool, workspace_id).await?;
    let rows = entries
        .into_iter()
        .map(|e| InboxEntryRow {
            id: e.id,
            kind: e.kind.as_str().to_string(),
            event: e.event,
            subject_id: e.subject_id,
            summary: e.summary,
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
    Ok(rows.into_iter().map(attention_row_to_wire).collect())
}

/// Flatten one store `attention` row into its wire render shape.
fn attention_row_to_wire(row: ainb_hangar_store::repo::attention::AttentionRow) -> AttentionRow {
    AttentionRow {
        id: row.id,
        session_id: row.session_id,
        cwd: row.cwd,
        workspace_id: row.workspace_id,
        kind: row.kind.as_str().to_string(),
        payload: row.payload,
        degraded: row.degraded,
        created_at: row.created_at,
        // The resolved routing channels stamped at raise time (tcp T5) — the
        // bridge/web/atc consumers reading this feed filter on them.
        channels: row.channels,
    }
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

/// Mark every currently-unread inbox entry in `workspace` as read, returning
/// `(marked, unread_after)` (`hangar/inbox_mark_read`, e38.14).
///
/// `marked` is how many rows the sweep flipped (the unread count before);
/// `unread_after` is the unread count once the sweep commits, which is `0` for
/// this whole-workspace sweep. Idempotent — a re-sweep flips nothing. The daemon
/// resolves + rejects a mistyped workspace before this call, so a missing
/// workspace never reaches here.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the sweep or the follow-up count fails.
pub async fn inbox_mark_read(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    workspace_id: &str,
) -> Result<(i64, i64), sqlx::Error> {
    let marked = InboxRepo::mark_all_read(pool, workspace_id, clock.now_ms()).await?;
    let unread = InboxRepo::unread_count(pool, workspace_id).await?;
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
    Ok(Some(IssueRow {
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
    }))
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
    Ok(Some(IssueRow {
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
    IssueRepo::update_state(pool, issue_id, PR_MERGED_DONE_STATE).await?;
    // Re-read the now-Done row so the caller can push `IssueUpdated`.
    let row = read_issue_row(pool, workspace_id, issue_id).await?;
    Ok((status, row))
}

/// Create one new issue in `workspace_id`, then return it as a wire [`IssueRow`]
/// (`hangar/issue_create`, e38.29).
///
/// Mints a fresh ULID via `idgen`, stamps `created_at` from `clock`, and inserts
/// through [`IssueRepo::insert`] in the `open` lifecycle state. The new issue is
/// unassigned (the create flow only captures title + description); `creator` is
/// the already-parsed actor-ref. The re-wrapped [`IssueRow`] mirrors the
/// `issues_list` shape (a freshly-created issue has no completed task, so
/// `pr_url` is always `None`), so the response row and the pushed `IssueCreated`
/// event are byte-identical to a list snapshot of the row.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store fault (e.g. a `workspace_id` FK
/// violation), or a malformed minted id on the re-wrap (impossible for a ULID).
pub async fn issue_create(
    pool: &SqlitePool,
    idgen: &dyn IdGen,
    clock: &dyn HangarClock,
    workspace_id: &str,
    title: &str,
    description: Option<&str>,
    creator: &ActorRef,
) -> Result<IssueRow, sqlx::Error> {
    use ainb_hangar_store::repo::issue::NewIssue;

    let id = idgen.new_ulid();
    let created_at = clock.now_ms();
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
            priority: 0,
            due_date: None,
            labels: Vec::new(),
        },
    )
    .await?;
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
        id: issue_id,
        display_id,
        workspace_id: workspace_id.to_string(),
        title,
        description: description.map(ToString::to_string),
        state: "open".to_string(),
        assignee: None,
        creator: format!("{}:{}", creator.kind().as_str(), creator.id()),
        created_at,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
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
    body: &str,
) -> Result<Vec<String>, sqlx::Error> {
    use crate::mentions::parse_mentions;
    use ainb_hangar_store::repo::task::NewTask;

    let handles = parse_mentions(body);
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    // The workspace's agents are the only resolution candidates: a foreign
    // tenant's agent sharing a handle is never in this list, so it cannot be
    // mention-triggered here.
    let agents = AgentRepo::list_by_workspace(pool, workspace_id).await?;
    let now = clock.now_ms();
    let mut spawned = Vec::new();
    for handle in &handles {
        // Resolve by name; an unknown handle simply matches no agent (ignored).
        let Some(agent) = agents.iter().find(|a| &a.name == handle) else {
            continue;
        };
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
        };
        match TaskRepo::insert(pool, &task).await {
            Ok(_) => spawned.push(agent.id.clone()),
            // The agent already has a pending task on this issue (the per-(issue,
            // agent) unique index): coalesce, don't error. The trigger is
            // idempotent — re-mentioning an already-queued agent is a no-op.
            Err(e) if is_unique_violation(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(spawned)
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
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_store::Store;

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
            "@nobody hello and @ghost too",
        )
        .await
        .unwrap();

        assert!(spawned.is_empty());
        assert_eq!(count_for_issue3(&store).await, 0);
    }
}
