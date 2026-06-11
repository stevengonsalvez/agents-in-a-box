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
/// CLI does (`$AINB_TOOLKIT_SKILLS_DIR`, else a walk to `toolkit/packages/skills`).
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

/// A three-state edit instruction for a nullable issue field (e38.8).
///
/// A bare `Option<T>` collapses "leave this field unchanged" and "clear this
/// field to NULL" into the same `None`, which a partial-update RPC must keep
/// distinct. This wrapper separates the two:
///
/// - the **key omitted** in the request JSON → `Keep` (leave the column as-is);
/// - the key present as **`null`** → `Clear` (set the column to NULL);
/// - the key present with a **value** → `Set(value)` (overwrite the column).
///
/// `#[serde(untagged)]` lets a wire `null` decode as `Clear` and a wire value as
/// `Set`; the *absent* case is supplied by `#[serde(default)]` on the field that
/// holds this wrapper (which yields [`FieldUpdate::Keep`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum FieldUpdate<T> {
    /// The key was present with an explicit `null`: clear the column to NULL.
    Clear,
    /// The key was present with a value: overwrite the column with it.
    Set(T),
    /// The key was omitted: leave the column unchanged. The `default` variant so
    /// `#[serde(default)]` on the holding field maps an absent key to this.
    #[serde(skip)]
    #[default]
    Keep,
}

impl<T> FieldUpdate<T> {
    /// `true` when this update leaves the column unchanged (the omitted case).
    #[must_use]
    pub const fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }
}

/// Params for [`crate::methods::HANGAR_ISSUE_UPDATE`] (e38.8): edit one issue's
/// fields, scoped to a workspace.
///
/// `workspace_id` + `issue_id` identify the row (the workspace is the
/// tenant-isolation guard — a foreign-tenant issue id touches nothing). The
/// remaining fields are partial-update instructions: a non-nullable field uses
/// `Option<T>` (`None` = leave unchanged) and a nullable field uses
/// [`FieldUpdate`] (omitted = leave, `null` = clear, value = set). The editable
/// fields are the real `issue` columns — `state` / `assignee` / `priority` /
/// `due_date`. There is no `project` column at v1, so project is not editable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueUpdateParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to edit (`issue.id`).
    pub issue_id: String,
    /// New lifecycle state (e.g. `"in_progress"`); `None` leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// New assignee actor-ref (`"agent:<id>"` / `"member:<id>"`); omitted leaves
    /// it, an explicit `null` clears the assignee (unassign).
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub assignee: FieldUpdate<String>,
    /// New urgency `0..3` (P3..P0, HIGHER = MORE URGENT); `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// New due date (epoch milliseconds); omitted leaves it, explicit `null`
    /// clears the deadline.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub due_date: FieldUpdate<i64>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_LABEL_ATTACH`] /
/// [`crate::methods::HANGAR_ISSUE_LABEL_DETACH`] (e38.10): attach or detach a
/// label on one issue, scoped to a workspace.
///
/// `workspace_id` + `issue_id` identify the target row (the workspace is the
/// tenant-isolation guard — a foreign-tenant issue id touches nothing). `name`
/// is the label name, resolved (or, on attach, created) within the workspace.
/// `color` is an optional presentation hint applied only when an attach mints a
/// fresh label; it is ignored on detach and when an existing label is reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueLabelParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to (de)label (`issue.id`).
    pub issue_id: String,
    /// The label name (resolved or, on attach, created within the workspace).
    pub name: String,
    /// Optional presentation colour (hex) applied when an attach mints a fresh
    /// label; omitted when unset, ignored on detach / label reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_CREATE`] (e38.29): create one new
/// issue in a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one. `title` is the issue title (mandatory + non-blank).
/// `description` is optional free-form body text. `creator` is the polymorphic
/// actor-ref (`"agent:<id>"` / `"member:<id>"`) the daemon parses. The daemon
/// mints the id, stamps `created_at`, and inserts the row in the `open` state —
/// the client supplies none of those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueCreateParams {
    /// The subscribed workspace the new issue belongs to (tenant guard).
    pub workspace_id: String,
    /// The issue title (mandatory, non-blank).
    pub title: String,
    /// Optional free-form description; omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The creating actor in canonical `member:<id>` / `agent:<id>` form.
    pub creator: String,
}

