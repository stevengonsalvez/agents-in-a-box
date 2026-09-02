//! [`PullService::advance_after_stage`] contract tests: the HANDOFF that makes a
//! pipeline a pipeline.
//!
//! # Why this file exists
//!
//! The advance had NO automated coverage. It is referenced from exactly one
//! place (`board.rs`'s `advance_pipeline_stage`) and from no test in the
//! workspace, and its only proof was `live_pipeline_walks_four_stages`, which
//! sits behind the `live-e2e` cargo feature. That feature has zero references in
//! `.github/workflows/`, so it never runs in CI, and it cannot: it needs real
//! authenticated provider CLIs. The gate is inherently local, so the invariants
//! are pinned here instead, against a real ephemeral `SQLite` database.
//!
//! What the statement must guarantee:
//!   * it steps EXACTLY ONE column, never jumping to the end of the board,
//!   * it refuses at the last column rather than running off the end,
//!   * it refuses while the card still holds an active task,
//!   * it refuses when either `auto_move` kill-switch is off,
//!   * it refuses on a column that is not role-gated,
//!   * a board deleted mid-run is a clean no-op, not an error.
//!
//! The refusals matter as much as the move: every one of them is a case where
//! the card must stay exactly where it is, and a statement that moved it anyway
//! would strand work or skip a review stage silently.
//!
//! # Every refusal carries a MUTATION PROOF
//!
//! A test that only runs the real statement proves the card STAYED PUT. It
//! cannot prove that the guard under test is what kept it there: delete the
//! guard and the test may well keep passing because another clause happened to
//! refuse the same row. So each refusal is paired with a `mutation_proof_*` test
//! that deletes exactly that guard from [`ADVANCE_SQL`] and asserts the card NOW
//! MOVES. The mutant is the code without that one guard, which is the only way
//! to show these tests are not vacuous, given the guards were already in place
//! when this file was written.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::Store;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::service::pull::{ADVANCE_SQL, PullService, STAGES_REMAIN_SQL, current_gated_column};
use sqlx::SqlitePool;

const NOW_MS: i64 = 1_700_000_500_000;

/// Deterministic id generator over a seeded sequence.
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

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    (dir, store)
}

/// One workspace, one runtime, one board with the master `auto_move` on.
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
        "INSERT INTO board (id, workspace_id, name, auto_move, created_at) \
         VALUES ('b-1','ws-1','Pipeline',1,0)",
    )
    .execute(pool)
    .await
    .expect("board");
    sqlx::query(
        "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
         VALUES ('sq-1','ws-1','team1','agent','ag-1',0)",
    )
    .execute(pool)
    .await
    .expect("squad");
    // A bare agent for the hand-inserted task rows to reference. It joins NO
    // squad, so it holds no role and can never itself pull a card.
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES ('ag-1','ws-1','ag-1','rt-1','workspace','u-1',5)",
    )
    .execute(pool)
    .await
    .expect("bare agent");
}

async fn add_agent(pool: &SqlitePool, id: &str, roles: &str) {
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?1,'ws-1',?1,'rt-1','workspace','u-1',5)",
    )
    .bind(id)
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

/// Add a column. `role = None` leaves it ungated; `auto_move` is the per-column
/// kill-switch the advance consults.
async fn add_column(pool: &SqlitePool, id: &str, ord: i64, role: Option<&str>, auto_move: bool) {
    sqlx::query(
        "INSERT INTO board_column \
         (id, board_id, ord, name, fsm_state, auto_move, services_role, wip_limit, \
          excludes_prior_agent) \
         VALUES (?1,'b-1',?2,?1,NULL,?3,?4,NULL,0)",
    )
    .bind(id)
    .bind(ord)
    .bind(i64::from(auto_move))
    .bind(role)
    .execute(pool)
    .await
    .expect("column");
}

async fn add_card(pool: &SqlitePool, issue_id: &str, column_id: Option<&str>) {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, priority) \
         VALUES (?1,'ws-1',?1,'open','member','u-1',0,0)",
    )
    .bind(issue_id)
    .execute(pool)
    .await
    .expect("issue");
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

/// Insert a task on the card directly, for setting up prior / concurrent state.
async fn add_task(pool: &SqlitePool, id: &str, issue_id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, generation) \
         VALUES (?1,'ws-1','rt-1','ag-1',?2,?3,0,1)",
    )
    .bind(id)
    .bind(issue_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("task");
}

