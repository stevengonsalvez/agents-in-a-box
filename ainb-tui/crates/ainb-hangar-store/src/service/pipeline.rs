//! The DEFAULT six-stage pull pipeline, and the enqueue that feeds it.
//!
//! ```text
//!  Backlog  ->  Triage  ->  Implement  ->  Review  ->   QA    ->  Done
//!    (-)       triager    implementer     reviewer    tester      (-)
//!   wip -        wip -       wip 2         wip 3       wip 1     wip -
//!                                        excl. prior  excl. prior
//! ```
//!
//! Backlog and Done carry NO `services_role`, so they are not pull queues: work
//! parks there and is moved by a human or by the stage advance. The four middle
//! stages are role-gated, and a card walks them one column at a time, one owner
//! per stage.
//!
//! # Why every column here has `fsm_state = NULL`
//!
//! This is the load-bearing subtlety, and getting it wrong silently destroys the
//! pipeline.
//!
//! Hangar already has a state-driven auto-move hook,
//! [`BoardRepo::auto_move_on_state`](crate::repo::board::BoardRepo::auto_move_on_state),
//! which the daemon calls on EVERY task FSM transition and which moves a card to
//! the column whose `fsm_state` equals the new task state. That hook and the
//! stage advance are two different concepts sharing one board:
//!
//!   * `auto_move_on_state` is STATE driven: "a task started, show the card under
//!     In Progress". It can only express as many stages as there are task states.
//!   * [`PullService::advance_after_stage`](crate::service::pull::PullService::advance_after_stage)
//!     is SEQUENCE driven: "this stage finished, step one column right".
//!
//! If a pipeline column carried `fsm_state = 'done'` the state hook would fire
//! the moment the FIRST stage completed and jump the card straight to that
//! column, skipping Review and QA entirely. The pipeline would appear to work
//! and would in fact never review anything.
//!
//! Leaving `fsm_state` NULL makes the state hook INERT on these columns (its
//! query matches on `col.fsm_state = new_state`, and NULL never equals anything)
//! while `auto_move = 1` keeps the operator's existing per-column and
//! per-board kill-switch meaningful, because that is exactly what
//! `advance_after_stage` consults. So the two mechanisms coexist on one board
//! rather than fighting, and no parallel "stage" concept is introduced.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::{Row, SqlitePool};

/// The name of the board the default pipeline is provisioned onto.
pub const DEFAULT_PIPELINE_BOARD: &str = "Pipeline";

/// One stage of the default pipeline: `(name, services_role, wip_limit,
/// excludes_prior_agent)`.
///
/// Review and QA both exclude a prior agent: a stage whose whole purpose is to
/// CHECK the work is worthless when performed by whoever produced it. The
/// consequence is deliberate and is the documented edge case, with only one
/// eligible agent the card WAITS rather than being self-reviewed.
pub const DEFAULT_STAGES: &[(&str, Option<&str>, Option<i64>, bool)] = &[
    ("Backlog", None, None, false),
    ("Triage", Some("triager"), None, false),
    ("Implement", Some("implementer"), Some(2), false),
    ("Review", Some("reviewer"), Some(3), true),
    ("QA", Some("tester"), Some(1), true),
    ("Done", None, None, false),
];

/// Why an enqueue could not place a card.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The workspace has no board carrying a role-gated pipeline.
    #[error("workspace has no role-gated pipeline; provision one first")]
    NoPipeline,
    /// An underlying store failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Provisioning + enqueue over the role-gated board pipeline.
pub struct PipelineService;