/// Params for [`crate::methods::HANGAR_AGENT_UPDATE`] (e38.15): edit one agent's
/// config knobs, scoped to a workspace.
///
/// `workspace_id` + `agent_id` identify the row (the workspace is the
/// tenant-isolation guard — a foreign-tenant agent id touches nothing). The
/// remaining fields are partial-update instructions: `name` is non-nullable so
/// it uses `Option<String>` (`None` = leave); the four nullable text fields use
/// [`FieldUpdate`] (omitted = leave, `null` = clear to the column default, value
/// = set); and the two JSON collection fields (`cli_args` / `agent_env`) use a
/// plain `Option` (an empty collection is a valid "no args" / "no env" value,
/// distinct from leaving the field). `agent_env` is an ordered key-value list so
/// it serialises deterministically.
///
/// This bead persists + exposes the config; the provider EXEC consumption of
/// `model` / `cli_args` is a separate bead (e38.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentUpdateParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent to edit (`agent.id`).
    pub agent_id: String,
    /// New agent name; `None` leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New instructions; omitted leaves it, `null` clears it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub instructions: FieldUpdate<String>,
    /// New model override; omitted leaves it, `null` clears it (provider
    /// default), a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub model: FieldUpdate<String>,
    /// New CLI-args list; `None` leaves it (an empty list is a valid "no args").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_args: Option<Vec<String>>,
    /// New MCP config (raw JSON-object string); omitted leaves it, `null` clears
    /// it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub mcp_config: FieldUpdate<String>,
    /// New thinking level; omitted leaves it, `null` clears it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub thinking: FieldUpdate<String>,
    /// New per-agent env map (ordered key-value pairs); `None` leaves it (an
    /// empty list is a valid "no env").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_env: Option<Vec<(String, String)>>,
}

/// Params for [`crate::methods::HANGAR_AGENT_ARCHIVE`] (e38.15): archive or
/// un-archive one agent, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// scopes the flip by `(agent_id, workspace_id)` so a foreign-tenant agent id
/// archives nothing. `archived: true` hides the agent from the active picker;
/// `false` restores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentArchiveParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent to (un)archive (`agent.id`).
    pub agent_id: String,
    /// `true` archives (hides from the active picker); `false` restores.
    pub archived: bool,
}

