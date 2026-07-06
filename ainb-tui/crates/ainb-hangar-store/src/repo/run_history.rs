//! Typed repository wrapper over the `run_history` table + `cost_rollup` view
//! (P10 / D19).
//!
//! [`RunHistoryRepo`] is a thin, stateless sqlx layer over the durable per-run
//! observability record (migration 0029). Each row is one FINISHED provider run:
//! its provider / session / profile / outcome / duration and the token-cost it
//! reported. Unlike [`crate::repo::usage`] (the per-`task_id` upsert aggregate),
//! this is APPEND-ONLY — a re-run of a task appends a second row, so the history
//! preserves every run rather than only the latest per task.
//!
//! The daemon's run loop calls [`RunHistoryRepo::record`] at the finalize seam
//! (both the success and failure paths) for every finished run, so a run that was
//! once transient JSONL now lands durably on the timeline.
//!
//! Reads are **workspace-scoped** by the `workspace_id` column (mirroring the
//! tenant isolation every other repo enforces — a foreign tenant's runs are never
//! read):
//!
//!   - [`RunHistoryRepo::list_by_workspace`] — the newest-first run timeline (the
//!     History view's row source).
//!   - [`RunHistoryRepo::workspace_cost_rollup`] — the daily per-provider
//!     tokens + cost aggregate over the `cost_rollup` view (the card stat-strip /
//!     rollup source).

use sqlx::{Row, SqlitePool};

/// Parameters for recording one finished run's history row.
///
/// `run_id` is the PRIMARY KEY and is minted fresh per run (never the task id),
/// so a retried task appends a second row rather than overwriting the first — the
/// append-only history contract. The daemon already resolved the workspace +
/// provider before calling here.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRunHistory {
    /// Fresh per-run id (PRIMARY KEY).
    pub run_id: String,
    /// The task the run executed, or `None` for a task-less run.
    pub task_id: Option<String>,
    /// The owning workspace's resolved row id (never the slug) — the scoping key.
    pub workspace_id: String,
    /// The provider session id the run used, or `None`.
    pub session_id: Option<String>,
    /// The provider that executed the run (`claude` / `codex`).
    pub provider: String,
    /// The agent profile slug the run launched under, or `None` until P5 wires it.
    pub profile: Option<String>,
    /// When the run started (epoch ms), or `None` if it never reached `running`.
    pub started_at: Option<i64>,
    /// When the run finished (epoch ms).
    pub finished_at: i64,
    /// Terminal FSM result: `success` | `failed`.
    pub outcome: String,
    /// Prompt/input tokens the provider reported (0 when none).
    pub input_tokens: i64,
    /// Completion/output tokens the provider reported (0 when none).
    pub output_tokens: i64,
    /// Total run cost in USD the provider reported (0 when none).
    pub cost_usd: f64,
    /// Lines added by the run's diff (0 until diff plumbing lands).
    pub diff_add: i64,
    /// Lines removed by the run's diff (0 until diff plumbing lands).
    pub diff_del: i64,
}

/// One run-history row read back off the timeline
/// ([`RunHistoryRepo::list_by_workspace`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RunHistoryRow {
    /// The run's id (PRIMARY KEY).
    pub run_id: String,
    /// The task the run executed, or `None`.
    pub task_id: Option<String>,
    /// The provider session id the run used, or `None`.
    pub session_id: Option<String>,
    /// The provider that executed the run.
    pub provider: String,
    /// The agent profile slug the run launched under, or `None`.
    pub profile: Option<String>,
    /// When the run started (epoch ms), or `None`.
    pub started_at: Option<i64>,
    /// When the run finished (epoch ms).
    pub finished_at: i64,
    /// Terminal FSM result: `success` | `failed`.
    pub outcome: String,
    /// Prompt/input tokens the run reported.
    pub input_tokens: i64,
    /// Completion/output tokens the run reported.
    pub output_tokens: i64,
    /// Total run cost in USD.
    pub cost_usd: f64,
    /// Lines added by the run's diff.
    pub diff_add: i64,
    /// Lines removed by the run's diff.
    pub diff_del: i64,
}

/// One daily per-provider rollup row over the `cost_rollup` view
/// ([`RunHistoryRepo::workspace_cost_rollup`]).
#[derive(Debug, Clone, PartialEq)]
pub struct CostRollupRow {
    /// The provider the row aggregates.
    pub provider: String,
    /// The UTC day bucket (epoch-ms integer-divided by ms-per-day).
    pub day: i64,
    /// Summed input tokens over the day's runs for this provider.
    pub input_tokens: i64,
    /// Summed output tokens over the day's runs for this provider.
    pub output_tokens: i64,
    /// Summed cost (USD) over the day's runs for this provider.
    pub cost_usd: f64,
    /// Number of runs the bucket aggregates.
    pub runs: i64,
}

