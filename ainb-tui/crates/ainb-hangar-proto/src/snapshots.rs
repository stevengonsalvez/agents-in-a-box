//! Request params + result wrappers for the P4 `hangar/*` snapshot RPCs.
//!
//! Each snapshot RPC carries a `{ workspace_id }` request (except
//! [`crate::methods::HANGAR_HEALTH`], which is workspace-agnostic) and answers
//! with a thin envelope wrapping the row vec the corresponding screen renders
//! from. The row types themselves ([`crate::events::IssueRow`],
//! [`crate::events::ActorRow`], [`crate::events::SkillRow`],
//! [`crate::settings::HealthSnapshot`]) live next to the event/settings wire
//! types; this module only adds the request/response envelopes so the daemon
//! handler and the plugin client agree on the exact JSON shape.
//!
//! These are **pure wire types** — `serde` only, no host deps — matching the
//! rest of `ainb-hangar-proto`.

use serde::{Deserialize, Serialize};

use crate::events::{
    ActorRow, AutopilotRow, AutopilotRunRow, IssueRow, SkillFile, SkillRow, TaskCardRow,
};

/// The `{ workspace_id }` params shared by every workspace-scoped snapshot RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceScopedParams {
    /// The workspace whose rows to snapshot.
    pub workspace_id: String,
}

/// Result of [`crate::methods::HANGAR_ISSUES_LIST`].
///
/// Every issue row in the workspace, in daemon order (`created_at` ascending).
/// The plugin buckets them into the Todo / In Progress / Done columns
/// client-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuesListResult {
    /// The issue rows.
    pub issues: Vec<IssueRow>,
}

/// Result of [`crate::methods::HANGAR_AGENTS_LIST`]: the polymorphic actor list
/// (members + agents) the agent-picker modal renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsListResult {
    /// The actor rows (members and agents in one flat list).
    pub actors: Vec<ActorRow>,
}

/// Result of [`crate::methods::HANGAR_SKILLS_LIST`]: the workspace's skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsListResult {
    /// The skill rows.
    pub skills: Vec<SkillRow>,
}

/// Params for [`crate::methods::HANGAR_SKILL_GET`].
///
/// The workspace plus the skill id to fetch in detail. The `workspace_id` scopes
/// the lookup so a skill id from another tenant resolves to `null`, never
/// another workspace's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillGetParams {
    /// The subscribed workspace the skill must belong to.
    pub workspace_id: String,
    /// The skill id (its slug — the stable id the list rows carry).
    pub skill_id: String,
}

/// Result of [`crate::methods::HANGAR_SKILL_GET`]: one skill's full detail.
///
/// The `SKILL.md` body plus the ordered file list, rendered by the skill-manager
/// detail pane. `body` is the top-level skill content (`None` when the skill is
/// file-only); `files` is the path-ordered child list (the file tree). Distinct
/// from the list-row [`SkillRow`], which carries only name / `used` / `updated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    /// The skill slug (its stable id, matching the list row).
    pub slug: String,
    /// Human-readable skill name.
    pub name: String,
    /// Short description; `None` when unset.
    pub description: Option<String>,
    /// The top-level `SKILL.md` body; `None` when the skill is file-only.
    pub body: Option<String>,
    /// The skill's child files, ordered by `path` (the file tree).
    pub files: Vec<SkillFile>,
}

/// Params for [`crate::methods::HANGAR_SKILLS_SYNC`]: the target workspace plus
/// an optional source directory override.
///
/// When `source_path` is `None` the daemon resolves the source the same way the
/// CLI does (`$AINB_TOOLKIT_SKILLS_DIR`, else a walk to `ainb-toolkit/skills`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSyncParams {
    /// The workspace to import into.
    pub workspace_id: String,
    /// Override the source directory (`<name>/SKILL.md` shaped), or `None` to
    /// resolve the default toolkit source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

/// Result of [`crate::methods::HANGAR_SKILLS_SYNC`]: the imported skill names.
///
/// `imported` is the kebab-case names of every skill the sync upserted (in
/// import order); `count` is its length, carried explicitly so the plugin can
/// surface "Imported N skills" without re-counting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSyncResult {
    /// The imported skill names, in import order.
    pub imported: Vec<String>,
    /// The number of skills imported (`imported.len()`).
    pub count: usize,
}

