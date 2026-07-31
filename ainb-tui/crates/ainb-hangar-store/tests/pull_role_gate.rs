//! `PullService` integration tests: the three claim-path invariants that make a
//! squad behave like an engineering team instead of a broadcast.
//!
//! Exercises the atomic role-gated pull against a real ephemeral `SQLite` WAL
//! database, one isolated tempdir store per test so parallel cargo threads never
//! share state.
//!
//! The invariants under test (goal success criterion 3):
//!   (a) an agent cannot pull from a column whose `services_role` it does not hold,
//!   (b) a column at its `wip_limit` yields no further pulls,
//!   (c) an agent cannot pull a review stage for a card it implemented.
//!
//! Plus the supporting contract: one owner per card, blocked cards are never
//! pullable, per-agent capacity and priority ordering survive, and an ungated
//! column (`services_role IS NULL`, e.g. Backlog / Done) is not a queue at all.
//!
//! # Every invariant carries a MUTATION PROOF
//!
//! A test that only runs the real statement proves the invariant HOLDS. It
//! cannot prove that the clause under test is WHAT MAKES it hold: delete the
//! clause and the test may well keep passing because some other predicate
//! happened to exclude the same row. So each invariant is paired with a
//! `mutation_proof_*` test that deletes exactly that clause from
//! [`PULL_SQL`] and asserts the forbidden pull NOW SUCCEEDS.
//!
//! That pairing is the "fails before the change, passes after" evidence, made
//! permanent and re-runnable rather than a one-off screenshot of a red run: the
//! mutant IS the pre-change code for that one predicate. If someone later
//! weakens a clause, its mutation proof stops distinguishing itself from the
//! real statement and goes red.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::service::pull::{PULL_SQL, PullService};
use sqlx::{Row, SqlitePool};

/// Frozen "now" so `created_at` assertions are deterministic.
const NOW_MS: i64 = 1_700_000_500_000;

/// Deterministic id generator: hands out a caller-supplied sequence so an
/// assertion can name the exact task id a pull will mint.
struct SeqIdGen {
    next: std::sync::Mutex<Vec<String>>,
}

impl SeqIdGen {
    fn new(ids: &[&str]) -> Self {
        Self {
            next: std::sync::Mutex::new(ids.iter().rev().map(|s| (*s).to_string()).collect()),
        }
    }
}

impl IdGen for SeqIdGen {
    fn new_ulid(&self) -> String {
        self.next.lock().expect("idgen lock").pop().expect("ran out of seeded ids")
    }
}

/// A minimal but REAL world: one workspace, one runtime, and a board whose
/// columns the individual tests then gate however they need.
///
/// Deliberately built from raw SQL against the migrated schema rather than
/// through the repos, so a test asserts the STATEMENT's behaviour and cannot be
/// masked by a repo-level guard that happens to reject the same case.
async fn seed_world(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .expect("workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1','a@x.dev',0)")
        .execute(pool)
        .await
        .expect("user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','d-1','claude','local')",
    )
    .execute(pool)
    .await
    .expect("runtime");
    sqlx::query(
        "INSERT INTO board (id, workspace_id, name, created_at) VALUES ('b-1','ws-1','B',0)",
    )
    .execute(pool)
    .await
    .expect("board");
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
                 VALUES ('sq-1','ws-1','team1','agent','ag-lead',0)",
    )
    .execute(pool)
    .await
    .expect("squad");
}

/// Add an agent, optionally granting it a comma-separated role set via a
/// `squad_member` row. `roles = ""` means the agent joins the squad with NO
/// stated role, which is exactly the shape of the user's live database.
async fn add_agent(pool: &SqlitePool, id: &str, roles: &str, max_concurrent: i64) {
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?1,'ws-1',?1,'rt-1','workspace','u-1',?2)",
    )
    .bind(id)
    .bind(max_concurrent)
    .execute(pool)
    .await
    .expect("agent");
    sqlx::query(
        "INSERT INTO squad_member (squad_id, member_type, member_id, role) \
         VALUES ('sq-1','agent',?1,?2)",
    )
    .bind(id)
    .bind(roles)
    .execute(pool)
    .await
    .expect("squad member");
}

