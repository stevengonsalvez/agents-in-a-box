//! The `SquadAssign` service: turn a squad assignment into a routed task (e38.17).
//!
//! This is the product seam that makes **leader routing actually take effect**.
//! [`SquadRepo::leader_agent_id`](crate::repo::squad::SquadRepo::leader_agent_id)
//! resolves a squad to its leader's agent id, but a resolver alone routes
//! nothing — something has to convert a squad assignment into a concrete
//! `agent_task_queue` row the existing claim/dispatch path picks up.
//! [`SquadAssignService::assign_to_leader`] is that something: given a squad, it
//!
//! 1. resolves the squad's LEADER to its agent id (the routing seam), rejecting a
//!    human-member leader (no agent to dispatch to) and an unknown squad;
//! 2. looks the leader agent up to derive its **runtime** — the runtime the
//!    leader is bound to, NOT a caller-supplied string — so the claim path keys
//!    the task to the leader's runtime;
//! 3. enqueues a [`NewTask`] carrying the leader's `agent_id` + the leader's
//!    `runtime_id`, so [`ClaimTaskService::claim_for_runtime`] routes the task to
//!    the leader and nobody else.
//!
//! The runtime is **derived from the squad's leader**, not passed in: a caller
//! names the squad (and optionally the issue), and the service alone turns that
//! into the `(agent_id, runtime_id)` pair the queue is keyed on. That is what
//! distinguishes a real routing path from a test that hand-builds the task.
//!
//! [`ClaimTaskService::claim_for_runtime`]: crate::service::claim::ClaimTaskService::claim_for_runtime
//! [`NewTask`]: crate::repo::task::NewTask

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

use crate::repo::agent::AgentRepo;
use crate::repo::squad::SquadRepo;
use crate::repo::task::{NewTask, TaskRepo};

/// The task knobs a squad assignment rides through to the enqueued row.
///
/// Carries the issue the task works (or `None` for an ad-hoc task), the run's
/// working directory, and the claim priority. Bundled so the assignment call
/// stays narrow and the optional fields default cleanly (`Default` = no issue, no
/// work-dir, priority `0`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SquadAssignRequest<'a> {
    /// The issue the routed task carries (`issue.id`), or `None` for an ad-hoc task.
    pub issue_id: Option<&'a str>,
    /// The run's working directory, or `None`.
    pub work_dir: Option<&'a str>,
    /// Claim urgency (0..3, higher = more urgent); `0` is the routine default.
    pub priority: i64,
}

/// The outcome of a successful squad assignment: the enqueued task plus the
/// leader identity it routed to, so a caller can report *who* the work landed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadAssignment {
    /// The enqueued `agent_task_queue` row id.
    pub task_id: String,
    /// The leader agent the task was routed to (`agent.id`).
    pub leader_agent_id: String,
    /// The runtime the task was keyed to (the leader agent's `runtime_id`).
    pub runtime_id: String,
}

/// Why a squad assignment could not be routed.
#[derive(Debug, thiserror::Error)]
pub enum SquadAssignError {
    /// No squad with that id exists in the workspace, or its leader is a human
    /// `member` (a human carries no agent to dispatch to). Either way there is no
    /// agent to route the work to, so the assignment is rejected.
    #[error("squad has no agent leader to route to (unknown squad or a human leader)")]
    NoAgentLeader,
    /// The squad's leader agent row is missing (a dangling leader ref). The
    /// assignment is rejected rather than enqueueing a task for an unknown runtime.
    #[error("squad leader agent `{0}` not found")]
    LeaderAgentMissing(String),
    /// A squad member's agent row is missing (a dangling member ref) during a
    /// fan-out. The whole fan-out is rejected rather than enqueueing a task for an
    /// unknown runtime — the caller re-issues once the member ref is fixed.
    #[error("squad member agent `{0}` not found")]
    MemberAgentMissing(String),
    /// An underlying store failure (resolve, lookup, or enqueue).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// One member task enqueued by a fan-out: the row id plus the member agent /
/// runtime it routed to (P7). The peer of [`SquadAssignment`] for the fanned-out
/// members — named for the member, not the leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadMemberDispatch {
    /// The enqueued `agent_task_queue` row id.
    pub task_id: String,
    /// The member agent the task was routed to (`agent.id`).
    pub agent_id: String,
    /// The runtime the task was keyed to (the member agent's `runtime_id`).
    pub runtime_id: String,
}

