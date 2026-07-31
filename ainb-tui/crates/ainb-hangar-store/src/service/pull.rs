//! The `PullCard` service: role-gated, one-owner-per-card PULL of board work.
//!
//! This is the seam that replaces BROADCAST with PULL. Before it,
//! [`SquadAssignService::assign_fanout`](crate::service::squad_assign::SquadAssignService::assign_fanout)
//! wrote one `agent_task_queue` row per squad member, so one issue became N
//! simultaneous runs all doing the same work in N worktrees. After it, a card
//! sitting in a role-gated column IS the queue, and exactly one eligible agent
//! takes it.
//!
//! # Why the queue row is created HERE and not by a dispatcher
//!
//! `agent_task_queue.agent_id` is `NOT NULL`, so a queued row always already
//! names its agent: the pre-existing model is PUSH (a dispatcher picks the
//! agent; [`ClaimTaskService`](crate::service::claim::ClaimTaskService) merely
//! executes that choice). True pull needs the agent chosen AT CLAIM TIME.
//! Rather than make `agent_id` nullable (a full table rebuild on SQLite, against
//! a populated production database), the `board_card` row is the queue and this
//! service INSERTs the `agent_task_queue` row once an eligible agent has been
//! selected. The agent is therefore known by construction at insert time.
//!
//! The result is a two-stage pipeline that reuses the whole existing claim path
//! unchanged:
//!
//! ```text
//! board_card in a role-gated column        <- the pull queue
//!        |  PullService::pull_for_runtime  (this module: role / WIP / reviewer
//!        v                                  / blocked / capacity predicates)
//! agent_task_queue row, status 'queued'    <- the execution queue
//!        |  ClaimTaskService::claim_for_runtime  (unchanged)
//!        v
//! status 'dispatched' -> the runner
//! ```
//!
//! # Atomicity
//!
//! The whole selection-and-insert is ONE `INSERT ... SELECT ... RETURNING`
//! statement, for the same reason
//! [`CLAIM_SQL`](crate::service::claim) is one statement: SQLite serialises
//! writes, so a concurrent puller's sub-select observes this statement's
//! committed row and its one-owner-per-card guard then excludes the card. Two
//! agents can therefore never take the same card, and the guarantee holds across
//! processes, not just across tasks in one daemon.
//!
//! # The five predicates
//!
//! A `(card, agent)` pair is *pullable* when ALL of these hold:
//!
//! 1. **Role gate.** The card's column declares a `services_role` and the agent
//!    HOLDS that role. A column with `services_role IS NULL` (Backlog, Done, and
//!    every column that predates migration 0074) is not a pull queue at all, so
//!    work parks there until a human or the auto-advance moves it.
//! 2. **WIP limit.** The column currently has fewer than `wip_limit` cards
//!    holding an active task. `NULL` means unlimited.
//! 3. **One owner per card.** The card's issue has NO active
//!    (`queued`/`dispatched`/`running`) task at all. This is strictly stronger
//!    than the pre-existing per-(issue, agent) guard, and it is what makes
//!    "exactly one task running per card" true rather than merely likely.
//! 4. **Prior-agent exclusion.** On a column with `excludes_prior_agent = 1`, an
//!    agent that already holds a `done` task on this card may not take it. This
//!    is the reviewer-is-never-the-implementer rule. Note the direction of
//!    failure: if the only eligible agent implemented the card, the card WAITS
//!    rather than being self-reviewed.
//! 5. **Not blocked, and the agent has capacity.** Unfinished `card_dependency`
//!    blockers make a card unpullable at every stage (the F7 refuse-run guard,
//!    reused verbatim from
//!    [`CardDependencyRepo::unfinished_blockers_of`](crate::repo::card_dependency::CardDependencyRepo::unfinished_blockers_of)),
//!    and the agent must be under its `max_concurrent_tasks` cap.
//!
//! # How an agent's ROLES resolve
//!
//! Via `squad_member.role` (migration 0053), which until now had no consumer at
//! the dispatch seam: `SquadRepo::member_agent_ids` is documented "role-blind by
//! design", which is precisely the broadcast defect. An agent HOLDS a role when
//! any of its squad memberships, in the same workspace, names that role. No
//! agent-level roles field is introduced, so the existing
//! `SquadRepo::add_member_with_role` / `set_member_role` writers are already the
//! way an operator grants one.
//!
//! A membership's `role` is free text and is matched as a COMMA-SEPARATED TOKEN
//! set, case-insensitively and ignoring spaces, so one membership can advertise
//! several roles (`"implementer,reviewer"`) without a second table. Matching
//! uses `INSTR` over comma-delimited needles rather than `LIKE`, so a role
//! containing `%` or `_` cannot act as a wildcard and silently over-match.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use sqlx::{Row, SqlitePool};