/// Add a board column. `role = None` leaves it UNGATED (not a pull queue).
async fn add_column(
    pool: &SqlitePool,
    id: &str,
    ord: i64,
    role: Option<&str>,
    wip: Option<i64>,
    excludes_prior: bool,
) {
    sqlx::query(
        "INSERT INTO board_column \
         (id, board_id, ord, name, fsm_state, auto_move, services_role, wip_limit, excludes_prior_agent) \
         VALUES (?1,'b-1',?2,?1,NULL,1,?3,?4,?5)",
    )
    .bind(id)
    .bind(ord)
    .bind(role)
    .bind(wip)
    .bind(i64::from(excludes_prior))
    .execute(pool)
    .await
    .expect("column");
}

/// Add an issue with NO board card. Used for blockers, which gate a dependent
/// through `card_dependency` without themselves needing to sit on the board.
async fn add_issue(pool: &SqlitePool, issue_id: &str, priority: i64) {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, priority) \
         VALUES (?1,'ws-1',?1,'open','member','u-1',0,?2)",
    )
    .bind(issue_id)
    .bind(priority)
    .execute(pool)
    .await
    .expect("issue");
}

/// Add an issue and park its card in `column_id`.
async fn add_card(pool: &SqlitePool, issue_id: &str, column_id: &str, priority: i64) {
    add_issue(pool, issue_id, priority).await;
    sqlx::query(
        "INSERT INTO board_card (board_id, issue_id, column_id, added_at, ord) \
         VALUES ('b-1',?1,?2,0,0)",
    )
    .bind(issue_id)
    .bind(column_id)
    .execute(pool)
    .await
    .expect("card");
}

/// Insert a task row directly, for setting up prior / concurrent state.
async fn add_task(
    pool: &SqlitePool,
    id: &str,
    agent_id: &str,
    issue_id: &str,
    status: &str,
    generation: i64,
) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, generation) \
         VALUES (?1,'ws-1','rt-1',?2,?3,?4,0,?5)",
    )
    .bind(id)
    .bind(agent_id)
    .bind(issue_id)
    .bind(status)
    .bind(generation)
    .execute(pool)
    .await
    .expect("task");
}

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    (dir, store)
}

/// Run the real pull once.
async fn pull(pool: &SqlitePool, id: &str) -> Option<ainb_hangar_store::service::pull::PulledCard> {
    PullService::pull_for_runtime(pool, "rt-1", &SeqIdGen::new(&[id]), &FixedClock(NOW_MS))
        .await
        .expect("pull runs")
}

/// Run a MUTANT of the pull statement: [`PULL_SQL`] with `clause` deleted.
///
/// Panics if `clause` is not found verbatim, so a reworded predicate cannot
/// silently turn a mutation proof into a no-op that "passes" while testing
/// nothing.
async fn pull_mutant(pool: &SqlitePool, id: &str, clause: &str) -> Option<String> {
    assert!(
        PULL_SQL.contains(clause),
        "mutation target not found verbatim in PULL_SQL; the proof would test nothing:\n{clause}"
    );
    let mutated = PULL_SQL.replace(clause, " ");
    let row = sqlx::query(&mutated)
        .bind(id)
        .bind(NOW_MS)
        .bind("rt-1")
        .fetch_optional(pool)
        .await
        .expect("mutant runs");
    row.map(|r| r.try_get::<String, _>("agent_id").expect("agent_id"))
}

// ---------------------------------------------------------------------------
// Invariant (a): the ROLE GATE
// ---------------------------------------------------------------------------

/// An agent whose roles do not include the column's `services_role` cannot pull
/// from it, and the card simply WAITS rather than being silently dropped.
#[tokio::test]
async fn agent_without_the_role_cannot_pull() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    // The agent services QA, the column wants an implementer.
    add_agent(pool, "ag-qa", "tester", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;

    assert!(
        pull(pool, "t-1").await.is_none(),
        "a tester must not pull an implementer stage"
    );

    // The card is still queued on the board, untouched: not dropped, not moved.
    let col: String = sqlx::query_scalar("SELECT column_id FROM board_card WHERE issue_id='i-1'")
        .fetch_one(pool)
        .await
        .expect("card still parked");
    assert_eq!(col, "col-impl");
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
        .fetch_one(pool)
        .await
        .expect("count");
    assert_eq!(tasks, 0, "a refused pull writes no row at all");
}

/// The same world, with the ROLE PREDICATE DELETED, DOES pull. This is the
/// pre-change behaviour: role-blind dispatch, which is exactly what
/// `SquadRepo::member_agent_ids` is documented to do today.
#[tokio::test]
async fn mutation_proof_a_without_the_role_predicate_the_wrong_agent_pulls() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-qa", "tester", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;

    let winner = pull_mutant(pool, "t-1", ROLE_GATE_CLAUSE).await;
    assert_eq!(
        winner.as_deref(),
        Some("ag-qa"),
        "without the role gate the tester takes the implementer stage, \
         which is the defect this predicate fixes"
    );
}

