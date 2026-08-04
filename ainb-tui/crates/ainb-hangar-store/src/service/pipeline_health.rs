//! "Why is nothing moving?" answered in one strip.
//!
//! A role-gated pull pipeline ([`super::pipeline`]) stalls SILENTLY. Nothing
//! errors, nothing retries, no task row is ever written: a card whose stage
//! demands `reviewer` and whose workspace has no reviewer simply sits there,
//! and finding that out today means reading the board, the squad roster and the
//! agent list in three separate places. This module folds those three reads into
//! one snapshot so both the CLI (`ainb hangar pipeline show`) and the Boards
//! screen render the SAME four facts from the SAME query.
//!
//! ```text
//!  ● daemon ok   ● roles covered   ○ wip 1/2 Implement   ● 0 stuck
//! ```
//!
//! Three of the four lights are computed here; `daemon` is deliberately NOT,
//! because each caller already has a liveness notion (the CLI reads the pid
//! file, the plugin its socket link) and inventing a second one here would let
//! them disagree.
//!
//! # Everything is derived at query time
//!
//! There is no health table, no cache and no background job. The snapshot is a
//! read of live rows, so it cannot go stale and there is nothing to invalidate.
//! The role-matching predicate is the SAME comma-separated, case-insensitive
//! token match the pull statement gates on
//! ([`super::pull`]), which is what makes a green "roles covered" light mean
//! "the pull WILL fire" rather than "a role string looked similar".

use ainb_hangar_core::ids::WorkspaceId;
use sqlx::{Row, SqlitePool};

/// How long a card may sit in a role-gated stage, with no active task, before it
/// counts as STUCK.
///
/// Fifteen minutes is long enough that an ordinary hand-off between stages never
/// trips it and short enough that an operator notices within a coffee break.
//
// ponytail: fixed on purpose. A stall threshold is the kind of knob that gets a
// config key, a migration and a settings row before anyone has ever wanted a
// second value. If someone actually asks for a different threshold, move it into
// the `daemon_config` registry (`DAEMON_CONFIG_REGISTRY`), which already gives a
// knob CLI get/set/list and a live daemon reload for free.
pub const STUCK_AFTER_MS: i64 = 15 * 60 * 1000;

/// The task statuses that count as an agent (or a card) being BUSY.
const ACTIVE_STATUSES: &str = "'queued','dispatched','running'";

/// One stage's health, as read off the live board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageHealth {
    /// The column's stable id.
    pub column_id: String,
    /// The column's display name (`Review`).
    pub name: String,
    /// The column's left-to-right position.
    pub ord: i64,
    /// The role that gates pulling from this stage, or `None` when the stage is
    /// not a pull queue at all (Backlog / Done).
    pub services_role: Option<String>,
    /// The stage's WIP cap, or `None` for unlimited.
    pub wip_limit: Option<i64>,
    /// Cards in this stage holding an active task right now (what `wip_limit` is
    /// compared against by the pull statement).
    pub wip_active: i64,
    /// Non-archived agents in the workspace that HOLD this stage's role.
    /// Zero is the silent-stall condition: cards here can never be pulled.
    pub role_agents: i64,
    /// Of those, how many are under their own `max_concurrent_tasks` — i.e. could
    /// pull a card right now. Zero with `role_agents > 0` is ordinary busyness,
    /// not a misconfiguration.
    pub role_agents_free: i64,
    /// Cards currently sitting in this stage.
    pub cards: i64,
    /// Cards that have sat here with no active task for longer than
    /// [`STUCK_AFTER_MS`] (blocked cards excluded — those are waiting, not stuck).
    pub stuck: i64,
}

impl StageHealth {
    /// Whether this stage is a pull queue (has a role gate) at all.
    #[must_use]
    pub const fn is_gated(&self) -> bool {
        self.services_role.is_some()
    }

    /// The stage nobody can serve: role-gated, and NO agent in the workspace
    /// holds the role. This is the light that matters — cards park here forever
    /// and nothing anywhere reports an error.
    #[must_use]
    pub const fn role_uncovered(&self) -> bool {
        self.is_gated() && self.role_agents == 0
    }