impl PipelineService {
    /// Provision the default six-stage pipeline for `workspace`, returning the
    /// board id.
    ///
    /// IDEMPOTENT and NON-DESTRUCTIVE: if a board named
    /// [`DEFAULT_PIPELINE_BOARD`] already exists it is returned untouched, so
    /// re-running this never rewrites an operator's tuned WIP limits or renamed
    /// stages. It creates a NEW board rather than retrofitting an existing one,
    /// so an operator's current Kanban keeps working exactly as it did.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn provision_default(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
    ) -> Result<String, sqlx::Error> {
        if let Some(existing) = sqlx::query_scalar::<_, String>(
            "SELECT id FROM board WHERE workspace_id = ?1 AND name = ?2",
        )
        .bind(workspace.as_str())
        .bind(DEFAULT_PIPELINE_BOARD)
        .fetch_optional(pool)
        .await?
        {
            return Ok(existing);
        }

        let board_id = idgen.new_ulid();
        let now = clock.now_ms();
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO board (id, workspace_id, name, auto_move, created_at) \
             VALUES (?1, ?2, ?3, 1, ?4)",
        )
        .bind(&board_id)
        .bind(workspace.as_str())
        .bind(DEFAULT_PIPELINE_BOARD)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for (ord, (name, role, wip, excludes_prior)) in DEFAULT_STAGES.iter().enumerate() {
            sqlx::query(
                "INSERT INTO board_column \
                 (id, board_id, ord, name, fsm_state, auto_move, \
                  services_role, wip_limit, excludes_prior_agent) \
                 VALUES (?1, ?2, ?3, ?4, NULL, 1, ?5, ?6, ?7)",
            )
            .bind(idgen.new_ulid())
            .bind(&board_id)
            .bind(i64::try_from(ord).unwrap_or(i64::MAX))
            .bind(name)
            .bind(*role)
            .bind(*wip)
            .bind(i64::from(*excludes_prior))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(board_id)
    }

    /// ENQUEUE `issue_id` into the pipeline: place its card in the FIRST
    /// role-gated column of `workspace`'s pipeline board.
    ///
    /// This is what replaces broadcast. Where the old fan-out wrote one task per
    /// squad member, this writes ONE card placement and no tasks at all, then
    /// lets [`PullService`](crate::service::pull::PullService) hand the card to
    /// exactly one eligible agent.
    ///
    /// Returns `(board_id, column_id)`. Re-enqueueing a card already on the board
    /// MOVES it back to the first role-gated stage, which is the natural
    /// "re-run this from the top" semantic.
    ///
    /// # Errors
    ///
    /// [`PipelineError::NoPipeline`] when the workspace has no role-gated column
    /// anywhere, or [`PipelineError::Db`] on a store fault.
    pub async fn enqueue(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        clock: &dyn HangarClock,
    ) -> Result<(String, String), PipelineError> {
        // The first role-gated column of the workspace, preferring the default
        // pipeline board when several boards carry one, so an operator's older
        // ad-hoc board cannot silently capture squad dispatches.
        let target = sqlx::query(
            "SELECT col.id AS column_id, col.board_id AS board_id \
               FROM board_column AS col \
               JOIN board AS bd ON bd.id = col.board_id \
              WHERE bd.workspace_id = ?1 \
                AND col.services_role IS NOT NULL \
              ORDER BY (bd.name <> ?2), bd.name, bd.id, col.ord \
              LIMIT 1",
        )
        .bind(workspace.as_str())
        .bind(DEFAULT_PIPELINE_BOARD)
        .fetch_optional(pool)
        .await?
        .ok_or(PipelineError::NoPipeline)?;

        let board_id: String = target.try_get("board_id")?;
        let column_id: String = target.try_get("column_id")?;

        sqlx::query(
            "INSERT INTO board_card (board_id, issue_id, column_id, added_at, ord) \
             VALUES (?1, ?2, ?3, ?4, \
                     (SELECT COALESCE(MAX(o.ord) + 1, 0) FROM board_card AS o \
                       WHERE o.column_id = ?3)) \
             ON CONFLICT (board_id, issue_id) DO UPDATE SET column_id = excluded.column_id",
        )
        .bind(&board_id)
        .bind(issue_id)
        .bind(&column_id)
        .bind(clock.now_ms())
        .execute(pool)
        .await?;

        Ok((board_id, column_id))
    }
}