/// A card successfully pulled into the execution queue: the freshly-inserted
/// `agent_task_queue` row plus the board position it came from, so the caller
/// can advance the card when the run finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledCard {
    /// The inserted `agent_task_queue` row id (status `queued`).
    pub task_id: String,
    /// The agent that took the card (`agent.id`).
    pub agent_id: String,
    /// The runtime the task is keyed to (`agent_runtime.id`).
    pub runtime_id: String,
    /// The card's issue (`issue.id`). A card IS an issue.
    pub issue_id: String,
    /// The board the card sits on.
    pub board_id: String,
    /// The role-gated column the card was pulled FROM.
    pub column_id: String,
    /// That column's `services_role`, i.e. the role this run is serving.
    pub services_role: String,
    /// The run generation minted for this stage.
    pub generation: i64,
}

/// Stateless pull service over `board_card` + `agent_task_queue`.
pub struct PullService;

impl PullService {
    /// Pull at most ONE eligible card for any agent bound to `runtime_id`,
    /// inserting the `queued` `agent_task_queue` row that
    /// [`ClaimTaskService::claim_for_runtime`](crate::service::claim::ClaimTaskService::claim_for_runtime)
    /// will then dispatch.
    ///
    /// Returns `Ok(None)` when nothing is pullable, which is the ordinary idle
    /// case (no role-gated card waiting, every column at its WIP limit, no agent
    /// on this runtime holding a needed role, or every candidate excluded by a
    /// guard). The caller sleeps and retries.
    ///
    /// Ordering is `issue.priority DESC, column.ord DESC, card.ord, issue_id`:
    /// urgent work first, then PULL FROM THE RIGHT, i.e. the latest stage first.
    /// Draining the end of the pipeline before admitting new work at the start
    /// is what stops cards piling up mid-flight, and it is the reason a WIP limit
    /// is a cap rather than a deadlock.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement or a row decode fails.
    #[tracing::instrument(
        name = "card.pull",
        skip(pool, idgen, clock),
        fields(runtime_id = %runtime_id, task_id = tracing::field::Empty, issue_id = tracing::field::Empty, services_role = tracing::field::Empty)
    )]
    pub async fn pull_for_runtime(
        pool: &SqlitePool,
        runtime_id: &str,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<Option<PulledCard>, sqlx::Error> {
        let task_id = idgen.new_ulid();
        let now = clock.now_ms();

        let inserted = sqlx::query(PULL_SQL)
            .bind(&task_id)
            .bind(now)
            .bind(runtime_id)
            .fetch_optional(pool)
            .await?;

        let Some(row) = inserted else { return Ok(None) };

        let issue_id: String = row.try_get("issue_id")?;
        let agent_id: String = row.try_get("agent_id")?;
        let runtime: String = row.try_get("runtime_id")?;
        let generation: i64 = row.try_get("generation")?;

        // The board position is NOT reachable from the INSERT's RETURNING (which
        // projects only the inserted task row), so it is read back here. The read
        // is race-free by construction: the card now holds an active task, so
        // predicate 3 excludes it from every concurrent pull, and nothing else
        // moves a card while it is running.
        let pos = sqlx::query(
            "SELECT bc.board_id, bc.column_id, col.services_role \
             FROM board_card AS bc \
             JOIN board_column AS col ON col.id = bc.column_id \
             WHERE bc.issue_id = ?1",
        )
        .bind(&issue_id)
        .fetch_one(pool)
        .await?;

        let pulled = PulledCard {
            task_id: row.try_get("id")?,
            agent_id,
            runtime_id: runtime,
            issue_id,
            board_id: pos.try_get("board_id")?,
            column_id: pos.try_get("column_id")?,
            services_role: pos.try_get("services_role")?,
            generation,
        };

        let span = tracing::Span::current();
        span.record("task_id", pulled.task_id.as_str());
        span.record("issue_id", pulled.issue_id.as_str());
        span.record("services_role", pulled.services_role.as_str());

        Ok(Some(pulled))
    }

    /// ADVANCE a card one column to the right after its stage finished.
    ///
    /// This is the handoff: the Implement stage completes, the card steps into
    /// Review, and a DIFFERENT role pulls it on the next tick. Call it when a
    /// task on `issue_id` reaches `done`.
    ///
    /// Advancing exactly one column at a time (never jumping to the end) is what
    /// the live proof asserts, and it is what lets each stage's
    /// `excludes_prior_agent` see the previous stage's agent.
    ///
    /// The move is refused, leaving the card exactly where it is, when:
    ///   * the card is not in a role-gated column (it is parked in Backlog/Done,
    ///     or on a board that predates migration 0074),
    ///   * the card still has an ACTIVE task, which is what makes a deliberate
    ///     `--redundant` fan-out advance only once ALL its runs have drained
    ///     rather than on the first one home,
    ///   * there is no column to the right (the last stage stays put),
    ///   * the board's master `auto_move` or the current column's `auto_move` is
    ///     off, which is the existing operator kill-switch this reuses rather
    ///     than duplicating.
    ///
    /// Returns the `(board_id, new_column_id)` of each card actually moved.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement or a row decode fails.
    #[tracing::instrument(name = "card.advance", skip(pool), fields(issue_id = %issue_id))]
    pub async fn advance_after_stage(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Vec<(String, String)>, sqlx::Error> {
        let rows = sqlx::query(ADVANCE_SQL).bind(issue_id).fetch_all(pool).await?;
        rows.iter()
            .map(|r| Ok((r.try_get("board_id")?, r.try_get("column_id")?)))
            .collect()
    }
}

/// Move a finished card one column to the right.
///
/// ONE statement, no transaction, for the reason
/// [`BoardRepo::auto_move_on_state`](crate::repo::board::BoardRepo::auto_move_on_state)
/// documents at length: a SELECT-then-UPDATE takes a read snapshot and then
/// upgrades it to a write, which `SQLite` fails with `SQLITE_BUSY_SNAPSHOT` (517)
/// whenever another daemon writer commits in that window, and `busy_timeout`
/// does NOT cover a stale snapshot. A bare UPDATE takes the write lock
/// immediately and has no snapshot to invalidate, so the card cannot silently
/// stay in its old column forever with its stage already finished.
///
/// `?1` = `issue_id`. The new `ord` appends the card to the end of its target
/// column, consistent with a manual `card_move` and with the auto-move hook.
const ADVANCE_SQL: &str = "\
UPDATE board_card \
   SET column_id = ( \
        SELECT n.id FROM board_column AS n \
         WHERE n.board_id = board_card.board_id \
           AND n.ord > (SELECT cur.ord FROM board_column AS cur \
                         WHERE cur.id = board_card.column_id) \
         ORDER BY n.ord LIMIT 1), \
       ord = ( \
        SELECT COALESCE(MAX(o.ord) + 1, 0) FROM board_card AS o \
         WHERE o.column_id = ( \
              SELECT n2.id FROM board_column AS n2 \
               WHERE n2.board_id = board_card.board_id \
                 AND n2.ord > (SELECT cur2.ord FROM board_column AS cur2 \
                                WHERE cur2.id = board_card.column_id) \
               ORDER BY n2.ord LIMIT 1)) \
 WHERE board_card.issue_id = ?1 \
   AND EXISTS ( \
        SELECT 1 FROM board_column AS cur \
          JOIN board AS bd ON bd.id = cur.board_id \
         WHERE cur.id = board_card.column_id \
           AND cur.services_role IS NOT NULL \
           AND cur.auto_move = 1 \
           AND bd.auto_move = 1 \
       ) \
   AND NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS t \
         WHERE t.issue_id = board_card.issue_id \
           AND t.status IN ('queued','dispatched','running') \
       ) \
   AND EXISTS ( \
        SELECT 1 FROM board_column AS n3 \
         WHERE n3.board_id = board_card.board_id \
           AND n3.ord > (SELECT cur3.ord FROM board_column AS cur3 \
                          WHERE cur3.id = board_card.column_id) \
       ) \
RETURNING board_id, column_id";

/// The atomic pull statement: select the most urgent pullable `(card, agent)`
/// pair for the runtime and INSERT its `queued` task row in one statement.
///
/// `?1` = new task id, `?2` = `created_at` (now), `?3` = `runtime_id`.
///
/// `agent_kind` is derived from the PULLING AGENT, not from the card. Under pull
/// the agent is chosen per stage, so the provider CLI that runs the stage must be
/// the chosen agent's own: a codex reviewer has to invoke `codex`, even when the
/// card was created with `agent_kind = 'claude'`.
///
/// `agent.provider` is the authority, NOT `agent_runtime.provider`. A runtime is
/// a DAEMON, not a provider binding: `ainb hangar agent create --provider codex`
/// records `codex` on the agent and still binds it to the single `default`
/// runtime, whose own `provider` column reads `claude` for every install. Keying
/// on the runtime therefore silently ran every agent as claude. The runtime is
/// kept only as a fallback for an agent row with no provider recorded, then the
/// card's own kind, then `claude`, so an unrecognised value still dispatches
/// rather than tripping the NOT NULL.
///
/// `generation` is `MAX(existing) + 1`: each STAGE is its own run generation, so
/// the card-state folds (aggregate, blocker-finished, auto-move) that scope to an
/// issue's latest generation see the current stage alone and are not confused by
/// the previous stage's `done` row.
///
/// `parent_task_id` chains each stage to the one before it: the most recent
/// `done` task on the card. This is the HANDOFF CHAIN, and it is what lets an
/// operator (or an assertion) walk a card's history stage by stage and see who
/// handed what to whom. The column already existed for infra retries, whose
/// chain is parent-to-child within one stage; a pipeline chain is the same edge
/// read across stages, so nothing new is introduced. The first stage of a card
/// has no `done` predecessor and correctly chains to NULL.
///
/// # Why this is `pub`
///
/// So the MUTATION PROOFS in `tests/pull_role_gate.rs` can delete exactly one
/// predicate from it and assert the corresponding invariant goes red. A test
/// that only ever runs the real statement proves the invariant holds; it cannot
/// prove the CLAUSE is what makes it hold, and would keep passing if the clause
/// were deleted and some other accident covered for it. Reading the statement
/// under test is the only way to write that proof, so it is deliberately
/// reachable rather than private.
pub const PULL_SQL: &str = "\
INSERT INTO agent_task_queue \
    (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, \
     priority, generation, agent_kind, repo_ref, squad_id, parent_task_id) \
SELECT ?1, \
       bd.workspace_id, \
       a.runtime_id, \
       a.id, \
       bc.issue_id, \
       'queued', \
       ?2, \
       i.priority, \
       (SELECT COALESCE(MAX(g.generation), 0) + 1 FROM agent_task_queue AS g \
         WHERE g.issue_id = bc.issue_id), \
       COALESCE( \
         NULLIF(CASE WHEN a.provider IN ('claude','codex','copilot') \
                     THEN a.provider END, ''), \
         (SELECT rt.provider FROM agent_runtime AS rt \
           WHERE rt.id = a.runtime_id \
             AND rt.provider IN ('claude','codex','copilot')), \
         i.agent_kind, 'claude'), \
       i.repo_ref, \
       i.squad_id, \
       (SELECT p.id FROM agent_task_queue AS p \
         WHERE p.issue_id = bc.issue_id AND p.status = 'done' \
         ORDER BY p.generation DESC, p.finished_at DESC, p.id DESC LIMIT 1) \
  FROM board_card AS bc \
  JOIN board_column AS col ON col.id = bc.column_id \
  JOIN board AS bd ON bd.id = bc.board_id \
  JOIN issue AS i ON i.id = bc.issue_id \
  JOIN agent AS a ON a.workspace_id = bd.workspace_id \
 WHERE col.services_role IS NOT NULL \
   AND a.runtime_id = ?3 \
   AND a.archived = 0 \
   AND i.state NOT IN ('closed','cancelled') \
   AND EXISTS ( \
        SELECT 1 FROM squad_member AS sm \
          JOIN squad AS sq ON sq.id = sm.squad_id \
         WHERE sm.member_type = 'agent' \
           AND sm.member_id = a.id \
           AND sq.workspace_id = bd.workspace_id \
           AND INSTR( \
                 ',' || REPLACE(LOWER(sm.role), ' ', '') || ',', \
                 ',' || LOWER(TRIM(col.services_role)) || ',' \
               ) > 0 \
       ) \
   AND ( col.wip_limit IS NULL OR ( \
        SELECT COUNT(DISTINCT w.issue_id) FROM board_card AS w \
          JOIN agent_task_queue AS wq ON wq.issue_id = w.issue_id \
         WHERE w.column_id = col.id \
           AND wq.status IN ('queued','dispatched','running') \
       ) < col.wip_limit ) \
   AND NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS o \
         WHERE o.issue_id = bc.issue_id \
           AND o.status IN ('queued','dispatched','running') \
       ) \
   AND ( col.excludes_prior_agent = 0 OR NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS p \
         WHERE p.issue_id = bc.issue_id \
           AND p.agent_id = a.id \
           AND p.status = 'done' \
       ) ) \
   AND NOT EXISTS ( \
        SELECT 1 FROM card_dependency AS d \
         WHERE d.dependent_issue_id = bc.issue_id \
           AND d.link_type = 'blocked_by' \
           AND NOT ( \
                 EXISTS ( \
                   SELECT 1 FROM agent_task_queue AS t \
                    WHERE t.issue_id = d.blocker_issue_id AND t.status = 'done' \
                      AND t.generation = (SELECT MAX(g2.generation) FROM agent_task_queue AS g2 \
                                           WHERE g2.issue_id = d.blocker_issue_id) \
                 ) \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM agent_task_queue AS t \
                    WHERE t.issue_id = d.blocker_issue_id \
                      AND t.status IN ('queued','dispatched','running') \
                      AND t.generation = (SELECT MAX(g2.generation) FROM agent_task_queue AS g2 \
                                           WHERE g2.issue_id = d.blocker_issue_id) \
                 ) \
               ) \
       ) \
   AND ( SELECT COUNT(*) FROM agent_task_queue AS r \
          WHERE r.agent_id = a.id \
            AND r.status IN ('queued','dispatched','running') \
       ) < a.max_concurrent_tasks \
 ORDER BY i.priority DESC, col.ord DESC, bc.ord, bc.issue_id \
 LIMIT 1 \
RETURNING id, agent_id, runtime_id, issue_id, generation";