/// A membership may advertise SEVERAL roles as a comma-separated set, so one
/// agent can service more than one stage without a second table.
#[tokio::test]
async fn multi_role_membership_matches_any_of_its_roles() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-multi", "Implementer, Reviewer", 5).await;
    add_column(pool, "col-rev", 1, Some("reviewer"), None, false).await;
    add_card(pool, "i-1", "col-rev", 0).await;

    let got = pull(pool, "t-1").await.expect("multi-role agent pulls");
    assert_eq!(got.agent_id, "ag-multi");
    assert_eq!(got.services_role, "reviewer");
}

/// Role matching must not be a substring match: an agent holding `review` does
/// NOT thereby hold `reviewer`, and vice versa. Token matching is what keeps
/// `INSTR` honest.
#[tokio::test]
async fn role_matching_is_token_exact_not_substring() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-partial", "review", 5).await;
    add_column(pool, "col-rev", 1, Some("reviewer"), None, false).await;
    add_card(pool, "i-1", "col-rev", 0).await;

    assert!(
        pull(pool, "t-1").await.is_none(),
        "'review' must not satisfy 'reviewer'"
    );
}

/// A membership with NO stated role (the shape of BOTH members of the user's
/// live squad `team1`) holds no roles and can pull nothing. The pipeline is
/// opt-in: an operator must actually grant roles.
#[tokio::test]
async fn roleless_membership_holds_no_roles() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-blank", "", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;

    assert!(
        pull(pool, "t-1").await.is_none(),
        "an empty role grants nothing"
    );
}

/// An UNGATED column (`services_role IS NULL`) is not a pull queue. Backlog and
/// Done are the shipped examples, and so is every column that predates
/// migration 0074, which is what makes the migration inert on existing boards.
#[tokio::test]
async fn ungated_column_is_not_a_queue() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-any", "implementer,reviewer,tester,triager", 5).await;
    add_column(pool, "col-backlog", 0, None, None, false).await;
    add_card(pool, "i-1", "col-backlog", 0).await;

    assert!(
        pull(pool, "t-1").await.is_none(),
        "no role satisfies an ungated column, so Backlog never auto-runs"
    );
}

// ---------------------------------------------------------------------------
// Invariant (b): the WIP LIMIT
// ---------------------------------------------------------------------------

/// A column at its `wip_limit` yields no further pulls; the next card waits.
#[tokio::test]
async fn column_at_wip_limit_yields_no_pull() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-impl", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), Some(1), false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_card(pool, "i-2", "col-impl", 0).await;
    // i-1 already occupies the column's single WIP slot.
    add_task(pool, "t-existing", "ag-impl", "i-1", "running", 1).await;

    assert!(pull(pool, "t-2").await.is_none(), "the column is full");

    // Free the slot the way the real pipeline does: the run finishes and the
    // card ADVANCES out of the column. The SAME world now yields a pull,
    // proving the refusal was the WIP cap and not some unrelated exclusion.
    sqlx::query("UPDATE agent_task_queue SET status='done' WHERE id='t-existing'")
        .execute(pool)
        .await
        .expect("drain the slot");
    add_column(pool, "col-done", 9, None, None, false).await;
    sqlx::query("UPDATE board_card SET column_id='col-done' WHERE issue_id='i-1'")
        .execute(pool)
        .await
        .expect("advance the finished card out of the column");

    let got = pull(pool, "t-2").await.expect("slot freed, next card pulls");
    assert_eq!(got.issue_id, "i-2");
}

/// With the WIP PREDICATE DELETED the over-limit pull succeeds, proving the
/// clause is what enforces the cap.
#[tokio::test]
async fn mutation_proof_b_without_the_wip_predicate_the_column_overfills() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-impl", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), Some(1), false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_card(pool, "i-2", "col-impl", 0).await;
    add_task(pool, "t-existing", "ag-impl", "i-1", "running", 1).await;

    let winner = pull_mutant(pool, "t-2", WIP_LIMIT_CLAUSE).await;
    assert_eq!(
        winner.as_deref(),
        Some("ag-impl"),
        "without the WIP predicate a full column keeps handing out work"
    );
}