/// Params for `hangar/skill_attach` / `hangar/skill_detach`.
///
/// The workspace plus the agent + skill to (de)associate. The `workspace_id` is
/// the tenant-isolation guard — the daemon verifies both ids belong to it before
/// mutating the junction. See [`crate::methods::HANGAR_SKILL_ATTACH`] /
/// [`crate::methods::HANGAR_SKILL_DETACH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillAttachParams {
    /// The subscribed workspace both ids must belong to.
    pub workspace_id: String,
    /// The agent the skill is attached to / detached from.
    pub agent_id: String,
    /// The skill being (de)attached.
    pub skill_id: String,
}

/// Result of [`crate::methods::HANGAR_AUTOPILOTS_LIST`]: the workspace's
/// autopilots (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotsListResult {
    /// The autopilot rows, ordered by name.
    pub autopilots: Vec<AutopilotRow>,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_RUNS`].
///
/// The workspace (tenant guard) plus the autopilot id and a row cap. The
/// `workspace_id` scopes the lookup so a foreign autopilot id yields an empty
/// run set, never another tenant's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRunsParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot whose runs to list.
    pub autopilot_id: String,
    /// Maximum number of runs to return (latest-first).
    pub limit: u32,
}

/// Result of [`crate::methods::HANGAR_AUTOPILOT_RUNS`]: one autopilot's recent
/// runs, latest-first (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRunsResult {
    /// The run rows, latest-first, capped at the requested limit.
    pub runs: Vec<AutopilotRunRow>,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_FIRE_NOW`]: the workspace