/// Params for [`crate::methods::HANGAR_COMMENT_ADD`] (e38.5): append one comment
/// to an issue, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one, and scopes the insert by `(issue_id, workspace_id)` so
/// a cross-tenant issue id writes no comment. `author` is the polymorphic
/// actor-ref (`"agent:<id>"` / `"member:<id>"`) the daemon parses; `body` is the
/// comment text. Both are mandatory (a comment with no author or empty body is
/// rejected with `INVALID_PARAMS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CommentAddParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to comment on (`issue.id`).
    pub issue_id: String,
    /// The comment author in canonical `member:<id>` / `agent:<id>` form.
    pub author: String,
    /// The comment body text.
    pub body: String,
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
                priority: 0,
                due_date: None,
                labels: Vec::new(),
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
                priority: 2,
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

    /// A pre-priority snapshot (no `priority` key) still decodes: the field
    /// defaults to 0 (P3, routine), so an old daemon's `tasks_list` stays
    /// readable.
    #[test]
    fn p8_task_card_priority_defaults_when_absent() {
        let json = r#"{"id":"task-1","workspace_id":"ws-1","agent_id":"agent-1","issue_id":null,"status":"queued","created_at":1}"#;
        let row: TaskCardRow = serde_json::from_str(json).unwrap();
        assert_eq!(row.priority, 0, "absent priority decodes to the P3 default");
    }

    /// `IssueUpdateParams` round-trips, and the three-state nullable wrapper
    /// keeps "omitted" (`Keep`), "explicit null" (`Clear`), and "value" (`Set`)
    /// distinct on the wire (e38.8).
    #[test]
    fn e38_issue_update_params_roundtrip_and_three_state() {
        // A full edit: change state + reassign + bump priority + set a due date.
        let full = IssueUpdateParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
            state: Some("in_progress".into()),
            assignee: FieldUpdate::Set("agent:a1".into()),
            priority: Some(3),
            due_date: FieldUpdate::Set(1_700_000_000_000),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<IssueUpdateParams>(&s).unwrap(), full);

        // The minimal edit (only the row identity) omits every optional field, so
        // each decodes to its leave-unchanged variant.
        let minimal = r#"{"workspace_id":"ws-1","issue_id":"issue-1"}"#;
        let p: IssueUpdateParams = serde_json::from_str(minimal).unwrap();
        assert_eq!(p.state, None, "absent state leaves it unchanged");
        assert_eq!(p.priority, None, "absent priority leaves it unchanged");
        assert!(p.assignee.is_keep(), "absent assignee leaves it unchanged");
        assert!(p.due_date.is_keep(), "absent due_date leaves it unchanged");
        // Keep-valued fields are omitted on re-serialize (byte-minimal request).
        assert_eq!(serde_json::to_string(&p).unwrap(), minimal);

        // An explicit `null` is the CLEAR instruction, distinct from omission.
        let clear =
            r#"{"workspace_id":"ws-1","issue_id":"issue-1","assignee":null,"due_date":null}"#;
        let p: IssueUpdateParams = serde_json::from_str(clear).unwrap();
        assert_eq!(p.assignee, FieldUpdate::Clear, "null assignee → clear");
        assert_eq!(p.due_date, FieldUpdate::Clear, "null due_date → clear");
    }

    /// `CommentAddParams` round-trips through JSON with all four mandatory fields
    /// preserved (e38.5).
    #[test]
    fn e38_comment_add_params_roundtrip() {
        let params = CommentAddParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
            author: "member:alice".into(),
            body: "looks good to me".into(),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<CommentAddParams>(&s).unwrap(),
            params
        );
    }

    /// `AgentUpdateParams` round-trips, and the three-state nullable wrapper keeps
    /// "omitted" (`Keep`), "explicit null" (`Clear`), and "value" (`Set`) distinct
    /// for the text knobs (e38.15).
    #[test]
    fn e38_agent_update_params_roundtrip_and_three_state() {
        // A full edit: rename + set every config knob.
        let full = AgentUpdateParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            name: Some("Builder Pro".into()),
            instructions: FieldUpdate::Set("Be precise.".into()),
            model: FieldUpdate::Set("claude-opus-4".into()),
            cli_args: Some(vec!["--verbose".into()]),
            mcp_config: FieldUpdate::Set(r#"{"servers":{}}"#.into()),
            thinking: FieldUpdate::Set("high".into()),
            agent_env: Some(vec![("FOO".into(), "bar".into())]),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<AgentUpdateParams>(&s).unwrap(), full);

        // The minimal edit (only the row identity) omits every optional field, so
        // each decodes to its leave-unchanged variant and re-serialises byte-minimal.
        let minimal = r#"{"workspace_id":"ws-1","agent_id":"agent-1"}"#;
        let p: AgentUpdateParams = serde_json::from_str(minimal).unwrap();
        assert_eq!(p.name, None, "absent name leaves it unchanged");
        assert!(p.instructions.is_keep(), "absent instructions leaves it");
        assert!(p.model.is_keep(), "absent model leaves it");
        assert_eq!(p.cli_args, None, "absent cli_args leaves it");
        assert!(p.mcp_config.is_keep(), "absent mcp_config leaves it");
        assert!(p.thinking.is_keep(), "absent thinking leaves it");
        assert_eq!(p.agent_env, None, "absent agent_env leaves it");
        assert_eq!(serde_json::to_string(&p).unwrap(), minimal);

        // An explicit `null` is the CLEAR instruction, distinct from omission.
        let clear = r#"{"workspace_id":"ws-1","agent_id":"agent-1","model":null,"thinking":null}"#;
        let p: AgentUpdateParams = serde_json::from_str(clear).unwrap();
        assert_eq!(p.model, FieldUpdate::Clear, "null model → clear");
        assert_eq!(p.thinking, FieldUpdate::Clear, "null thinking → clear");
    }

    /// `AgentArchiveParams` round-trips through JSON (e38.15).
    #[test]
    fn e38_agent_archive_params_roundtrip() {
        let params = AgentArchiveParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            archived: true,
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentArchiveParams>(&s).unwrap(),
            params
        );
    }
}