/// `wip_limit IS NULL` means unlimited, which is the pre-0074 semantics every
/// existing column inherits.
#[tokio::test]
async fn null_wip_limit_is_unlimited() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-impl", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_card(pool, "i-2", "col-impl", 0).await;
    add_task(pool, "t-existing", "ag-impl", "i-1", "running", 1).await;

    assert!(
        pull(pool, "t-2").await.is_some(),
        "an uncapped column keeps pulling"
    );
}

// ---------------------------------------------------------------------------
// Invariant (c): the REVIEWER IS NEVER THE IMPLEMENTER
// ---------------------------------------------------------------------------

/// An agent that already finished a task on the card cannot pull it again at a
/// stage flagged `excludes_prior_agent`. With only one eligible agent the card
/// WAITS rather than being self-reviewed, which is the documented edge case.
#[tokio::test]
async fn implementer_cannot_pull_the_review_stage_of_its_own_card() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-solo", "implementer,reviewer", 5).await;
    add_column(pool, "col-rev", 2, Some("reviewer"), None, true).await;
    add_card(pool, "i-1", "col-rev", 0).await;
    // ag-solo already implemented this card in the previous stage.
    add_task(pool, "t-impl", "ag-solo", "i-1", "done", 1).await;

    assert!(
        pull(pool, "t-rev").await.is_none(),
        "the implementer must not review its own card, even as the only candidate"
    );

    // A DIFFERENT reviewer takes it immediately, proving the exclusion is scoped
    // to the prior agent and does not simply freeze the stage.
    add_agent(pool, "ag-other", "reviewer", 5).await;
    let got = pull(pool, "t-rev").await.expect("a fresh reviewer pulls");
    assert_eq!(got.agent_id, "ag-other");
}

/// With the PRIOR-AGENT PREDICATE DELETED the implementer reviews its own work.
#[tokio::test]
async fn mutation_proof_c_without_the_exclusion_the_implementer_self_reviews() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-solo", "implementer,reviewer", 5).await;
    add_column(pool, "col-rev", 2, Some("reviewer"), None, true).await;
    add_card(pool, "i-1", "col-rev", 0).await;
    add_task(pool, "t-impl", "ag-solo", "i-1", "done", 1).await;

    let winner = pull_mutant(pool, "t-rev", PRIOR_AGENT_CLAUSE).await;
    assert_eq!(
        winner.as_deref(),
        Some("ag-solo"),
        "without the exclusion the implementer reviews its own card"
    );
}

/// The exclusion is OPT-IN per column: a stage that does not set
/// `excludes_prior_agent` still lets a prior agent continue, which is what makes
/// a one-agent roster usable at all.
#[tokio::test]
async fn exclusion_is_opt_in_per_column() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-solo", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_task(pool, "t-prior", "ag-solo", "i-1", "done", 1).await;

    assert!(
        pull(pool, "t-2").await.is_some(),
        "an unflagged stage does not exclude"
    );
}

// ---------------------------------------------------------------------------
// The supporting contract
// ---------------------------------------------------------------------------

/// ONE OWNER PER CARD: a card that already has an active task is not pullable by
/// anyone, however many eligible agents exist. This is the invariant the live
/// proof asserts as "never more than one running task per card".
#[tokio::test]
async fn card_with_an_active_task_is_not_pullable_by_anyone() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-a", "implementer", 5).await;
    add_agent(pool, "ag-b", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_task(pool, "t-running", "ag-a", "i-1", "running", 1).await;

    assert!(
        pull(pool, "t-2").await.is_none(),
        "a second agent must not join a running card"
    );
}

/// With the ONE-OWNER PREDICATE DELETED a second agent piles onto the running
/// card. This is the broadcast defect in miniature.
#[tokio::test]
async fn mutation_proof_without_one_owner_a_second_agent_piles_on() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-a", "implementer", 5).await;
    add_agent(pool, "ag-b", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_task(pool, "t-running", "ag-a", "i-1", "running", 1).await;

    let winner = pull_mutant(pool, "t-2", ONE_OWNER_CLAUSE).await;
    assert!(
        winner.is_some(),
        "without the one-owner guard the already-running card is handed out again"
    );
    // WHICH agent piles on is not the point (the statement has no agent
    // tie-break, so either eligible agent may win). The defect is that the card
    // now carries TWO simultaneous runs, which is the broadcast bug in miniature.
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_queue \
         WHERE issue_id='i-1' AND status IN ('queued','dispatched','running')",
    )
    .fetch_one(pool)
    .await
    .expect("count active");
    assert_eq!(
        active, 2,
        "the mutant put a second simultaneous run on one card"
    );
}