/// (tenant guard) plus the autopilot to fire immediately (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotFireNowParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot to fire now (bypassing the schedule).
    pub autopilot_id: String,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_SET_ENABLED`]: the workspace
/// (tenant guard), the autopilot, and the target enabled flag (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotSetEnabledParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot to toggle.
    pub autopilot_id: String,
    /// `true` enables (recompute next-tick from now); `false` disables.
    pub enabled: bool,
}

/// Result of [`crate::methods::HANGAR_TASKS_LIST`]: every task in the workspace
/// for the Kanban board (P8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksListResult {
    /// The task card rows, in daemon order (`created_at` ascending). The plugin
    /// buckets them into the four board columns by their `status`.
    pub tasks: Vec<TaskCardRow>,
}

/// Params for [`crate::methods::HANGAR_TASK_TRANSITION`]: the workspace (tenant
/// guard), the task to move, and the target status (P8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTransitionParams {
    /// The subscribed workspace the task must belong to.
    pub workspace_id: String,
    /// The task to move.
    pub task_id: String,
    /// The target lifecycle status — one of the six `TaskStatus` wire tokens.
    pub to_status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PresenceState;

    /// The params + result envelopes round-trip through JSON.
    #[test]
    fn envelopes_roundtrip() {
        let p = WorkspaceScopedParams {
            workspace_id: "ws-1".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspaceScopedParams>(&s).unwrap(),
            p
        );

        let issues = IssuesListResult {
            issues: vec![IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("i1").unwrap(),
                workspace_id: "ws-1".into(),
                title: "Refactor API".into(),
                description: None,
                state: "open".into(),
                assignee: None,
                creator: "member:alice".into(),
                created_at: 0,
                pr_url: None,
            }],
        };
        let s = serde_json::to_string(&issues).unwrap();
        assert_eq!(
            serde_json::from_str::<IssuesListResult>(&s).unwrap(),
            issues
        );

        let actors = AgentsListResult {
            actors: vec![ActorRow {
                actor_ref: "agent:a1".into(),
                display_name: "claude-agent".into(),
                subtitle: "agent · claude".into(),
                presence: PresenceState::Online,
                is_agent: true,
                recent_rank: Some(0),
            }],
        };
        let s = serde_json::to_string(&actors).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentsListResult>(&s).unwrap(),
            actors
        );

        let skills = SkillsListResult {
            skills: vec![SkillRow {
                slug: "commit".into(),
                name: "commit".into(),
                used: true,
                updated_at: 0,
            }],
        };
        let s = serde_json::to_string(&skills).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillsListResult>(&s).unwrap(),
            skills
        );
    }

    /// The P6.5 skill-management envelopes round-trip through JSON.
    #[test]
    fn p6_skill_envelopes_roundtrip() {
        let get = SkillGetParams {
            workspace_id: "ws-1".into(),
            skill_id: "skill-commit".into(),
        };
        let s = serde_json::to_string(&get).unwrap();
        assert_eq!(serde_json::from_str::<SkillGetParams>(&s).unwrap(), get);

        let detail = SkillDetail {
            slug: "commit".into(),
            name: "commit".into(),
            description: Some("Create well-formatted commits".into()),
            body: Some("# Commit\n".into()),
            files: vec![SkillFile {
                path: "SKILL.md".into(),
            }],
        };
        let s = serde_json::to_string(&detail).unwrap();
        assert_eq!(serde_json::from_str::<SkillDetail>(&s).unwrap(), detail);

        let sync = SkillsSyncParams {
            workspace_id: "ws-1".into(),
            source_path: Some("/tmp/skills".into()),
        };
        let s = serde_json::to_string(&sync).unwrap();
        assert_eq!(serde_json::from_str::<SkillsSyncParams>(&s).unwrap(), sync);
        // `source_path` is omitted when None.
        let no_src = SkillsSyncParams {
            workspace_id: "ws-1".into(),
            source_path: None,
        };
        assert_eq!(
            serde_json::to_string(&no_src).unwrap(),
            "{\"workspace_id\":\"ws-1\"}"
        );

        let report = SkillsSyncResult {
            imported: vec!["commit".into(), "review".into()],
            count: 2,
        };
        let s = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillsSyncResult>(&s).unwrap(),
            report
        );

        let attach = SkillAttachParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            skill_id: "skill-commit".into(),
        };
        let s = serde_json::to_string(&attach).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillAttachParams>(&s).unwrap(),
            attach
        );
    }

    /// The P7.5 autopilot envelopes round-trip through JSON.
    #[test]
    fn p7_autopilot_envelopes_roundtrip() {
        let list = AutopilotsListResult {
            autopilots: vec![AutopilotRow {
                id: "ap-1".into(),
                workspace_id: "ws-1".into(),
                agent_id: "agent-1".into(),
                name: "daily-triage".into(),
                cron_expr: "0 9 * * 1-5".into(),
                next_tick_at: Some(1_700_000_000_000),
                enabled: true,
                last_run_status: Some("completed".into()),
                last_run_at: Some(1_699_000_000_000),
            }],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotsListResult>(&s).unwrap(),
            list
        );

        let runs_params = AutopilotRunsParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
            limit: 10,
        };
        let s = serde_json::to_string(&runs_params).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotRunsParams>(&s).unwrap(),
            runs_params
        );

        let runs = AutopilotRunsResult {
            runs: vec![AutopilotRunRow {
                id: "run-1".into(),
                autopilot_id: "ap-1".into(),
                started_at: 1_699_000_000_000,
                completed_at: Some(1_699_000_120_000),
                status: "completed".into(),
            }],
        };
        let s = serde_json::to_string(&runs).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotRunsResult>(&s).unwrap(),
            runs
        );

        let fire = AutopilotFireNowParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
        };
        let s = serde_json::to_string(&fire).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotFireNowParams>(&s).unwrap(),
            fire
        );

        let toggle = AutopilotSetEnabledParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
            enabled: false,
        };
        let s = serde_json::to_string(&toggle).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotSetEnabledParams>(&s).unwrap(),
            toggle
        );
    }

    /// The P8.4 Kanban task envelopes round-trip through JSON.
    #[test]
    fn p8_task_envelopes_roundtrip() {
        let list = TasksListResult {
            tasks: vec![TaskCardRow {
                id: ainb_hangar_core::ids::TaskId::from_str("task-1").unwrap(),
                workspace_id: "ws-1".into(),
                agent_id: "agent-1".into(),
                issue_id: Some("issue-1".into()),
                status: "running".into(),
                created_at: 1_700_000_000_000,
            }],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<TasksListResult>(&s).unwrap(), list);

        let transition = TaskTransitionParams {
            workspace_id: "ws-1".into(),
            task_id: "task-1".into(),
            to_status: "done".into(),
        };
        let s = serde_json::to_string(&transition).unwrap();
        assert_eq!(
            serde_json::from_str::<TaskTransitionParams>(&s).unwrap(),
            transition
        );
    }
}