/// The outcome of a squad *fan-out* (P7): the LEADER's brief task plus one task
/// per distinct `agent` member, all on the SAME issue.
///
/// The per-(issue, agent) claim guard (migration `0012`) is what makes this real
/// — the leader and every member each hold their own pending task on the one
/// issue and claim it in parallel on their own runtime. A human `member` and the
/// leader's own agent are never double-dispatched (the leader task is the brief;
/// the members list carries the fanned-out agents, deduped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadFanout {
    /// The leader's brief task (identical to [`SquadAssignService::assign_to_leader`]).
    pub leader: SquadAssignment,
    /// One dispatch per distinct `agent` member (the leader's agent excluded),
    /// ordered by member agent id.
    pub members: Vec<SquadMemberDispatch>,
}

/// Stateless service that routes a squad assignment to the squad's leader.
pub struct SquadAssignService;

impl SquadAssignService {
    /// Assign work to `squad` by enqueueing a task routed to the squad's LEADER.
    ///
    /// Resolves the squad's leader agent id (the routing seam), derives that
    /// agent's runtime, and enqueues a [`NewTask`] keyed to the leader's
    /// `(agent_id, runtime_id)` so the existing claim path dispatches it to the
    /// leader. The task knobs (`issue_id` / `work_dir` / `priority`) ride through
    /// from `request`.
    ///
    /// Returns the enqueued task id together with the leader identity it routed
    /// to. The runtime is **derived from the leader**, never supplied by the
    /// caller — this is the seam a test cannot fake by hand-building the task.
    ///
    /// # Errors
    ///
    /// - [`SquadAssignError::NoAgentLeader`] when the squad is unknown in the
    ///   workspace or its leader is a human `member`.
    /// - [`SquadAssignError::LeaderAgentMissing`] when the leader agent row is
    ///   absent (a dangling leader ref).
    /// - [`SquadAssignError::Db`] on a store fault (resolve, lookup, enqueue —
    ///   notably a UNIQUE violation when a pending task already exists for the
    ///   leader on the same issue).
    pub async fn assign_to_leader(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
        request: &SquadAssignRequest<'_>,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<SquadAssignment, SquadAssignError> {
        // 1. Resolve the squad to its leader's agent id — the routing seam. A
        //    human-member leader (or unknown squad) resolves to `None`: there is
        //    no agent to dispatch to, so the assignment is rejected.
        let leader_agent_id = SquadRepo::leader_agent_id(pool, workspace, squad_id)
            .await?
            .ok_or(SquadAssignError::NoAgentLeader)?;

        // 2. Derive the leader agent's runtime — the queue is `runtime_id`-keyed,
        //    so the task must carry the LEADER's runtime, not a caller string.
        let agent = AgentRepo::get(pool, &leader_agent_id)
            .await?
            .ok_or_else(|| SquadAssignError::LeaderAgentMissing(leader_agent_id.clone()))?;
        let runtime_id = agent.runtime_id.clone();

        // 3. Enqueue a task keyed to the leader's `(agent_id, runtime_id)` — the
        //    existing claim/dispatch path routes it to the leader and nobody else.
        let task_id = idgen.new_ulid();
        TaskRepo::insert(
            pool,
            &NewTask {
                id: task_id.clone(),
                workspace_id: workspace.as_str().to_string(),
                runtime_id: runtime_id.clone(),
                agent_id: leader_agent_id.clone(),
                issue_id: request.issue_id.map(str::to_string),
                work_dir: request.work_dir.map(str::to_string),
                priority: request.priority,
                created_at: clock.now_ms(),
                autopilot_run_id: None,
            },
        )
        .await?;

        Ok(SquadAssignment {
            task_id,
            leader_agent_id,
            runtime_id,
        })
    }