/// Run a MUTANT of the advance: [`ADVANCE_SQL`] with `guard` deleted.
///
/// Panics if `guard` is not found verbatim, so a reworded clause breaks the
/// proof loudly instead of silently turning it into a no-op that tests nothing.
async fn advance_mutant(pool: &SqlitePool, issue_id: &str, guard: &str) {
    assert!(
        ADVANCE_SQL.contains(guard),
        "mutation target not found verbatim in ADVANCE_SQL; the proof would test nothing:\n{guard}"
    );
    let mutated = ADVANCE_SQL.replace(guard, " ");
    sqlx::query(&mutated).bind(issue_id).fetch_all(pool).await.expect("mutant runs");
}

/// Where the card sits now.
async fn column_of(pool: &SqlitePool, issue_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT column_id FROM board_card WHERE issue_id = ?1")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("card exists")
}

/// A four-stage board: Backlog (ungated), Triage, Implement, Review, Done
/// (ungated). Mirrors the shipped `DEFAULT_STAGES` shape without depending on it.
async fn seed_pipeline(pool: &SqlitePool) {
    seed_world(pool).await;
    add_column(pool, "col-backlog", 0, None, true).await;
    add_column(pool, "col-triage", 1, Some("triager"), true).await;
    add_column(pool, "col-impl", 2, Some("implementer"), true).await;
    add_column(pool, "col-review", 3, Some("reviewer"), true).await;
    add_column(pool, "col-done", 4, None, true).await;
}

// ---------------------------------------------------------------------------
// The move itself
// ---------------------------------------------------------------------------

/// THE REGRESSION: the advance steps EXACTLY ONE column, never jumping to the
/// end. A statement that landed the card in Done on the first completion would
/// look like a working pipeline and would in fact never review anything.
#[tokio::test]
async fn advance_steps_exactly_one_column() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-triage")).await;
    add_task(pool, "t-1", "i-1", "done").await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert_eq!(moved, vec![("b-1".to_string(), "col-impl".to_string())]);
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-impl"),
        "Triage hands to Implement, not to Review and not to Done"
    );

    // Each further advance is one more step, so the card WALKS the board.
    PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-review"));
    PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-done"),
        "the last gated stage hands to the ungated parking column"
    );
}

/// The advanced card APPENDS to its target column rather than colliding with the
/// cards already there, matching a manual `card_move`.
#[tokio::test]
async fn advance_appends_to_the_end_of_the_target_column() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-sitting", Some("col-impl")).await;
    sqlx::query("UPDATE board_card SET ord = 7 WHERE issue_id='i-sitting'")
        .execute(pool)
        .await
        .expect("seed an occupied target column");
    add_card(pool, "i-1", Some("col-triage")).await;

    PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");

    let ord: i64 = sqlx::query_scalar("SELECT ord FROM board_card WHERE issue_id='i-1'")
        .fetch_one(pool)
        .await
        .expect("read ord");
    assert_eq!(
        ord, 8,
        "the card lands after the cards already in the column"
    );
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// At the LAST column of the board there is nowhere to go, so the card stays put
/// and the statement reports nothing moved. Without the "a column to the right
/// exists" guard the `column_id` sub-select yields NULL and the card would be
/// silently knocked OFF the board into a NULL column.
#[tokio::test]
async fn advance_at_the_last_column_does_not_run_off_the_end() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    // A single gated column IS the last column: no Done to park in.
    add_column(pool, "col-only", 0, Some("implementer"), true).await;
    add_card(pool, "i-1", Some("col-only")).await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "there is no column to the right");
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-only"),
        "the card must stay put, NOT be knocked into a NULL column"
    );
}

/// A card that still holds an ACTIVE task does not advance. This is what makes a
/// deliberate `--redundant` fan-out advance only once ALL its runs have drained,
/// rather than on the first one home.
#[tokio::test]
async fn advance_refuses_while_the_card_still_has_an_active_task() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-triage")).await;
    add_task(pool, "t-done", "i-1", "done").await;
    add_task(pool, "t-live", "i-1", "running").await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "a sibling is still running");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-triage"));

    // Drain it, and the card advances.
    sqlx::query("UPDATE agent_task_queue SET status='done' WHERE id='t-live'")
        .execute(pool)
        .await
        .expect("drain");
    PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-impl"));
}