/// Two pulls in a row never hand the same card to two agents: the first takes
/// it, the second finds nothing.
#[tokio::test]
async fn sequential_pulls_never_double_assign_one_card() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-a", "implementer", 5).await;
    add_agent(pool, "ag-b", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;

    assert!(
        pull(pool, "t-1").await.is_some(),
        "first pull takes the card"
    );
    assert!(
        pull(pool, "t-2").await.is_none(),
        "second pull finds it already owned"
    );

    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id='i-1'")
        .fetch_one(pool)
        .await
        .expect("count");
    assert_eq!(n, 1, "exactly one task exists for the card");
}

/// A card with an UNFINISHED blocker is not pullable at any stage (the F7
/// refuse-run guard, reused verbatim), and becomes pullable once the blocker's
/// latest generation drains with a `done`.
#[tokio::test]
async fn blocked_card_is_not_pullable_until_its_blocker_finishes() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-impl", "implementer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-dep", "col-impl", 0).await;
    // The blocker is an issue but NOT a card in the gated column, so the only
    // thing this test can possibly pull is the dependent. Otherwise the blocker
    // itself becomes pullable the moment it finishes and wins on the id
    // tie-break, which would mask the dependent staying blocked.
    add_issue(pool, "i-blocker", 0).await;
    sqlx::query(
        "INSERT INTO card_dependency \
         (workspace_id, dependent_issue_id, blocker_issue_id, created_at, link_type) \
         VALUES ('ws-1','i-dep','i-blocker',0,'blocked_by')",
    )
    .execute(pool)
    .await
    .expect("dependency");
    // The blocker is mid-run, so the dependent is blocked. The blocker itself is
    // excluded by the one-owner guard, so nothing at all is pullable.
    add_task(pool, "t-blk", "ag-impl", "i-blocker", "running", 1).await;

    assert!(
        pull(pool, "t-1").await.is_none(),
        "a blocked dependent must not run"
    );

    // Finish the blocker: its latest generation drains with a `done`.
    sqlx::query("UPDATE agent_task_queue SET status='done' WHERE id='t-blk'")
        .execute(pool)
        .await
        .expect("finish blocker");
    let got = pull(pool, "t-1").await.expect("the dependent unblocks");
    assert_eq!(got.issue_id, "i-dep");
}

/// Per-agent capacity survives: an agent at `max_concurrent_tasks` pulls no more.
#[tokio::test]
async fn agent_at_capacity_pulls_nothing_further() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-impl", "implementer", 1).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_card(pool, "i-1", "col-impl", 0).await;
    add_card(pool, "i-2", "col-impl", 0).await;
    add_task(pool, "t-busy", "ag-impl", "i-1", "running", 1).await;

    assert!(
        pull(pool, "t-2").await.is_none(),
        "the only agent is at capacity"
    );
}

/// Ordering: urgent work first, then PULL FROM THE RIGHT (latest stage first),
/// so the pipeline drains rather than piling up mid-flight.
#[tokio::test]
async fn pull_takes_priority_first_then_the_latest_stage() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-both", "implementer,reviewer", 5).await;
    add_column(pool, "col-impl", 1, Some("implementer"), None, false).await;
    add_column(pool, "col-rev", 2, Some("reviewer"), None, false).await;
    // Equal priority: the LATER column wins.
    add_card(pool, "i-impl", "col-impl", 0).await;
    add_card(pool, "i-rev", "col-rev", 0).await;

    let first = pull(pool, "t-1").await.expect("pulls");
    assert_eq!(
        first.issue_id, "i-rev",
        "drain the end of the pipeline first"
    );

    // Higher priority beats stage position.
    add_card(pool, "i-urgent", "col-impl", 3).await;
    let second = pull(pool, "t-2").await.expect("pulls");
    assert_eq!(second.issue_id, "i-urgent", "priority outranks stage order");
}

