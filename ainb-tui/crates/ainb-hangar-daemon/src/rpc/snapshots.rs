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

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, IssueId, SkillId, WorkspaceId};
use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_proto::events::{
    ActorRow, AutopilotRow, AutopilotRunRow, IssueRow, PresenceState, SkillFile, SkillRow,
    TaskCardRow,
};
use ainb_hangar_proto::snapshots::{SkillDetail, SkillsSyncResult};
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, AutopilotRepoError};
use ainb_hangar_store::repo::autopilot_run::{FireError, fire_autopilot_tick};
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
use ainb_hangar_store::repo::task::TaskRepo;
use sqlx::{Row, SqlitePool};

/// Every Hangar issue lifecycle state, queried per-state and concatenated so the
/// snapshot carries the whole board. `IssueRepo` lists by `(workspace, state)`,
/// so the snapshot unions the canonical states; an issue in an unknown state is
/// still surfaced by the catch-all `"open"`-style buckets the plugin groups
/// under Todo.
const ISSUE_STATES: &[&str] = &["open", "todo", "in_progress", "done", "closed"];

/// Snapshot every issue in `workspace_id`, mapped to wire [`IssueRow`]s.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if any per-state query fails.
pub async fn issues_list(
    pool: &SqlitePool,
    workspace_id: &str,
) -> Result<Vec<IssueRow>, sqlx::Error> {
    let mut out = Vec::new();
    for state in ISSUE_STATES {
        for issue in IssueRepo::list_by_workspace_state(pool, workspace_id, state).await? {
            let id = IssueId::from_str(&issue.id).map_err(|e| sqlx::Error::ColumnDecode {
                index: "id".to_string(),
                source: format!("malformed issue id {:?}: {e}", issue.id).into(),
            })?;
            out.push(IssueRow {
                id,
                workspace_id: issue.workspace_id,
                title: issue.title,
                description: issue.description,
                state: issue.state,
                assignee: issue.assignee.map(|a| format!("{}:{}", a.kind().as_str(), a.id())),
                creator: format!("{}:{}", issue.creator.kind().as_str(), issue.creator.id()),
                created_at: issue.created_at,
            });
        }
    }
    Ok(out)
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
        let presence = match AgentRuntimeRepo::get(pool, &agent.runtime_id).await? {
            Some(rt) => presence_from_status(&rt.status),
            None => PresenceState::Offline,
        };
        out.push(ActorRow {
            actor_ref: format!("agent:{}", agent.id),
            display_name: agent.name,
            subtitle: "agent".to_string(),
            presence,
            is_agent: true,
            recent_rank: None,
        });
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
/// Kanban board (`hangar/tasks_list`, P8.4).
///
/// Carries every lifecycle status (terminal rows included) so the board can
/// bucket the six statuses into its four columns; a foreign workspace yields an
/// empty set.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] on a store failure or a corrupt stored id.
pub async fn tasks_list(
    pool: &SqlitePool,
    workspace_id: &str,
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
        out.push(TaskCardRow {
            id,
            workspace_id: t.workspace_id,
            agent_id: t.agent_id,
            issue_id: t.issue_id,
            status: t.status,
            created_at: t.created_at,
        });
    }
    Ok(out)
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
