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

use ainb_hangar_core::ids::{AgentId, IssueId, SkillId, WorkspaceId};
use ainb_hangar_proto::events::{ActorRow, IssueRow, PresenceState, SkillFile, SkillRow};
use ainb_hangar_proto::snapshots::{SkillDetail, SkillsSyncResult};
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
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