    /// Fan `issue`/work out across the WHOLE squad (P7): brief the LEADER *and*
    /// enqueue one task per distinct `agent` member, all keyed to the same issue.
    ///
    /// This is the seam the phase-P7 acceptance turns on — "issue assigned to a
    /// squad → leader + ≥2 member tasks claimable in parallel". It works ONLY
    /// because migration `0012` scoped the pending-task guard to `(issue, agent)`:
    /// the leader and every member each hold their own pending task on the one
    /// issue, and each claims it on its own runtime with no contention.
    ///
    /// The leader brief is enqueued first (reusing [`Self::assign_to_leader`], so
    /// a human-leader / unknown squad is rejected up front). Then the squad's
    /// `agent` members are resolved: a human `member` carries no runtime and is
    /// skipped, the leader's own agent is skipped (its brief is the leader task),
    /// and a repeated member agent is deduped — so no member row can collide with
    /// the leader (or another member) on the `(issue, agent)` guard. Each surviving
    /// member is resolved to its runtime and enqueued keyed to its own
    /// `(agent_id, runtime_id)`.
    ///
    /// # Errors
    ///
    /// - [`SquadAssignError::NoAgentLeader`] / [`SquadAssignError::LeaderAgentMissing`]
    ///   from the leader brief (an unknown squad, a human leader, a dangling
    ///   leader ref).
    /// - [`SquadAssignError::MemberAgentMissing`] when a member's agent row is
    ///   absent (a dangling member ref).
    /// - [`SquadAssignError::Db`] on a store fault (resolve, lookup, enqueue).
    pub async fn assign_fanout(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
        request: &SquadAssignRequest<'_>,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<SquadFanout, SquadAssignError> {
        // 1. Brief the leader (the guard for human-leader / unknown squad).
        let leader = Self::assign_to_leader(pool, workspace, squad_id, request, idgen, clock).await?;

        // 2. Fan out to the distinct `agent` members. Seed the dedupe set with the
        //    leader's agent so its brief is never double-dispatched as a member.
        let mut seen = std::collections::HashSet::new();
        seen.insert(leader.leader_agent_id.clone());

        let member_agent_ids = SquadRepo::member_agent_ids(pool, workspace, squad_id).await?;
        let mut members = Vec::new();
        for agent_id in member_agent_ids {
            // Skip the leader's own agent and any repeated member agent — either
            // would collide with an already-enqueued task on the `(issue, agent)`
            // guard.
            if !seen.insert(agent_id.clone()) {
                continue;
            }
            let agent = AgentRepo::get(pool, &agent_id)
                .await?
                .ok_or_else(|| SquadAssignError::MemberAgentMissing(agent_id.clone()))?;
            let runtime_id = agent.runtime_id.clone();
            let task_id = idgen.new_ulid();
            TaskRepo::insert(
                pool,
                &NewTask {
                    id: task_id.clone(),
                    workspace_id: workspace.as_str().to_string(),
                    runtime_id: runtime_id.clone(),
                    agent_id: agent_id.clone(),
                    issue_id: request.issue_id.map(str::to_string),
                    work_dir: request.work_dir.map(str::to_string),
                    priority: request.priority,
                    created_at: clock.now_ms(),
                    autopilot_run_id: None,
                },
            )
            .await?;
            members.push(SquadMemberDispatch {
                task_id,
                agent_id,
                runtime_id,
            });
        }

        Ok(SquadFanout { leader, members })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::service::claim::ClaimTaskService;
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_core::clock::FixedClock;
    use ainb_hangar_core::idgen::FixedIdGen;

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    fn agent_ref(id: &str) -> ActorRef {
        ActorRef::new(ActorKind::Agent, id).unwrap()
    }

    fn member_ref(id: &str) -> ActorRef {
        ActorRef::new(ActorKind::Member, id).unwrap()
    }

    async fn seed_ws(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(id)
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Seed a minimal `issue` row so a fanned-out task's `issue_id` FK resolves.
    async fn seed_issue(pool: &SqlitePool, ws_id: &str, issue_id: &str) {
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, 'member', 'u-1', 0)",
        )
        .bind(issue_id)
        .bind(ws_id)
        .bind("fan-out issue")
        .execute(pool)
        .await
        .unwrap();
    }