/// The pulled row carries the fields the runner needs: the agent's OWN provider
/// as `agent_kind` (a codex reviewer must invoke codex, whatever the card was
/// created with) and a fresh generation per stage.
#[tokio::test]
async fn pulled_row_carries_the_agents_provider_and_a_fresh_generation() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-codex','ws-1','d-2','codex','local')",
    )
    .execute(pool)
    .await
    .expect("codex runtime");
    add_agent(pool, "ag-rev", "reviewer", 5).await;
    // `agent.provider` is the authority, not the runtime's. A runtime is a
    // DAEMON: `agent create --provider codex` records codex on the AGENT and
    // still binds it to the single `default` runtime, whose provider column
    // reads `claude` on every install.
    sqlx::query("UPDATE agent SET runtime_id='rt-codex', provider='codex' WHERE id='ag-rev'")
        .execute(pool)
        .await
        .expect("repoint agent at codex");
    add_column(pool, "col-rev", 2, Some("reviewer"), None, false).await;
    add_card(pool, "i-1", "col-rev", 0).await;
    // A prior stage ran under claude at generation 1.
    add_task(pool, "t-prior", "ag-rev", "i-1", "done", 1).await;

    let got = PullService::pull_for_runtime(
        pool,
        "rt-codex",
        &SeqIdGen::new(&["t-rev"]),
        &FixedClock(NOW_MS),
    )
    .await
    .expect("pull runs")
    .expect("codex reviewer pulls");
    assert_eq!(got.generation, 2, "each stage mints its own generation");

    let (kind, created): (String, i64) =
        sqlx::query_as("SELECT agent_kind, created_at FROM agent_task_queue WHERE id='t-rev'")
            .fetch_one(pool)
            .await
            .expect("read the pulled row");
    assert_eq!(kind, "codex", "the run uses the PULLING agent's provider");
    assert_eq!(created, NOW_MS);
}

/// REGRESSION: the run's provider comes from `agent.provider`, NOT from the
/// runtime's.
///
/// A runtime is a DAEMON, not a provider binding. `ainb hangar agent create
/// --provider codex` records `codex` on the AGENT and still binds it to the one
/// `default` runtime, whose own `provider` column reads `claude` on every
/// install. Keying the pulled row on the runtime therefore dispatched EVERY
/// agent as claude while the roster looked correctly mixed, which is exactly
/// what the first live pipeline run surfaced.
#[tokio::test]
async fn agent_provider_beats_the_runtime_provider() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_agent(pool, "ag-rev", "reviewer", 5).await;
    // The shipped shape: a codex agent on the shared `default` runtime, which
    // advertises `claude`.
    sqlx::query("UPDATE agent SET provider='codex' WHERE id='ag-rev'")
        .execute(pool)
        .await
        .expect("mark the agent codex");
    add_column(pool, "col-rev", 2, Some("reviewer"), None, false).await;
    add_card(pool, "i-1", "col-rev", 0).await;

    let got = pull(pool, "t-1").await.expect("reviewer pulls");
    assert_eq!(got.runtime_id, "rt-1", "still the shared default runtime");
    let kind: String = sqlx::query_scalar("SELECT agent_kind FROM agent_task_queue WHERE id='t-1'")
        .fetch_one(pool)
        .await
        .expect("read agent_kind");
    assert_eq!(
        kind, "codex",
        "the run must invoke the AGENT's provider, even though its runtime says claude"
    );
}

// ---------------------------------------------------------------------------
// Mutation targets: the exact substrings the proofs delete from PULL_SQL.
// Kept as consts so a reworded predicate breaks the proofs loudly (via the
// `contains` assertion in `pull_mutant`) rather than silently voiding them.
// ---------------------------------------------------------------------------

/// Invariant (a): the agent must hold the column's `services_role`.
const ROLE_GATE_CLAUSE: &str = "\
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
       ) ";

/// Invariant (b): the column must be under its `wip_limit`.
const WIP_LIMIT_CLAUSE: &str = "\
   AND ( col.wip_limit IS NULL OR ( \
        SELECT COUNT(DISTINCT w.issue_id) FROM board_card AS w \
          JOIN agent_task_queue AS wq ON wq.issue_id = w.issue_id \
         WHERE w.column_id = col.id \
           AND wq.status IN ('queued','dispatched','running') \
       ) < col.wip_limit ) ";

/// Invariant (c): a prior agent on the card is excluded from a flagged stage.
const PRIOR_AGENT_CLAUSE: &str = "\
   AND ( col.excludes_prior_agent = 0 OR NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS p \
         WHERE p.issue_id = bc.issue_id \
           AND p.agent_id = a.id \
           AND p.status = 'done' \
       ) ) ";

/// The supporting one-owner-per-card guard.
const ONE_OWNER_CLAUSE: &str = "\
   AND NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS o \
         WHERE o.issue_id = bc.issue_id \
           AND o.status IN ('queued','dispatched','running') \
       ) ";
