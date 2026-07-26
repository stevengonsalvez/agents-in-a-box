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
    /// The card's REPO (an absolute checkout path or `scratch`), stamped onto every
    /// fanned-out task so each provisions its OWN volatile worktree (tcp T4 / F7).
    /// `None` (the default) leaves the tasks with a NULL `repo_ref` — the pre-T4
    /// squads-screen fan-out, which runs in-tree.
    pub repo_ref: Option<&'a str>,
    /// The resolved provider agent-kind stamped onto every fanned-out task, so each
    /// member run routes through the right provider (tcp T4 / F7). `None` leaves the
    /// column default (`claude`). Only consulted when `repo_ref` is set.
    pub agent_kind: Option<ainb_hangar_core::agent_kind::AgentKind>,
    /// The run generation stamped onto every task of this assignment (migration
    /// 0039, tcp 8ln). The caller mints it ONCE per Run / rerun / fan-out
    /// ([`TaskRepo::next_generation_for_issue`]) so the leader + all members SHARE
    /// it — a fan-out is one run. `0` (the default) is the first run / the pre-8ln
    /// squads-screen path.
    pub generation: i64,
    /// The EFFECTIVE invoking user id the invocation-permission gate judges every
    /// dispatch target by (gap #8, multica `canInvokeAgent` parity).
    ///
    /// `None` (the [`Default`]) means **"resolve the workspace owner"**, NOT "skip
    /// the gate" — the owner branch of
    /// [`AgentRepo::can_invoke`](crate::repo::agent::AgentRepo::can_invoke) always
    /// admits, so the ordinary single-operator assign is unchanged while any future
    /// caller inherits the gate for free. A multi-user caller names a non-owner
    /// member here to be gated against each target agent's allow-list.
    ///
    /// A workspace with no `owner`-role member resolves to `""`, which matches no
    /// `agent.owner_id` and therefore denies every `private` agent — the same
    /// pre-existing behaviour the single-agent run path has (`run_card`'s
    /// `unwrap_or_default()`), kept identical here so the two paths cannot drift.
    pub invoker: Option<&'a str>,
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
    /// A dispatch target (the leader or a member) is not invocable by the effective
    /// invoker (gap #8, multica `canInvokeAgent` parity). NO task row is written —
    /// the gate runs in the pre-flight resolve phase, before the fan-out transaction
    /// opens, so a denial leaves the queue untouched rather than rolled back.
    #[error(
        "agent `{agent_id}` is not invocable by `{invoker}` — it is private or you are not on its allow-list"
    )]
    NotInvocable {
        /// The dispatch target that refused the invoker (`agent.id`).
        agent_id: String,
        /// The effective invoking user id the gate judged (`""` when the workspace
        /// has no `owner`-role member to fall back to).
        invoker: String,
    },
    /// The squad is ARCHIVED, so it refuses new work (migration 0052, multica
    /// `DeleteSquad` soft-delete parity). The guard runs in the pre-flight resolve
    /// phase, so NO task row is written — restore the squad to assign to it again.
    #[error("squad `{0}` is archived — restore it before assigning work")]
    Archived(String),
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
    /// - [`SquadAssignError::Archived`] when the squad is archived — no task row
    ///   is written.
    /// - [`SquadAssignError::NoAgentLeader`] when the squad is unknown in the
    ///   workspace or its leader is a human `member`.
    /// - [`SquadAssignError::LeaderAgentMissing`] when the leader agent row is
    ///   absent (a dangling leader ref).
    /// - [`SquadAssignError::NotInvocable`] when the effective invoker
    ///   ([`SquadAssignRequest::invoker`], else the workspace owner) may not invoke
    ///   the leader agent (gap #8) — no task row is written.
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
        // 0. An ARCHIVED squad refuses new work (migration 0052). Checked FIRST,
        //    in the pre-flight phase, so no task row is written.
        if SquadRepo::is_archived(pool, workspace, squad_id).await? {
            return Err(SquadAssignError::Archived(squad_id.to_string()));
        }

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

        // 2b. gap #8 invocation gate: the effective invoker must be permitted to
        //     invoke the leader. Denied means NO task row is written at all.
        let invoker = Self::effective_invoker(pool, workspace, request).await?;
        Self::gate(pool, &agent, &invoker).await?;

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
                generation: request.generation,
            },
        )
        .await?;
        // Stamp the dispatching squad onto the just-committed leader row (migration
        // 0045), so the daemon claim path can key a leader-briefing injection off it.
        // The insert used its own transaction; a follow-up UPDATE on the committed
        // row is fine (the row is not yet claimable-and-gone this same tick).
        TaskRepo::set_squad_id(pool, &task_id, squad_id).await?;

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
    /// The fan-out is **all-or-nothing**: every dispatch target (the leader and
    /// each `agent` member) is resolved and validated *before* a single row is
    /// written, then the leader brief and all member tasks are inserted in ONE
    /// transaction. A dangling / foreign member ref — or a mid-loop UNIQUE
    /// `(issue, agent)` collision — rolls the whole fan-out back, so the caller
    /// never sees a rejected fan-out that nonetheless left the leader (or some
    /// members) queued and running.
    ///
    /// Resolution rules: a human `member` carries no runtime and is skipped (only
    /// `agent` members reach the fan-out), the leader's own agent is skipped (its
    /// brief is the leader task), a repeated member agent is deduped — so no member
    /// row can collide with the leader (or another member) on the `(issue, agent)`
    /// guard — and every agent (leader + members) is resolved **within this
    /// workspace**: a member/leader ref that names another tenant's agent resolves
    /// to no row and is rejected, so a squad cannot borrow a foreign workspace's
    /// agent + runtime to dispatch across the tenant boundary. Each surviving
    /// member is keyed to its own `(agent_id, runtime_id)`.
    ///
    /// # Errors
    ///
    /// - [`SquadAssignError::Archived`] when the squad is archived — the whole
    ///   fan-out is refused with zero rows written.
    /// - [`SquadAssignError::NoAgentLeader`] / [`SquadAssignError::LeaderAgentMissing`]
    ///   from the leader brief (an unknown squad, a human leader, a dangling
    ///   leader ref).
    /// - [`SquadAssignError::MemberAgentMissing`] when a member's agent row is
    ///   absent (a dangling member ref).
    /// - [`SquadAssignError::NotInvocable`] when the effective invoker may not
    ///   invoke the LEADER or ANY member (gap #8). The gate runs in the pre-flight
    ///   resolve phase, so a denial writes zero rows.
    /// - [`SquadAssignError::Db`] on a store fault (resolve, lookup, enqueue).
    pub async fn assign_fanout(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
        request: &SquadAssignRequest<'_>,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<SquadFanout, SquadAssignError> {
        // Resolve + validate EVERY dispatch target before touching the queue, so a
        // dangling / foreign member ref rejects the whole fan-out up front instead
        // of leaving the leader (and earlier members) queued. Nothing is inserted
        // until all targets are known-good; then every insert lands in ONE
        // transaction (all-or-nothing).

        // 0. An ARCHIVED squad refuses new work (migration 0052), before any
        //    resolution and well before `pool.begin()`.
        if SquadRepo::is_archived(pool, workspace, squad_id).await? {
            return Err(SquadAssignError::Archived(squad_id.to_string()));
        }

        // 1. Resolve the squad's leader agent — the routing seam. A human-member
        //    leader or unknown squad resolves to `None` and is rejected. The
        //    leader agent is resolved WITHIN this workspace, so a leader ref that
        //    names a foreign tenant's agent is rejected rather than dispatched.
        let leader_agent_id = SquadRepo::leader_agent_id(pool, workspace, squad_id)
            .await?
            .ok_or(SquadAssignError::NoAgentLeader)?;
        let leader = Self::agent_in_ws(pool, workspace, &leader_agent_id)
            .await?
            .ok_or_else(|| SquadAssignError::LeaderAgentMissing(leader_agent_id.clone()))?;
        let leader_runtime_id = leader.runtime_id.clone();

        // 1b. gap #8 invocation gate: resolve the EFFECTIVE invoker ONCE, then judge
        //     every dispatch target (the leader here, each member below) by it —
        //     inside this pre-flight resolve phase, BEFORE `pool.begin()`. A denial
        //     therefore writes zero rows rather than rolling written ones back.
        let invoker = Self::effective_invoker(pool, workspace, request).await?;
        Self::gate(pool, &leader, &invoker).await?;

        // 2. Resolve the distinct `agent` members. Seed the dedupe set with the
        //    leader's agent so its brief is never double-dispatched as a member;
        //    each member agent is resolved WITHIN this workspace so a member ref
        //    cannot borrow a foreign tenant's agent + runtime.
        let mut seen = std::collections::HashSet::new();
        seen.insert(leader_agent_id.clone());

        let member_agent_ids = SquadRepo::member_agent_ids(pool, workspace, squad_id).await?;
        let mut member_targets = Vec::new();
        for agent_id in member_agent_ids {
            // Skip the leader's own agent and any repeated member agent — either
            // would collide with an already-enqueued task on the `(issue, agent)`
            // guard.
            if !seen.insert(agent_id.clone()) {
                continue;
            }
            let member = Self::agent_in_ws(pool, workspace, &agent_id)
                .await?
                .ok_or_else(|| SquadAssignError::MemberAgentMissing(agent_id.clone()))?;
            // gap #8: every fanned-out member is gated by the same effective
            // invoker as the leader — a foreign private agent dragged into a squad
            // is refused here, before any row is written.
            Self::gate(pool, &member, &invoker).await?;
            member_targets.push((agent_id, member.runtime_id));
        }

        // 3. Every target is known-good — enqueue the leader brief and all member
        //    tasks in ONE transaction so a mid-loop failure (e.g. a UNIQUE
        //    `(issue, agent)` collision) rolls the whole fan-out back, honouring
        //    the documented all-or-nothing contract.
        let leader_task_id = idgen.new_ulid();
        let mut members = Vec::with_capacity(member_targets.len());

        let mut tx = pool.begin().await?;
        TaskRepo::insert_in_tx(
            &mut tx,
            &NewTask {
                id: leader_task_id.clone(),
                workspace_id: workspace.as_str().to_string(),
                runtime_id: leader_runtime_id.clone(),
                agent_id: leader_agent_id.clone(),
                issue_id: request.issue_id.map(str::to_string),
                work_dir: request.work_dir.map(str::to_string),
                priority: request.priority,
                created_at: clock.now_ms(),
                autopilot_run_id: None,
                generation: request.generation,
            },
        )
        .await?;
        Self::stamp_dispatch_fields(&mut tx, &leader_task_id, request).await?;
        // Stamp the dispatching squad onto the leader brief (migration 0045). A
        // SEPARATE call from `stamp_dispatch_fields` (not folded in) so squad_id is
        // stamped even for the no-repo squads-screen fan-out, where
        // `stamp_dispatch_fields` early-returns on a NULL `repo_ref`.
        TaskRepo::set_squad_id_in_tx(&mut tx, &leader_task_id, squad_id).await?;
        for (agent_id, runtime_id) in member_targets {
            let task_id = idgen.new_ulid();
            TaskRepo::insert_in_tx(
                &mut tx,
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
                    generation: request.generation,
                },
            )
            .await?;
            Self::stamp_dispatch_fields(&mut tx, &task_id, request).await?;
            TaskRepo::set_squad_id_in_tx(&mut tx, &task_id, squad_id).await?;
            members.push(SquadMemberDispatch {
                task_id,
                agent_id,
                runtime_id,
            });
        }
        tx.commit().await?;

        Ok(SquadFanout {
            leader: SquadAssignment {
                task_id: leader_task_id,
                leader_agent_id,
                runtime_id: leader_runtime_id,
            },
            members,
        })
    }

    /// Stamp the card's `repo_ref` + resolved `agent_kind` onto a fanned-out task
    /// WITHIN the fan-out transaction (tcp T4 / F7), so each member run provisions
    /// its own worktree from the card's repo and routes through the right provider.
    /// A no-repo request (the pre-T4 squads-screen fan-out) is a no-op — the task
    /// keeps its NULL `repo_ref` and runs in-tree.
    async fn stamp_dispatch_fields(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
        request: &SquadAssignRequest<'_>,
    ) -> Result<(), sqlx::Error> {
        let Some(repo_ref) = request.repo_ref else {
            return Ok(());
        };
        let agent_kind =
            request.agent_kind.unwrap_or(ainb_hangar_core::agent_kind::AgentKind::DEFAULT);
        crate::repo::card_parity::CardParityRepo::set_task_repo_agent_in_tx(
            tx,
            task_id,
            Some(repo_ref),
            agent_kind,
        )
        .await
    }

    /// Resolve `agent_id` to its full agent row **within `workspace`**, returning
    /// `None` when no agent row with that id exists in the workspace — a dangling
    /// ref, or a ref that names another tenant's agent. This is the guard that stops
    /// a squad member/leader ref from borrowing a foreign workspace's agent +
    /// runtime and dispatching a task across the tenant boundary (`AgentRepo::get`
    /// alone keys only on the primary id, which is not workspace-scoped).
    ///
    /// Returns the WHOLE agent (not just its runtime) because the gap #8 invocation
    /// gate judges `permission_mode` / `owner_id` / `workspace_id`, not the runtime.
    async fn agent_in_ws(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent_id: &str,
    ) -> Result<Option<crate::repo::agent::Agent>, sqlx::Error> {
        let Some(agent) = AgentRepo::get(pool, agent_id).await? else {
            return Ok(None);
        };
        if agent.workspace_id != workspace.as_str() {
            return Ok(None);
        }
        Ok(Some(agent))
    }

    /// Resolve the EFFECTIVE invoking user id for an assignment (gap #8): the
    /// caller-supplied [`SquadAssignRequest::invoker`], else the workspace owner.
    ///
    /// Deliberately identical in shape to the single-agent run path's resolution
    /// (`run_card`), including the `unwrap_or_default()` on an owner-less workspace,
    /// so the squad seam and the single-agent seam can never drift apart.
    async fn effective_invoker(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        request: &SquadAssignRequest<'_>,
    ) -> Result<String, sqlx::Error> {
        match request.invoker {
            Some(u) => Ok(u.to_string()),
            None => Ok(
                crate::repo::workspace::WorkspaceRepo::owner_id(pool, workspace)
                    .await?
                    .unwrap_or_default(),
            ),
        }
    }

    /// Refuse a dispatch target the effective invoker may not invoke (gap #8).
    ///
    /// Both the leader and every fanned-out member are judged as the SAME
    /// [`ActorKind::Member`] invoker — the human who pressed Run — because hangar's
    /// fan-out enqueues member work directly on that human's behalf (multica's
    /// leader instead spawns member work over A2A). Gating members as an agent actor
    /// would pass a `None` originator and deny every `private` agent, which is every
    /// hangar agent by default.
    async fn gate(
        pool: &SqlitePool,
        agent: &crate::repo::agent::Agent,
        invoker: &str,
    ) -> Result<(), SquadAssignError> {
        let allowed = AgentRepo::can_invoke(
            pool,
            agent,
            ainb_hangar_core::actor::ActorKind::Member,
            Some(invoker),
        )
        .await?;
        if allowed {
            return Ok(());
        }
        Err(SquadAssignError::NotInvocable {
            agent_id: agent.id.clone(),
            invoker: invoker.to_string(),
        })
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

    /// Seed a workspace WITH its `owner`-role member (`user-1`), matching what
    /// `bootstrap::ensure_default_workspace` lays down. The gap #8 gate resolves the
    /// default invoker through `WorkspaceRepo::owner_id`, so an owner-less fixture
    /// would resolve `""` and deny every (private-by-default) agent — a fixture
    /// artefact, not the behaviour under test.
    async fn seed_ws(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(id)
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
        seed_user(pool, "user-1").await;
        sqlx::query(
            "INSERT INTO member (workspace_id, user_id, role) VALUES (?, 'user-1', 'owner')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Insert a `user` row (idempotent) — `agent.owner_id` and `member.user_id`
    /// both FK to it.
    async fn seed_user(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(format!("{id}@example.com"))
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Add `user_id` to `ws_id` as a plain (non-owner) member — the multi-user case
    /// the gap #8 allow-list exists for.
    async fn seed_plain_member(pool: &SqlitePool, ws_id: &str, user_id: &str) {
        seed_user(pool, user_id).await;
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'member')")
            .bind(ws_id)
            .bind(user_id)
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

    /// Seed an `agent_runtime` + `agent` pair bound to `runtime_id`, owned by the
    /// workspace owner (`user-1`) — the ordinary single-operator shape.
    async fn seed_agent(pool: &SqlitePool, ws_id: &str, agent_id: &str, runtime_id: &str) {
        seed_agent_owned(pool, ws_id, agent_id, runtime_id, "user-1").await;
    }

    /// Seed an `agent_runtime` + `agent` pair bound to `runtime_id`, owned by
    /// `owner_id`. Every agent is `private` (migration 0047's default), so the gap
    /// #8 gate admits only the owner until a target is added.
    async fn seed_agent_owned(
        pool: &SqlitePool,
        ws_id: &str,
        agent_id: &str,
        runtime_id: &str,
        owner_id: &str,
    ) {
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
        // across the agents these tests insert).
        seed_user(pool, owner_id).await;
        AgentRepo::insert(
            pool,
            &Agent {
                id: agent_id.into(),
                workspace_id: ws_id.into(),
                name: agent_id.into(),
                runtime_id: runtime_id.into(),
                instructions: None,
                visibility: "workspace".into(),
                permission_mode: "private".into(),
                owner_id: owner_id.into(),
                ..Agent::default()
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

        // The dispatching squad is stamped on the leader task row (migration 0045),
        // so the daemon claim path can key a leader-briefing injection off it.
        let row = TaskRepo::get_by_id(pool, &assignment.task_id).await.unwrap().unwrap();
        assert_eq!(
            row.squad_id.as_deref(),
            Some("s1"),
            "the leader-only assign stamps squad_id on the task row"
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
        // The claim projection carries the squad ref through to the daemon's
        // briefing hook (migration 0045).
        assert_eq!(
            claimed.squad_id.as_deref(),
            Some("s1"),
            "the claimed projection carries squad_id for the briefing hook"
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
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1"))
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m2"))
            .await
            .unwrap();
        // A human member carries no runtime and must NOT be fanned out.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &member_ref("u-1"))
            .await
            .unwrap();

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
            &FixedIdGen::new(vec!["task-lead".into(), "task-m1".into(), "task-m2".into()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap();

        // The leader gets the brief; both agent members fan out (the human does not).
        assert_eq!(fanout.leader.leader_agent_id, "a-lead");
        assert_eq!(fanout.leader.runtime_id, "rt-lead");
        assert_eq!(
            fanout.members.len(),
            2,
            "two agent members fanned out (human skipped)"
        );
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

        // Every fanned-out task (leader + both members) carries the dispatching
        // squad (migration 0045), so the daemon keys a briefing hook off each.
        for id in [
            &fanout.leader.task_id,
            &fanout.members[0].task_id,
            &fanout.members[1].task_id,
        ] {
            let row = TaskRepo::get_by_id(pool, id).await.unwrap().unwrap();
            assert_eq!(
                row.squad_id.as_deref(),
                Some("s1"),
                "fanned task {id} must carry squad_id"
            );
        }
    }

    /// FAN-OUT stamps the card's repo + agent-kind onto EVERY fanned task (leader
    /// + members), so each provisions its OWN worktree from the card's repo (tcp
    /// T4 / F7). The pre-T4 no-repo fan-out leaves them NULL (runs in-tree).
    #[tokio::test]
    async fn assign_fanout_stamps_the_card_repo_on_every_task() {
        use crate::repo::card_parity::CardParityRepo;
        use ainb_hangar_core::agent_kind::AgentKind;

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
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1"))
            .await
            .unwrap();

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            repo_ref: Some("/repos/app"),
            agent_kind: Some(AgentKind::Codex),
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

        // Both the leader brief and the member task carry the card's repo + kind,
        // so each provisions its own worktree from `/repos/app`.
        for id in [&fanout.leader.task_id, &fanout.members[0].task_id] {
            assert_eq!(
                CardParityRepo::get_task_repo_agent(pool, id).await.unwrap(),
                Some((Some("/repos/app".to_string()), AgentKind::Codex)),
                "fanned task {id} must carry the card repo + agent-kind"
            );
        }
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
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-lead"))
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1"))
            .await
            .unwrap();

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

    /// FAN-OUT is all-or-nothing: a dangling member ref rejects the WHOLE fan-out,
    /// leaving nothing queued — not even the leader brief. Regression guard for the
    /// non-atomic path that committed the leader before a later member insert
    /// failed, stranding a task the squad could never retry.
    #[tokio::test]
    async fn assign_fanout_rejects_atomically_leaving_no_leader_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "shippers", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        // A member whose agent row does not exist — a dangling ref.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-ghost"))
            .await
            .unwrap();

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        let err = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec!["task-lead".into(), "task-ghost".into()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::MemberAgentMissing(ref id) if id == "a-ghost"),
            "got {err:?}"
        );

        // The leader task must NOT have been committed — the fan-out rolled back
        // whole, so the leader's runtime can still claim nothing and a retry is
        // not blocked by a stranded pending task.
        let leftover = ClaimTaskService::claim_for_runtime(pool, "rt-lead", &FixedClock(10_000))
            .await
            .unwrap();
        assert!(
            leftover.is_none(),
            "the leader brief must have rolled back with the failed fan-out"
        );
    }

    /// FAN-OUT is workspace-scoped: a member ref that names an agent living in
    /// ANOTHER workspace is rejected, never dispatched. Guards against a squad
    /// borrowing a foreign tenant's agent + runtime to cross the tenant boundary.
    #[tokio::test]
    async fn assign_fanout_rejects_a_cross_workspace_member() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        // `a-foreign` lives in ws-b, on a ws-b runtime.
        seed_agent(pool, "ws-b", "a-foreign", "rt-foreign").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "shippers", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        // A member ref naming the FOREIGN-workspace agent (no FK / existence check
        // on the member side lets this ref be stored).
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-foreign"))
            .await
            .unwrap();

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        let err = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec!["task-lead".into(), "task-x".into()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::MemberAgentMissing(ref id) if id == "a-foreign"),
            "a foreign-workspace member must be rejected, got {err:?}"
        );

        // Nothing dispatched to the foreign runtime, and the fan-out rolled back
        // whole so the leader was not stranded either.
        let foreign = ClaimTaskService::claim_for_runtime(pool, "rt-foreign", &FixedClock(10_000))
            .await
            .unwrap();
        assert!(
            foreign.is_none(),
            "no task may cross into the foreign runtime"
        );
        let lead = ClaimTaskService::claim_for_runtime(pool, "rt-lead", &FixedClock(10_000))
            .await
            .unwrap();
        assert!(
            lead.is_none(),
            "the leader brief rolled back with the rejected fan-out"
        );
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

    /// How many rows the queue holds at all (the gap #8 acceptance assertion is
    /// "zero rows WRITTEN", not "rows rolled back").
    async fn queue_len(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// gap #8 — FAN-OUT MEMBER GATE: a squad member agent owned by SOMEONE ELSE and
    /// left `private` is not invocable by the assignment's effective invoker (here
    /// the workspace owner), so the whole fan-out is refused and NO task row is
    /// written — not the leader's, not the allowed member's. Allow-listing the
    /// invoker on that agent (public_to + a `member` target) then lets the very same
    /// call through, proving the gate is a real decision and not blanket denial.
    #[tokio::test]
    async fn fanout_refuses_a_private_member_agent_and_writes_no_row() {
        use crate::repo::agent::AgentRepo;
        use crate::repo::agent_invocation_target::AgentInvocationTargetRepo;
        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_core::idgen::SystemIdGen;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "agent-L", "rt-lead").await;
        seed_agent(pool, "ws-a", "agent-m1", "rt-m1").await;
        // `agent-m2` belongs to a DIFFERENT user and stays private → the workspace
        // owner is not its owner and is on no allow-list.
        seed_plain_member(pool, "ws-a", "carol").await;
        seed_agent_owned(pool, "ws-a", "agent-m2", "rt-m2", "carol").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        // Fixture sanity: an empty `owner_id` would make an agent uninvocable by
        // EVERYONE, which would make this test pass for the wrong reason.
        for id in ["agent-L", "agent-m1", "agent-m2"] {
            let agent = AgentRepo::get(pool, id).await.unwrap().unwrap();
            assert!(!agent.owner_id.is_empty(), "{id} must carry an owner");
        }

        SquadRepo::create(
            pool,
            &ws("ws-a"),
            "s1",
            "shippers",
            &agent_ref("agent-L"),
            1,
        )
        .await
        .unwrap();
        for m in ["agent-m1", "agent-m2"] {
            SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref(m)).await.unwrap();
        }

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        let ids = || FixedIdGen::new(vec!["task-lead".into(), "task-m1".into(), "task-m2".into()]);
        let err = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &ids(),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::NotInvocable { ref agent_id, .. } if agent_id == "agent-m2"),
            "the foreign private member must refuse the fan-out, got {err:?}"
        );
        assert_eq!(
            queue_len(pool).await,
            0,
            "a refused fan-out writes NO task row at all"
        );

        // CONTROL: share `agent-m2` with the workspace owner → the same call lands
        // the leader brief + both member tasks.
        AgentRepo::set_permission_mode(pool, "agent-m2", "public_to").await.unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            "agent-m2",
            "member",
            "user-1",
            None,
        )
        .await
        .unwrap();
        let fanout = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &ids(),
            &FixedClock(9_000),
        )
        .await
        .expect("an allow-listed member unblocks the fan-out");
        assert_eq!(fanout.members.len(), 2, "both members fanned out");
        assert_eq!(queue_len(pool).await, 3, "leader brief + two member tasks");
    }

    /// gap #8 — FAN-OUT LEADER GATE (the direct port of multica's
    /// `squad_private_leader_test.go` 403 case): a plain workspace member fanning an
    /// issue out to a squad whose LEADER is private and owned by someone else is
    /// refused, and nothing is enqueued.
    #[tokio::test]
    async fn fanout_refuses_a_non_owner_member_against_a_private_leader() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        // Leader + member both owned by the workspace owner and private by default.
        seed_agent(pool, "ws-a", "agent-L", "rt-lead").await;
        seed_agent(pool, "ws-a", "agent-m1", "rt-m1").await;
        seed_plain_member(pool, "ws-a", "bob").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        SquadRepo::create(
            pool,
            &ws("ws-a"),
            "s1",
            "shippers",
            &agent_ref("agent-L"),
            1,
        )
        .await
        .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("agent-m1"))
            .await
            .unwrap();

        let request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            invoker: Some("bob"),
            ..SquadAssignRequest::default()
        };
        let err = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec!["task-lead".into(), "task-m1".into()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::NotInvocable { ref agent_id, ref invoker }
                if agent_id == "agent-L" && invoker == "bob"),
            "a plain member must not fan out through a private leader, got {err:?}"
        );
        assert_eq!(
            queue_len(pool).await,
            0,
            "the private-leader refusal writes no row"
        );

        // The OWNER's fan-out over the identical squad still works — the gate bites
        // the non-owner only.
        let owner_request = SquadAssignRequest {
            issue_id: Some("issue-1"),
            ..SquadAssignRequest::default()
        };
        SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &owner_request,
            &FixedIdGen::new(vec!["task-lead".into(), "task-m1".into()]),
            &FixedClock(9_000),
        )
        .await
        .expect("the owner always fans out");
        assert_eq!(queue_len(pool).await, 2, "leader brief + one member task");
    }

    /// gap #8 — the LEADER-ONLY assign shares the same gate: a plain member cannot
    /// brief a private leader, and no task row is written.
    #[tokio::test]
    async fn assign_to_leader_refuses_a_non_owner_member() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "agent-L", "rt-lead").await;
        seed_plain_member(pool, "ws-a", "bob").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent_ref("agent-L"), 1)
            .await
            .unwrap();

        let request = SquadAssignRequest {
            invoker: Some("bob"),
            ..SquadAssignRequest::default()
        };
        let err = SquadAssignService::assign_to_leader(
            pool,
            &ws("ws-a"),
            "s1",
            &request,
            &FixedIdGen::new(vec!["task-1".to_string()]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::NotInvocable { ref agent_id, .. } if agent_id == "agent-L"),
            "got {err:?}"
        );
        assert_eq!(queue_len(pool).await, 0, "no task row for a refused assign");
    }

    /// An ARCHIVED squad refuses a leader assignment and writes NO task row
    /// (migration 0052, multica `DeleteSquad` parity). The no-write half is the
    /// point: the guard runs pre-flight, so the flag is not merely cosmetic.
    #[tokio::test]
    async fn assign_to_leader_refuses_an_archived_squad_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::set_archived(pool, &ws("ws-a"), "s1", true, Some(&member_ref("user-1")), 5_000)
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
            matches!(err, SquadAssignError::Archived(ref id) if id == "s1"),
            "got {err:?}"
        );
        assert_eq!(queue_len(pool).await, 0, "an archived squad enqueues nothing");

        // Restoring it makes the same assignment succeed — the guard is the only
        // thing that refused, not a broken fixture.
        SquadRepo::set_archived(pool, &ws("ws-a"), "s1", false, None, 6_000).await.unwrap();
        SquadAssignService::assign_to_leader(
            pool,
            &ws("ws-a"),
            "s1",
            &SquadAssignRequest::default(),
            &FixedIdGen::new(vec!["task-1".to_string()]),
            &FixedClock(9_000),
        )
        .await
        .expect("a restored squad accepts work again");
        assert_eq!(queue_len(pool).await, 1);
    }

    /// The same guard on the FAN-OUT path: an archived squad enqueues neither the
    /// leader brief nor any member task.
    #[tokio::test]
    async fn assign_fanout_refuses_an_archived_squad_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_agent(pool, "ws-a", "a-lead", "rt-lead").await;
        seed_agent(pool, "ws-a", "a-m1", "rt-m1").await;
        seed_agent(pool, "ws-a", "a-m2", "rt-m2").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent_ref("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m1")).await.unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent_ref("a-m2")).await.unwrap();
        SquadRepo::set_archived(pool, &ws("ws-a"), "s1", true, Some(&member_ref("user-1")), 5_000)
            .await
            .unwrap();

        let err = SquadAssignService::assign_fanout(
            pool,
            &ws("ws-a"),
            "s1",
            &SquadAssignRequest::default(),
            &FixedIdGen::new(vec![
                "task-1".to_string(),
                "task-2".to_string(),
                "task-3".to_string(),
            ]),
            &FixedClock(9_000),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SquadAssignError::Archived(ref id) if id == "s1"),
            "got {err:?}"
        );
        assert_eq!(
            queue_len(pool).await,
            0,
            "neither the leader brief nor any member task was written"
        );
    }
}