    /// Seed an `agent_runtime` + `agent` pair bound to `runtime_id`.
    async fn seed_agent(pool: &SqlitePool, ws_id: &str, agent_id: &str, runtime_id: &str) {
        use crate::repo::agent::{Agent, AgentRepo};
        use crate::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};

        AgentRuntimeRepo::insert(
            pool,
            &AgentRuntime {
                id: runtime_id.into(),
                workspace_id: ws_id.into(),
                daemon_id: "daemon-1".into(),
                // Vary provider per runtime so two runtimes in one workspace do
                // not collide on the `(workspace_id, daemon_id, provider)` index.
                provider: format!("provider-{runtime_id}"),
                runtime_mode: "local".into(),
                last_seen_at: Some(1),
                status: "online".into(),
            },
        )
        .await
        .unwrap();
        // `agent.owner_id` FKs to `user(id)`; seed the owner once (idempotent
        // across the two agents these tests insert).
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
            .bind("user-1")
            .bind("owner@example.com")
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
        AgentRepo::insert(
            pool,
            &Agent {
                id: agent_id.into(),
                workspace_id: ws_id.into(),
                name: agent_id.into(),
                runtime_id: runtime_id.into(),
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
        .await
        .unwrap();
    }

    /// The real routing path: `assign_to_leader` DERIVES the leader's runtime from
    /// the squad and enqueues a task the leader's runtime then claims — without the
    /// caller ever naming the agent or runtime.
    #[tokio::test]
    async fn assign_to_leader_routes_a_task_the_leader_runtime_claims() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        // The leader agent lives on `rt-lead`; a decoy agent lives on `rt-other`.
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        seed_agent(pool, "ws-a", "a-other", "rt-other").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent_ref("a-lead"), 1)
            .await
            .unwrap();

        // Assign to the SQUAD — naming only the squad, never the agent/runtime.
        let assignment = SquadAssignService::assign_to_leader(
            pool,
            &ws("ws-a"),
            "s1",
            &SquadAssignRequest::default(),
            &FixedIdGen::new(vec!["task-1".to_string()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap();
        assert_eq!(assignment.leader_agent_id, "a-lead");
        assert_eq!(
            assignment.runtime_id, "rt-lead",
            "the runtime was DERIVED from the squad's leader, not supplied"
        );

        // The LEADER's runtime claims the squad task — routing took effect.
        let claimed = ClaimTaskService::claim_for_runtime(pool, "rt-lead", &FixedClock(10_000))
            .await
            .unwrap()
            .expect("the leader's runtime claims the squad task");
        assert_eq!(claimed.id, assignment.task_id);
        assert_eq!(
            claimed.agent_id, "a-lead",
            "dispatched to the LEADER agent, not anyone else"
        );

        // The OTHER runtime claims nothing — the task is the leader's alone.
        let other = ClaimTaskService::claim_for_runtime(pool, "rt-other", &FixedClock(10_000))
            .await
            .unwrap();
        assert!(
            other.is_none(),
            "the squad task is not claimable by another runtime"
        );
    }

    /// FAN-OUT (P7): assigning an issue to a squad briefs the LEADER *and*
    /// enqueues one task per distinct `agent` member — all on the SAME issue — and
    /// every runtime (leader + each member) claims its own task in parallel. This
    /// is the acceptance the per-(issue, agent) guard (migration `0012`) unlocks:
    /// three agents each hold a pending task on one issue at once.
    #[tokio::test]
    async fn assign_fanout_briefs_the_leader_and_fans_members_claimable_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        // A leader + two member agents, each on its own runtime.
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        seed_agent(pool, "ws-a", "a-m1", "rt-m1").await;
        seed_agent(pool, "ws-a", "a-m2", "rt-m2").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "shippers", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1")).await.unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m2")).await.unwrap();
        // A human member carries no runtime and must NOT be fanned out.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &member_ref("u-1")).await.unwrap();