    /// Whether the stage is AT its WIP cap (no headroom to pull another card).
    #[must_use]
    pub fn wip_saturated(&self) -> bool {
        self.wip_limit.is_some_and(|limit| self.wip_active >= limit)
    }
}

/// A whole pipeline board's health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineHealth {
    /// Every column of the board, in board order.
    pub stages: Vec<StageHealth>,
}

impl PipelineHealth {
    /// The role-gated stages no agent can serve, in board order. Empty ⇒ the
    /// "roles covered" light is GREEN.
    #[must_use]
    pub fn uncovered(&self) -> Vec<&StageHealth> {
        self.stages.iter().filter(|s| s.role_uncovered()).collect()
    }

    /// The first stage sitting at its WIP cap, if any.
    #[must_use]
    pub fn saturated(&self) -> Option<&StageHealth> {
        self.stages.iter().find(|s| s.wip_saturated())
    }

    /// Total stuck cards across every gated stage.
    #[must_use]
    pub fn stuck(&self) -> i64 {
        self.stages.iter().map(|s| s.stuck).sum()
    }
}

/// Read one board's pipeline health, but ONLY when the board is a pipeline at
/// all: `Ok(None)` for a board with no role-gated column.
///
/// The full fold is not free. `role_agents_free` counts each candidate agent's
/// active tasks with a correlated subquery over `agent_task_queue`, once per
/// (column x agent), and the caller that matters here is
/// `boards_list`, which the Boards screen re-arms on every pushed daemon event
/// for EVERY board in the workspace. A plain kanban board can never produce a
/// non-empty light, so paying for that on one is pure waste.
///
/// The probe itself is served by `idx_board_column_pull` (0074), which is
/// partial on exactly this predicate.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if a query fails.
pub async fn snapshot_if_pipeline(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    board_id: &str,
    now_ms: i64,
) -> Result<Option<PipelineHealth>, sqlx::Error> {
    let gated: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM board_column col JOIN board bd ON bd.id = col.board_id \
          WHERE col.board_id = ?1 AND bd.workspace_id = ?2 \
            AND col.services_role IS NOT NULL LIMIT 1",
    )
    .bind(board_id)
    .bind(workspace.as_str())
    .fetch_optional(pool)
    .await?;
    if gated.is_none() {
        return Ok(None);
    }
    snapshot(pool, workspace, board_id, now_ms).await.map(Some)
}

/// Read one board's pipeline health.
///
/// `now_ms` is passed rather than read so the stuck light is deterministic under
/// test (the same reason every service here takes a
/// [`HangarClock`](ainb_hangar_core::clock::HangarClock)).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if a query fails.
pub async fn snapshot(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    board_id: &str,
    now_ms: i64,
) -> Result<PipelineHealth, sqlx::Error> {
    // One row per column. The two role counts share the pull statement's role
    // predicate VERBATIM (comma-separated token, case-insensitive, whitespace
    // stripped) so a green light and an actual pull cannot disagree; the second
    // adds the pull's per-agent concurrency cap on top.
    let sql = format!(
        "SELECT col.id AS id, col.name AS name, col.ord AS ord, \
                col.services_role AS role, col.wip_limit AS wip_limit, \
                (SELECT COUNT(*) FROM board_card bc WHERE bc.column_id = col.id) AS cards, \
                (SELECT COUNT(DISTINCT w.issue_id) FROM board_card w \
                   JOIN agent_task_queue wq ON wq.issue_id = w.issue_id \
                  WHERE w.column_id = col.id \
                    AND wq.status IN ({ACTIVE_STATUSES})) AS wip_active, \
                (SELECT COUNT(*) FROM agent a \
                  WHERE a.workspace_id = bd.workspace_id AND a.archived = 0 \
                    AND {ROLE_MATCH}) AS role_agents, \
                (SELECT COUNT(*) FROM agent a \
                  WHERE a.workspace_id = bd.workspace_id AND a.archived = 0 \
                    AND {ROLE_MATCH} \
                    AND (SELECT COUNT(*) FROM agent_task_queue r \
                          WHERE r.agent_id = a.id \
                            AND r.status IN ({ACTIVE_STATUSES})) < a.max_concurrent_tasks \
                ) AS role_agents_free \
           FROM board_column col JOIN board bd ON bd.id = col.board_id \
          WHERE col.board_id = ?1 AND bd.workspace_id = ?2 \
          ORDER BY col.ord"
    );
    let rows = sqlx::query(&sql)
        .bind(board_id)
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;

    let mut stages: Vec<StageHealth> = rows
        .iter()
        .map(|r| StageHealth {
            column_id: r.try_get("id").unwrap_or_default(),
            name: r.try_get("name").unwrap_or_default(),
            ord: r.try_get("ord").unwrap_or_default(),
            services_role: r.try_get("role").unwrap_or_default(),
            wip_limit: r.try_get("wip_limit").unwrap_or_default(),
            wip_active: r.try_get("wip_active").unwrap_or_default(),
            role_agents: r.try_get("role_agents").unwrap_or_default(),
            role_agents_free: r.try_get("role_agents_free").unwrap_or_default(),
            cards: r.try_get("cards").unwrap_or_default(),
            stuck: 0,
        })
        .collect();

    for stage in &mut stages {
        if stage.is_gated() {
            stage.stuck = stuck_in_column(pool, &stage.column_id, now_ms).await?;
        }
    }
    Ok(PipelineHealth { stages })
}

