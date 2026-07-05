//! Deterministic P4 seed fixture for the e2e tripwires (`test-support` only).
//!
//! The P4.9 tmux tripwires launch `ainb tui`, which spawns the daemon against a
//! `hangar.db` in an isolated `$AINB_HANGAR_HOME`. For the screens to render
//! *live* rows, that database must already contain a workspace with a few issues
//! / agents / skills and a running task. [`seed_p4_fixture`] writes exactly that
//! into a freshly-opened store, on the `default` workspace the plugin subscribes
//! to.
//!
//! The fixture is intentionally small and stable (fixed ids + titles) so the
//! tripwires can assert exact markers: the issue-list landing screen shows the
//! `Refactor API` issue and a `Todo (3)` group count; the agent picker lists
//! `claude-agent`; the skill manager lists `commit`.
//!
//! This module is compiled only under the `test-support` feature (and `#[cfg(test)]`),
//! so it never ships in the production daemon binary.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::{AgentId, SkillId, WorkspaceId};
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use ainb_hangar_store::repo::skill::{Skill, SkillRepo};
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use sqlx::SqlitePool;

/// The seeded workspace's real row id.
///
/// Deliberately a non-slug id (a fixed ULID-style string) so the fixture matches
/// reality: real workspaces are created with a ULID `id` and a separate `slug`.
/// The plugin subscribes by slug (`"default"`, see [`WS_SLUG`]); the daemon
/// resolves that slug to this id. A fixture that bound `id == slug` would silently
/// hide the slug→id resolution bug.
pub const WS_ID: &str = "01HANGARFIXTUREWS000000000";

/// The slug the plugin subscribes by (`workspace_id: "default"` on the wire). The
/// daemon resolves this to [`WS_ID`] before scoping any snapshot query.
pub const WS_SLUG: &str = "default";

/// The three Todo issues the issue-list landing screen renders. The first is the
/// `Refactor API` marker the tripwire asserts on.
const TODO_ISSUES: &[(&str, &str)] = &[
    ("issue-1", "Refactor API"),
    ("issue-2", "Fix flaky test"),
    ("issue-3", "Add metrics dashboard"),
];

/// Seed the `default` workspace with the P4 fixture: a workspace + user + member,
/// one runtime + one agent, two skills, three Todo issues, and one running task.
///
/// Idempotent enough for a fresh db (it inserts fixed-id rows once); calling it
/// twice on the same db errors on the duplicate primary keys, which is fine — the
/// tripwire seeds exactly once into a fresh isolated home.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if any insert fails.
pub async fn seed_p4_fixture(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let now: i64 = 1_700_000_000_000;

    // Tenancy: workspace + user + member (the agent picker lists the member).
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS_ID)
        .bind(WS_SLUG)
        .bind("Default Workspace")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("alice@example.com")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
        .bind(WS_ID)
        .bind("user-1")
        .bind("owner")
        .execute(pool)
        .await?;

    seed_runtime_and_agent(pool, now).await?;

    // Skills (the skill manager lists `commit`; one is unused).
    for (id, name) in [("skill-commit", "commit"), ("skill-review", "review")] {
        SkillRepo::insert(
            pool,
            &Skill {
                id: id.into(),
                workspace_id: WS_ID.into(),
                name: name.into(),
                description: None,
                content: None,
            },
        )
        .await?;
    }
    // `commit` is referenced by the agent → renders `used`. Both the agent and
    // the skill live in `WS_ID`, so the workspace-scoped attach succeeds.
    let ws = WorkspaceId::from_str(WS_ID).expect("non-empty workspace id");
    let agent_id = AgentId::from_str("agent-1").expect("non-empty agent id");
    let skill_id = SkillId::from_str("skill-commit").expect("non-empty skill id");
    SkillRepo::attach_to_agent(pool, &ws, &agent_id, &skill_id)
        .await
        .map_err(|e| match e {
            ainb_hangar_store::repo::skill::SkillRepoError::Db(db) => db,
            // Both ids are seeded into WS_ID above, so a cross-workspace
            // rejection here is an impossible-fixture bug; surface it loudly.
            other => sqlx::Error::Protocol(format!("seed attach failed: {other}")),
        })?;

    // Three Todo issues, the first assigned to the agent so it can carry a task.
    let creator = ActorRef::new(ActorKind::Member, "user-1").expect("valid member ref");
    let agent_ref = ActorRef::new(ActorKind::Agent, "agent-1").expect("valid agent ref");
    for (i, (id, title)) in TODO_ISSUES.iter().enumerate() {
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: (*id).into(),
                workspace_id: WS_ID.into(),
                title: (*title).into(),
                description: Some(format!("Seeded issue {}", i + 1)),
                state: "open".into(),
                assignee: (i == 0).then(|| agent_ref.clone()),
                creator: creator.clone(),
                created_at: now + i64::try_from(i).unwrap_or(0),
                priority: 0,
                due_date: None,
                labels: Vec::new(),
            },
        )
        .await?;
    }

    // One running task against the first issue (drives the task-detail screen +
    // the active-task banner).
    let task_id = TaskRepo::insert(
        pool,
        &NewTask {
            id: "task-1".into(),
            workspace_id: WS_ID.into(),
            runtime_id: "runtime-1".into(),
            agent_id: "agent-1".into(),
            issue_id: Some("issue-1".into()),
            work_dir: None,
            priority: 0,
            created_at: now,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await?;
    // Promote the task to `running` and the issue to `in_progress` so the board
    // shows live work (the FSM is exercised elsewhere; here we set the columns
    // directly to make the fixture deterministic without a running daemon).
    sqlx::query("UPDATE agent_task_queue SET status = 'running', started_at = ? WHERE id = ?")
        .bind(now)
        .bind(&task_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Seed the runtime + agent rows (the agent picker lists `claude-agent` with an
/// online presence). Extracted from [`seed_p4_fixture`] so the fixture body
/// stays within the per-function line budget.
async fn seed_runtime_and_agent(pool: &SqlitePool, now: i64) -> Result<(), sqlx::Error> {
    AgentRuntimeRepo::insert(
        pool,
        &AgentRuntime {
            id: "runtime-1".into(),
            workspace_id: WS_ID.into(),
            daemon_id: "daemon-1".into(),
            provider: "claude".into(),
            runtime_mode: "local".into(),
            last_seen_at: Some(now),
            status: "online".into(),
        },
    )
    .await?;
    AgentRepo::insert(
        pool,
        &Agent {
            id: "agent-1".into(),
            workspace_id: WS_ID.into(),
            name: "claude-agent".into(),
            runtime_id: "runtime-1".into(),
            instructions: None,
            visibility: "workspace".into(),
            owner_id: "user-1".into(),
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    #[tokio::test]
    async fn seed_populates_default_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_p4_fixture(store.pool()).await.unwrap();

        let issues = crate::rpc::snapshots::issues_list(store.pool(), WS_ID).await.unwrap();
        assert_eq!(issues.len(), 3, "three seeded issues");
        assert!(issues.iter().any(|i| i.title == "Refactor API"));

        let actors = crate::rpc::snapshots::agents_list(store.pool(), WS_ID).await.unwrap();
        assert!(actors.iter().any(|a| a.display_name == "claude-agent" && a.is_agent));
        assert!(actors.iter().any(|a| !a.is_agent), "member present");

        let skills = crate::rpc::snapshots::skills_list(store.pool(), WS_ID).await.unwrap();
        assert!(skills.iter().any(|s| s.name == "commit" && s.used));
    }
}