        // Fan out an ISSUE across the whole squad — naming only the squad + issue.
        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        let fanout = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec![
                "task-lead".into(),
                "task-m1".into(),
                "task-m2".into(),
            ]),
            &FixedClock(9_000),
        )
        .await
        .unwrap();

        // The leader gets the brief; both agent members fan out (the human does not).
        assert_eq!(fanout.leader.leader_agent_id, "a-lead");
        assert_eq!(fanout.leader.runtime_id, "rt-lead");
        assert_eq!(fanout.members.len(), 2, "two agent members fanned out (human skipped)");
        assert_eq!(fanout.members[0].agent_id, "a-m1");
        assert_eq!(fanout.members[0].runtime_id, "rt-m1");
        assert_eq!(fanout.members[1].agent_id, "a-m2");
        assert_eq!(fanout.members[1].runtime_id, "rt-m2");

        // PARALLEL CLAIM: every runtime claims its own task on the one issue — the
        // per-(issue, agent) guard lets three pending tasks coexist on `issue-1`.
        let clock = FixedClock(10_000);
        let lead = ClaimTaskService::claim_for_runtime(pool, "rt-lead", &clock)
            .await
            .unwrap()
            .expect("leader claims the brief");
        assert_eq!(lead.agent_id, "a-lead");
        assert_eq!(lead.id, fanout.leader.task_id);

        let m1 = ClaimTaskService::claim_for_runtime(pool, "rt-m1", &clock)
            .await
            .unwrap()
            .expect("member 1 claims its task in parallel");
        assert_eq!(m1.agent_id, "a-m1");
        assert_eq!(m1.id, fanout.members[0].task_id);

        let m2 = ClaimTaskService::claim_for_runtime(pool, "rt-m2", &clock)
            .await
            .unwrap()
            .expect("member 2 claims its task in parallel");
        assert_eq!(m2.agent_id, "a-m2");
        assert_eq!(m2.id, fanout.members[1].task_id);

        // All three tasks are distinct rows on the same issue.
        assert_eq!(lead.issue_id.as_deref(), Some("issue-1"));
        assert_eq!(m1.issue_id.as_deref(), Some("issue-1"));
        assert_eq!(m2.issue_id.as_deref(), Some("issue-1"));
    }

    /// FAN-OUT dedupe: a squad whose LEADER is also listed as a member does not
    /// double-dispatch — the leader's agent appears only as the brief, never again
    /// as a fanned-out member (which would collide on the `(issue, agent)` guard).
    #[tokio::test]
    async fn assign_fanout_never_double_dispatches_the_leader() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        seed_agent(pool, "ws-a", "a-m1", "rt-m1").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "shippers", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        // The leader is redundantly listed as a member, plus a real member.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-lead")).await.unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1")).await.unwrap();

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        let fanout = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec!["task-lead".into(), "task-m1".into()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap();

        assert_eq!(fanout.leader.leader_agent_id, "a-lead");
        assert_eq!(
            fanout.members.len(),
            1,
            "only the non-leader member fans out; the leader is not re-dispatched"
        );
        assert_eq!(fanout.members[0].agent_id, "a-m1");
    }

    /// A squad with a human-member leader has no agent to route to: the assignment
    /// is rejected rather than silently enqueueing nothing.
    #[tokio::test]
    async fn assign_to_leader_rejects_a_human_leader() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &member_ref("u-lead"), 1)
            .await
            .unwrap();

        let err = SquadAssignService::assign_to_leader(
            pool,
            &ws("ws-a"),
            "s1",
            &SquadAssignRequest::default(),
            &FixedIdGen::new(vec!["task-1".to_string()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::NoAgentLeader),
            "got {err:?}"
        );
    }

    /// An unknown squad id (or a foreign-tenant one) resolves to no agent leader,
    /// so the assignment is rejected — no cross-tenant routing.
    #[tokio::test]
    async fn assign_to_leader_rejects_an_unknown_squad() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent_ref("a-lead"), 1)
            .await
            .unwrap();

        // Resolving s1 through the WRONG tenant finds no agent leader.
        let err = SquadAssignService::assign_to_leader(
            pool,
            &ws("ws-b"),
            "s1",
            &SquadAssignRequest::default(),
            &FixedIdGen::new(vec!["task-1".to_string()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::NoAgentLeader),
            "got {err:?}"
        );
    }
}
