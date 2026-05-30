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

use crate::events::{ActorRow, IssueRow, SkillRow};

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
        assert_eq!(serde_json::from_str::<WorkspaceScopedParams>(&s).unwrap(), p);

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
            }],
        };
        let s = serde_json::to_string(&issues).unwrap();
        assert_eq!(serde_json::from_str::<IssuesListResult>(&s).unwrap(), issues);

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
        assert_eq!(serde_json::from_str::<AgentsListResult>(&s).unwrap(), actors);

        let skills = SkillsListResult {
            skills: vec![SkillRow {
                slug: "commit".into(),
                name: "commit".into(),
                used: true,
                updated_at: 0,
            }],
        };
        let s = serde_json::to_string(&skills).unwrap();
        assert_eq!(serde_json::from_str::<SkillsListResult>(&s).unwrap(), skills);
    }
}