/// The board's MASTER `auto_move` kill-switch freezes the advance: the operator
/// wants to move cards by hand.
#[tokio::test]
async fn advance_respects_the_board_auto_move_kill_switch() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-triage")).await;
    sqlx::query("UPDATE board SET auto_move = 0 WHERE id='b-1'")
        .execute(pool)
        .await
        .expect("switch the board off");

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "the board's auto_move is off");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-triage"));
}

/// The per-COLUMN `auto_move` kill-switch does the same for one stage alone: a
/// stage an operator wants to sign off by hand holds its cards.
#[tokio::test]
async fn advance_respects_the_column_auto_move_kill_switch() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_column(pool, "col-triage", 1, Some("triager"), false).await;
    add_column(pool, "col-impl", 2, Some("implementer"), true).await;
    add_card(pool, "i-1", Some("col-triage")).await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "this column's auto_move is off");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-triage"));
}

/// A card parked in a column that is NOT role-gated (Backlog, Done, or any
/// column on a board predating migration 0074) is not in the pipeline at all, so
/// a finished task on it must never drag it rightwards.
#[tokio::test]
async fn advance_refuses_from_an_ungated_column() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-backlog")).await;
    add_task(pool, "t-1", "i-1", "done").await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "Backlog is not a pipeline stage");
    assert_eq!(column_of(pool, "i-1").await.as_deref(), Some("col-backlog"));
}

/// A card with NO column at all (`board_card.column_id IS NULL`) has no stage to
/// advance from, and the NULL must not be treated as "before the first column".
#[tokio::test]
async fn advance_refuses_a_card_with_no_column() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", None).await;

    let moved = PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    assert!(moved.is_empty(), "a card with no column has no next column");
    assert_eq!(column_of(pool, "i-1").await, None);
}

/// A board DELETED while its card's stage was running is a clean no-op. The
/// advance is a best-effort hook fired after the task's terminal state has
/// already committed, so it must never error the claim loop over a board that
/// vanished underneath it.
#[tokio::test]
async fn advance_survives_a_board_deleted_mid_run() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-triage")).await;
    add_task(pool, "t-1", "i-1", "done").await;

    // Tear the board down in FK-safe order, exactly as a board delete does.
    for stmt in [
        "DELETE FROM board_card WHERE board_id='b-1'",
        "DELETE FROM board_column WHERE board_id='b-1'",
        "DELETE FROM board WHERE id='b-1'",
    ] {
        sqlx::query(stmt).execute(pool).await.expect("tear the board down");
    }

    let moved = PullService::advance_after_stage(pool, "i-1")
        .await
        .expect("advance must not error");
    assert!(moved.is_empty(), "there is no card left to move");
}

/// An issue that has never been on a board at all is a no-op, not an error: the
/// hook fires for every terminal task, including plain non-pipeline runs.
#[tokio::test]
async fn advance_on_an_issue_with_no_card_is_a_no_op() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;

    let moved = PullService::advance_after_stage(pool, "i-nowhere")
        .await
        .expect("advance must not error");
    assert!(moved.is_empty());
}

// ---------------------------------------------------------------------------
// Mutation proofs: each refusal, with its guard deleted
// ---------------------------------------------------------------------------

/// Without the "a column to the right exists" guard, the last column's card is
/// knocked clean OFF the board: both sub-selects yield NULL, so `column_id`
/// becomes NULL and the card vanishes from every column.
#[tokio::test]
async fn mutation_proof_without_the_last_column_guard_the_card_falls_off_the_board() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_world(pool).await;
    add_column(pool, "col-only", 0, Some("implementer"), true).await;
    add_card(pool, "i-1", Some("col-only")).await;

    advance_mutant(pool, "i-1", LAST_COLUMN_GUARD).await;
    assert_eq!(
        column_of(pool, "i-1").await,
        None,
        "the mutant moved the card into a NULL column, off the board entirely"
    );
}

/// Without the gating guard, a `done` task drags a card out of BACKLOG, which is
/// not a pipeline stage at all, and also ignores both `auto_move` kill-switches.
#[tokio::test]
async fn mutation_proof_without_the_gate_guard_an_ungated_card_advances() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-backlog")).await;
    sqlx::query("UPDATE board SET auto_move = 0 WHERE id='b-1'")
        .execute(pool)
        .await
        .expect("switch the board off too");

    advance_mutant(pool, "i-1", GATE_GUARD).await;
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-triage"),
        "the mutant advanced an ungated card on a board with auto_move OFF"
    );
}