/// The pull statement's role predicate, correlated on `a` (the candidate agent),
/// `col` (the stage) and `bd` (the board's workspace).
///
/// Kept as one string constant rather than retyped per query: this predicate IS
/// the role gate, and a second, subtly different copy of it would make the health
/// light lie about the very thing it exists to report.
const ROLE_MATCH: &str = "col.services_role IS NOT NULL AND EXISTS ( \
       SELECT 1 FROM squad_member sm JOIN squad sq ON sq.id = sm.squad_id \
        WHERE sm.member_type = 'agent' AND sm.member_id = a.id \
          AND sq.workspace_id = bd.workspace_id \
          AND INSTR( \
                ',' || REPLACE(LOWER(sm.role), ' ', '') || ',', \
                ',' || LOWER(TRIM(col.services_role)) || ',' \
              ) > 0 )";

/// Count the STUCK cards of one gated column.
///
/// A card is stuck when it holds no active task, its issue is still open, and its
/// last sign of life (the newest task's `finished_at`, or the card's `added_at`
/// for a card that has never run) is older than [`STUCK_AFTER_MS`].
///
/// BLOCKED cards are then filtered out in Rust rather than in SQL: a card waiting
/// on an unfinished blocker is behaving exactly as designed, and reporting it as
/// stuck would make the light cry wolf. The blocker check reuses
/// [`CardDependencyRepo::unfinished_blockers_of`](crate::repo::card_dependency::CardDependencyRepo::unfinished_blockers_of),
/// the same read the board renders 🔒 from, instead of restating its
/// generation-aware SQL a second time — and it only runs for the handful of cards
/// that already aged past the threshold.
async fn stuck_in_column(
    pool: &SqlitePool,
    column_id: &str,
    now_ms: i64,
) -> Result<i64, sqlx::Error> {
    use crate::repo::card_dependency::CardDependencyRepo;

    let cutoff = now_ms.saturating_sub(STUCK_AFTER_MS);
    let sql = format!(
        "SELECT bc.issue_id AS issue_id FROM board_card bc \
           JOIN issue i ON i.id = bc.issue_id \
          WHERE bc.column_id = ?1 \
            AND i.state NOT IN ('closed','cancelled') \
            AND NOT EXISTS (SELECT 1 FROM agent_task_queue t \
                             WHERE t.issue_id = bc.issue_id \
                               AND t.status IN ({ACTIVE_STATUSES})) \
            AND COALESCE( \
                  (SELECT MAX(f.finished_at) FROM agent_task_queue f \
                    WHERE f.issue_id = bc.issue_id), \
                  bc.added_at) < ?2"
    );
    let candidates: Vec<String> =
        sqlx::query_scalar(&sql).bind(column_id).bind(cutoff).fetch_all(pool).await?;

    let mut stuck = 0;
    for issue_id in &candidates {
        if CardDependencyRepo::unfinished_blockers_of(pool, issue_id).await?.is_empty() {
            stuck += 1;
        }
    }
    Ok(stuck)
}