/// Stateless typed wrapper over the `run_history` table + `cost_rollup` view.
pub struct RunHistoryRepo;

impl RunHistoryRepo {
    /// Append one finished run's history row.
    ///
    /// Keyed by `run_id` (a fresh per-run id), so this is a pure INSERT — a
    /// re-run of the same task appends a distinct row rather than replacing the
    /// prior one (the append-only history contract, in contrast to the
    /// `task_usage` upsert). The FKs to `workspace` (+ `agent_task_queue` when
    /// `task_id` is present) keep a dangling-id insert from landing.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — e.g. a workspace / task FK
    /// violation, or a duplicate `run_id`.
    pub async fn record(pool: &SqlitePool, run: &NewRunHistory) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO run_history \
             (run_id, task_id, workspace_id, session_id, provider, profile, \
              started_at, finished_at, outcome, input_tokens, output_tokens, \
              cost_usd, diff_add, diff_del) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run.run_id)
        .bind(&run.task_id)
        .bind(&run.workspace_id)
        .bind(&run.session_id)
        .bind(&run.provider)
        .bind(&run.profile)
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(&run.outcome)
        .bind(run.input_tokens)
        .bind(run.output_tokens)
        .bind(run.cost_usd)
        .bind(run.diff_add)
        .bind(run.diff_del)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// The workspace's run timeline, newest finished first, capped at `limit`.
    ///
    /// Workspace-scoped: a foreign tenant's runs never appear. Ordered
    /// `finished_at DESC, run_id` (the `run_id` tiebreak keeps two same-instant
    /// runs deterministic). An unknown / empty workspace yields an empty vec.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
        limit: i64,
    ) -> Result<Vec<RunHistoryRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT run_id, task_id, session_id, provider, profile, started_at, \
                    finished_at, outcome, input_tokens, output_tokens, cost_usd, \
                    diff_add, diff_del \
             FROM run_history WHERE workspace_id = ? \
             ORDER BY finished_at DESC, run_id DESC \
             LIMIT ?",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(RunHistoryRow {
                    run_id: row.try_get("run_id")?,
                    task_id: row.try_get("task_id")?,
                    session_id: row.try_get("session_id")?,
                    provider: row.try_get("provider")?,
                    profile: row.try_get("profile")?,
                    started_at: row.try_get("started_at")?,
                    finished_at: row.try_get("finished_at")?,
                    outcome: row.try_get("outcome")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    cost_usd: row.try_get("cost_usd")?,
                    diff_add: row.try_get("diff_add")?,
                    diff_del: row.try_get("diff_del")?,
                })
            })
            .collect()
    }

    /// The daily per-provider cost rollup for a workspace, over the `cost_rollup`
    /// view.
    ///
    /// Workspace-scoped: a foreign tenant's runs never count. Ordered by day
    /// descending then provider so the most recent bucket is first. An unknown /
    /// empty workspace yields an empty vec.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the aggregate query fails.
    pub async fn workspace_cost_rollup(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<CostRollupRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT provider, day, input_tokens, output_tokens, cost_usd, runs \
             FROM cost_rollup WHERE workspace_id = ? \
             ORDER BY day DESC, provider",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(CostRollupRow {
                    provider: row.try_get("provider")?,
                    day: row.try_get("day")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    cost_usd: row.try_get("cost_usd")?,
                    runs: row.try_get("runs")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    /// Seed one workspace + runtime + user + agent so the FK-scoped inserts
    /// resolve.
    async fn seed(pool: &SqlitePool, ws: &str, rt: &str, agent: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)",
        )
        .bind(ws)
        .bind(ws)
        .bind(ws)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, 0)")
            .bind(format!("user-{ws}"))
            .bind(format!("{ws}@x.test"))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO agent_runtime \
             (id, workspace_id, daemon_id, provider, runtime_mode, status) \
             VALUES (?, ?, 'd', 'claude', 'local', 'online')",
        )
        .bind(rt)
        .bind(ws)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO agent \
             (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, ?, ?, 'workspace', ?)",
        )
        .bind(agent)
        .bind(ws)
        .bind(agent)
        .bind(rt)
        .bind(format!("user-{ws}"))
        .execute(pool)
        .await
        .unwrap();
    }

    /// Seed a `done` task so a run's `task_id` FK resolves.
    async fn seed_task(pool: &SqlitePool, id: &str, ws: &str, rt: &str, agent: &str) {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, status, created_at) \
             VALUES (?, ?, ?, ?, 'done', 0)",
        )
        .bind(id)
        .bind(ws)
        .bind(rt)
        .bind(agent)
        .execute(pool)
        .await
        .unwrap();
    }

    fn run(
        run_id: &str,
        task_id: Option<&str>,
        ws: &str,
        provider: &str,
        finished: i64,
        outcome: &str,
        tin: i64,
        tout: i64,
        cost: f64,
    ) -> NewRunHistory {
        NewRunHistory {
            run_id: run_id.into(),
            task_id: task_id.map(Into::into),
            workspace_id: ws.into(),
            session_id: Some(format!("sess-{run_id}")),
            provider: provider.into(),
            profile: None,
            started_at: Some(finished - 1000),
            finished_at: finished,
            outcome: outcome.into(),
            input_tokens: tin,
            output_tokens: tout,
            cost_usd: cost,
            diff_add: 0,
            diff_del: 0,
        }
    }

    #[tokio::test]
    async fn record_appends_and_list_is_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;
        seed_task(pool, "t1", "ws-a", "rt-a", "agent-x").await;

        // Two runs of the SAME task — the append-only contract keeps BOTH.
        RunHistoryRepo::record(
            pool,
            &run(
                "r1",
                Some("t1"),
                "ws-a",
                "claude",
                1000,
                "failed",
                100,
                20,
                0.001,
            ),
        )
        .await
        .unwrap();
        RunHistoryRepo::record(
            pool,
            &run(
                "r2",
                Some("t1"),
                "ws-a",
                "claude",
                2000,
                "success",
                500,
                80,
                0.005,
            ),
        )
        .await
        .unwrap();

        let rows = RunHistoryRepo::list_by_workspace(pool, "ws-a", 50).await.unwrap();
        assert_eq!(rows.len(), 2, "a re-run appends, never overwrites");
        // Newest finished first.
        assert_eq!(rows[0].run_id, "r2");
        assert_eq!(rows[0].outcome, "success");
        assert_eq!(rows[0].input_tokens, 500);
        assert_eq!(rows[0].task_id.as_deref(), Some("t1"));
        assert_eq!(rows[1].run_id, "r1");
        assert_eq!(rows[1].outcome, "failed");
    }

    #[tokio::test]
    async fn list_respects_limit_and_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;
        seed(pool, "ws-b", "rt-b", "agent-z").await;

        for i in 0..5 {
            RunHistoryRepo::record(
                pool,
                &run(
                    &format!("a{i}"),
                    None,
                    "ws-a",
                    "claude",
                    1000 + i,
                    "success",
                    10,
                    5,
                    0.0,
                ),
            )
            .await
            .unwrap();
        }
        RunHistoryRepo::record(
            pool,
            &run("b1", None, "ws-b", "codex", 9999, "success", 1, 1, 9.9),
        )
        .await
        .unwrap();

        // Limit caps the returned rows (newest kept).
        let capped = RunHistoryRepo::list_by_workspace(pool, "ws-a", 3).await.unwrap();
        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].run_id, "a4");

        // ws-a never sees ws-b's run.
        let all_a = RunHistoryRepo::list_by_workspace(pool, "ws-a", 50).await.unwrap();
        assert_eq!(all_a.len(), 5);
        assert!(all_a.iter().all(|r| r.provider == "claude"));

        // Unknown workspace yields an empty timeline.
        assert!(RunHistoryRepo::list_by_workspace(pool, "ws-nope", 50).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cost_rollup_view_buckets_by_provider_and_day() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;

        // Same day (< 86_400_000 ms apart), two providers.
        RunHistoryRepo::record(
            pool,
            &run("r1", None, "ws-a", "claude", 1000, "success", 100, 20, 0.01),
        )
        .await
        .unwrap();
        RunHistoryRepo::record(
            pool,
            &run("r2", None, "ws-a", "claude", 2000, "success", 200, 40, 0.02),
        )
        .await
        .unwrap();
        RunHistoryRepo::record(
            pool,
            &run("r3", None, "ws-a", "codex", 3000, "success", 50, 10, 0.005),
        )
        .await
        .unwrap();

        let rollup = RunHistoryRepo::workspace_cost_rollup(pool, "ws-a").await.unwrap();
        // Two provider buckets on day 0.
        assert_eq!(rollup.len(), 2);
        let claude = rollup.iter().find(|r| r.provider == "claude").unwrap();
        assert_eq!(claude.day, 0);
        assert_eq!(claude.input_tokens, 300, "100+200 summed");
        assert_eq!(claude.output_tokens, 60, "20+40 summed");
        assert!((claude.cost_usd - 0.03).abs() < 1e-9, "0.01+0.02");
        assert_eq!(claude.runs, 2);
        let codex = rollup.iter().find(|r| r.provider == "codex").unwrap();
        assert_eq!(codex.runs, 1);
        assert!((codex.cost_usd - 0.005).abs() < 1e-9);

        // Unknown workspace yields an empty rollup.
        assert!(RunHistoryRepo::workspace_cost_rollup(pool, "ws-nope").await.unwrap().is_empty());
    }
}