/// Without the active-task guard, the FIRST sibling home advances the card while
/// the rest of a `--redundant` cluster is still running.
#[tokio::test]
async fn mutation_proof_without_the_active_guard_a_running_card_advances() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-triage")).await;
    add_task(pool, "t-done", "i-1", "done").await;
    add_task(pool, "t-live", "i-1", "running").await;

    advance_mutant(pool, "i-1", ACTIVE_TASK_GUARD).await;
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-impl"),
        "the mutant handed the card to the next stage mid-run"
    );
}

// ---------------------------------------------------------------------------
// Mutation targets: the exact substrings the proofs delete from ADVANCE_SQL.
// ---------------------------------------------------------------------------

/// The card must sit in a ROLE-GATED column with both `auto_move` switches on.
const GATE_GUARD: &str = "\
   AND EXISTS ( \
        SELECT 1 FROM board_column AS cur \
          JOIN board AS bd ON bd.id = cur.board_id \
         WHERE cur.id = board_card.column_id \
           AND cur.services_role IS NOT NULL \
           AND cur.auto_move = 1 \
           AND bd.auto_move = 1 \
       ) ";

/// The card must hold NO active task.
const ACTIVE_TASK_GUARD: &str = "\
   AND NOT EXISTS ( \
        SELECT 1 FROM agent_task_queue AS t \
         WHERE t.issue_id = board_card.issue_id \
           AND t.status IN ('queued','dispatched','running') \
       ) ";

/// There must BE a column to the right.
const LAST_COLUMN_GUARD: &str = "\
   AND EXISTS ( \
        SELECT 1 FROM board_column AS n3 \
         WHERE n3.board_id = board_card.board_id \
           AND n3.ord > (SELECT cur3.ord FROM board_column AS cur3 \
                          WHERE cur3.id = board_card.column_id) \
       ) ";

// ---------------------------------------------------------------------------
// The advance and the pull, together
// ---------------------------------------------------------------------------

/// The full handoff: an implementer finishes, the card advances, and a DIFFERENT
/// role takes the next stage. This is the loop the daemon actually runs, minus
/// the provider CLIs the `live-e2e` gate needs.
#[tokio::test]
async fn a_card_walks_the_pipeline_one_stage_per_role() {
    let (_d, s) = store().await;
    let pool = s.pool();
    seed_pipeline(pool).await;
    add_agent(pool, "ag-triage", "triager").await;
    add_agent(pool, "ag-impl", "implementer").await;
    add_agent(pool, "ag-rev", "reviewer").await;
    add_card(pool, "i-1", Some("col-triage")).await;

    let mut walked = Vec::new();
    for (task_id, column) in [
        ("t-1", "col-triage"),
        ("t-2", "col-impl"),
        ("t-3", "col-review"),
    ] {
        let pulled = PullService::pull_for_runtime(
            pool,
            "rt-1",
            &SeqIdGen::new(&[task_id]),
            &FixedClock(NOW_MS),
        )
        .await
        .expect("pull runs")
        .expect("a card is pullable at this stage");
        assert_eq!(pulled.column_id, column);
        walked.push((pulled.agent_id.clone(), pulled.generation));
        sqlx::query("UPDATE agent_task_queue SET status='done' WHERE id=?1")
            .bind(task_id)
            .execute(pool)
            .await
            .expect("finish the stage");
        PullService::advance_after_stage(pool, "i-1").await.expect("advance runs");
    }

    assert_eq!(
        walked,
        vec![
            ("ag-triage".to_string(), 1),
            ("ag-impl".to_string(), 2),
            ("ag-rev".to_string(), 3),
        ],
        "one owner per stage, a different role each time, one generation each"
    );
    assert_eq!(
        column_of(pool, "i-1").await.as_deref(),
        Some("col-done"),
        "the card ends parked in the ungated Done column"
    );
}

// ---------------------------------------------------------------------------
// Stages remaining (the issue-lifecycle gate)
// ---------------------------------------------------------------------------

/// A task that ran at a specific stage column (what a real pull records).
async fn add_stage_task(pool: &SqlitePool, id: &str, issue_id: &str, status: &str, column: &str) {
    add_stage_task_gen(pool, id, issue_id, status, Some(column), 1).await;
}

/// A task with an explicit stage column (or none, the push path's shape) and
/// generation.
async fn add_stage_task_gen(
    pool: &SqlitePool,
    id: &str,
    issue_id: &str,
    status: &str,
    column: Option<&str>,
    generation: i64,
) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, generation, \
          board_column_id) \
         VALUES (?1,'ws-1','rt-1','ag-1',?2,?3,0,?5,?4)",
    )
    .bind(id)
    .bind(issue_id)
    .bind(status)
    .bind(column)
    .bind(generation)
    .execute(pool)
    .await
    .expect("stage task");
}

/// Run a MUTANT of [`STAGES_REMAIN_SQL`] with `guard` replaced by
/// `replacement`, returning the mutant's verdict. Panics if `guard` is not
/// found verbatim, so a reworded clause breaks the proof loudly.
async fn stages_remain_mutant(pool: &SqlitePool, issue_id: &str, guard: &str, replacement: &str) -> bool {
    assert!(
        STAGES_REMAIN_SQL.contains(guard),
        "mutation target not found verbatim in STAGES_REMAIN_SQL; the proof would test nothing:\n{guard}"
    );
    let mutated = STAGES_REMAIN_SQL.replace(guard, replacement);
    let found: Option<i64> = sqlx::query_scalar(&mutated)
        .bind(issue_id)
        .fetch_optional(pool)
        .await
        .expect("mutant runs");
    found.is_some()
}

/// A later non-stage task on the issue (a push-path retry, a chat task) bumps
/// the issue-wide generation but must NOT un-finish a completed stage: the
/// current generation is the newest among STAGE tasks only.
#[tokio::test]
async fn stages_remain_ignores_generations_of_non_stage_tasks() {
    let (_dir, store) = store().await;
    let pool = store.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-review")).await;
    add_stage_task_gen(pool, "t-review", "i-1", "done", Some("col-review"), 1).await;
    // A headless push run later, generation 2, no stage column.
    add_stage_task_gen(pool, "t-push", "i-1", "done", None, 2).await;
    assert!(
        !PullService::stages_remain(pool, "i-1").await.unwrap(),
        "the Review stage stays finished across a later non-stage task"
    );
}

/// MUTATION PROOF: deleting the current-column clause makes the last gated
/// stage look finished the moment the card enters it (the regression this
/// predicate exists to prevent), and deleting the stage-row filter on the
/// generation sub-select lets a later push run un-finish a completed stage.
#[tokio::test]
async fn mutation_proofs_for_the_current_stage_clauses() {
    let (_dir, store) = store().await;
    let pool = store.pool();
    seed_pipeline(pool).await;
    add_card(pool, "i-1", Some("col-review")).await;
    add_stage_task(pool, "t-impl", "i-1", "done", "col-impl").await;
    assert!(PullService::stages_remain(pool, "i-1").await.unwrap());
    assert!(
        !stages_remain_mutant(pool, "i-1", "n.id = cur.id AND NOT EXISTS", "0 AND NOT EXISTS").await,
        "without the current-column clause the unrun last stage reads as finished"
    );

    add_card(pool, "i-2", Some("col-review")).await;
    add_stage_task_gen(pool, "t2-review", "i-2", "done", Some("col-review"), 1).await;
    add_stage_task_gen(pool, "t2-push", "i-2", "done", None, 2).await;
    assert!(!PullService::stages_remain(pool, "i-2").await.unwrap());
    assert!(
        stages_remain_mutant(pool, "i-2", "AND g.board_column_id IS NOT NULL", " ").await,
        "without the stage-row filter a later push run un-finishes the stage"
    );
}

/// A push-path Run on a card in a gated column is that stage's run: the helper
/// the run path stamps the task with names the card's current gated column,
/// and none for an ungated column or a card of another workspace.
#[tokio::test]
async fn current_gated_column_names_the_cards_stage() {
    let (_dir, store) = store().await;
    let pool = store.pool();
    seed_pipeline(pool).await;
    let ws = WorkspaceId::from_str("ws-1").unwrap();
    add_card(pool, "i-impl", Some("col-impl")).await;
    assert_eq!(
        current_gated_column(pool, &ws, "i-impl").await.unwrap().as_deref(),
        Some("col-impl")
    );
    add_card(pool, "i-backlog", Some("col-backlog")).await;
    assert_eq!(current_gated_column(pool, &ws, "i-backlog").await.unwrap(), None);
    let other = WorkspaceId::from_str("ws-other").unwrap();
    assert_eq!(current_gated_column(pool, &other, "i-impl").await.unwrap(), None);
}
