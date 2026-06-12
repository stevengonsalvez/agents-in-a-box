//! Typed repository wrapper over the `task_usage` table (e38.35).
//!
//! [`UsageRepo`] is a thin, stateless sqlx layer over the per-task provider-usage
//! aggregate (migration 0022). Each row is one provider run's captured usage:
//! the input/output tokens and the total cost the agent CLI reported on its final
//! `result` line. The daemon's run loop calls [`UsageRepo::record`] at the
//! finalize seam for every run that reported usage, so the once-transient JSONL
//! totals now land durably.
//!
//! The dashboard reads two shapes off this table, both **workspace-scoped** by the
//! `workspace_id` column (mirroring the tenant isolation every other repo
//! enforces — a foreign tenant's usage is never read):
//!
//!   - [`UsageRepo::workspace_totals`] — the grand total tokens + cost across the
//!     whole workspace (the dashboard header figure).
//!   - [`UsageRepo::rollup_by_agent`] — the same totals grouped by agent (the
//!     per-agent breakdown rows), ordered by cost descending so the heaviest
//!     spender is first.

use sqlx::{Row, SqlitePool};

/// Parameters for recording one task's provider usage.
///
/// `task_id` is the PRIMARY KEY, so recording is upsert-shaped (a re-run of the
/// same task replaces its prior usage row via `INSERT OR REPLACE`). The daemon
/// already resolved the workspace + agent row ids before calling here.
#[derive(Debug, Clone, PartialEq)]
pub struct NewUsage {
    /// The task the usage is for (PRIMARY KEY; FK to `agent_task_queue`).
    pub task_id: String,
    /// The owning workspace's resolved row id (never the slug).
    pub workspace_id: String,
    /// The agent that ran the task — the GROUP BY key of the per-agent rollup.
    pub agent_id: String,
    /// Prompt/input tokens the provider reported.
    pub input_tokens: i64,
    /// Completion/output tokens the provider reported.
    pub output_tokens: i64,
    /// Total cost in US dollars the provider reported (0 when none reported).
    pub cost_usd: f64,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// The grand totals across a workspace's recorded usage
/// ([`UsageRepo::workspace_totals`]).
///
/// A workspace with no recorded usage answers all-zero (the empty-dashboard
/// state), so this is always present — never an `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UsageTotals {
    /// Sum of input tokens over every recorded run.
    pub input_tokens: i64,
    /// Sum of output tokens over every recorded run.
    pub output_tokens: i64,
    /// Sum of cost (USD) over every recorded run.
    pub cost_usd: f64,
    /// Number of recorded runs (rows) the totals aggregate.
    pub runs: i64,
}

/// One per-agent rollup row ([`UsageRepo::rollup_by_agent`]): an agent's summed
/// tokens + cost over the runs it executed in the workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentUsage {
    /// The agent the row aggregates (`agent.id`, the GROUP BY key).
    pub agent_id: String,
    /// Sum of input tokens over this agent's runs.
    pub input_tokens: i64,
    /// Sum of output tokens over this agent's runs.
    pub output_tokens: i64,
    /// Sum of cost (USD) over this agent's runs.
    pub cost_usd: f64,
    /// Number of runs this agent executed.
    pub runs: i64,
}

/// Stateless typed wrapper over the `task_usage` table.
pub struct UsageRepo;

impl UsageRepo {
    /// Record (upsert) one task's provider usage.
    ///
    /// Keyed by `task_id` (the PRIMARY KEY), so a re-run of the same task replaces
    /// its prior usage row rather than double-counting it. The FKs to `workspace`
    /// + `agent` keep a dangling-id insert from landing.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the upsert fails — e.g. a workspace / agent /
    /// task FK violation.
    pub async fn record(pool: &SqlitePool, usage: &NewUsage) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR REPLACE INTO task_usage \
             (task_id, workspace_id, agent_id, input_tokens, output_tokens, cost_usd, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&usage.task_id)
        .bind(&usage.workspace_id)
        .bind(&usage.agent_id)
        .bind(usage.input_tokens)
        .bind(usage.output_tokens)
        .bind(usage.cost_usd)
        .bind(usage.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// The grand totals (tokens in/out, cost, run count) across a workspace's
    /// recorded usage.
    ///
    /// Workspace-scoped: a foreign tenant's usage never counts. A workspace with
    /// no recorded usage answers all-zero (the `SUM` of an empty set is `NULL`,
    /// coalesced to `0`), so the dashboard header always has a figure to render.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the aggregate query fails.
    pub async fn workspace_totals(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<UsageTotals, sqlx::Error> {
        let row = sqlx::query(
            "SELECT \
                COALESCE(SUM(input_tokens), 0)  AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                COALESCE(SUM(cost_usd), 0.0)    AS cost_usd, \
                COUNT(*)                        AS runs \
             FROM task_usage WHERE workspace_id = ?",
        )
        .bind(workspace_id)
        .fetch_one(pool)
        .await?;
        Ok(UsageTotals {
            input_tokens: row.try_get("input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            cost_usd: row.try_get("cost_usd")?,
            runs: row.try_get("runs")?,
        })
    }

    /// The per-agent rollup: each agent's summed tokens + cost over its runs in a
    /// workspace, heaviest spender first.
    ///
    /// Workspace-scoped: a foreign tenant's usage never appears. Ordered
    /// `cost_usd DESC, agent_id` so the dashboard's top row is the costliest agent
    /// (the `agent_id` tiebreak keeps two same-cost agents deterministic). An
    /// unknown / empty workspace yields an empty vec.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the grouped query fails.
    pub async fn rollup_by_agent(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<AgentUsage>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT \
                agent_id, \
                COALESCE(SUM(input_tokens), 0)  AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                COALESCE(SUM(cost_usd), 0.0)    AS cost_usd, \
                COUNT(*)                        AS runs \
             FROM task_usage WHERE workspace_id = ? \
             GROUP BY agent_id \
             ORDER BY cost_usd DESC, agent_id",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AgentUsage {
                    agent_id: row.try_get("agent_id")?,
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
    /// resolve. Returns nothing; ids are stable per `(ws, agent)`.
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

    /// Seed a `done` task so a usage row's `task_id` FK resolves.
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

    fn usage(task: &str, ws: &str, agent: &str, tin: i64, tout: i64, cost: f64) -> NewUsage {
        NewUsage {
            task_id: task.into(),
            workspace_id: ws.into(),
            agent_id: agent.into(),
            input_tokens: tin,
            output_tokens: tout,
            cost_usd: cost,
            created_at: 100,
        }
    }

    #[tokio::test]
    async fn totals_sum_across_runs_and_rollup_groups_by_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;
        seed(pool, "ws-a", "rt-a", "agent-y").await;

        // agent-x: two runs; agent-y: one run.
        for (task, agent, tin, tout, cost) in [
            ("t1", "agent-x", 1000_i64, 200_i64, 0.01_f64),
            ("t2", "agent-x", 500, 100, 0.005),
            ("t3", "agent-y", 2000, 800, 0.04),
        ] {
            seed_task(pool, task, "ws-a", "rt-a", agent).await;
            UsageRepo::record(pool, &usage(task, "ws-a", agent, tin, tout, cost))
                .await
                .unwrap();
        }

        // Grand totals sum every run.
        let totals = UsageRepo::workspace_totals(pool, "ws-a").await.unwrap();
        assert_eq!(totals.input_tokens, 3500, "1000+500+2000");
        assert_eq!(totals.output_tokens, 1100, "200+100+800");
        assert!((totals.cost_usd - 0.055).abs() < 1e-9, "0.01+0.005+0.04");
        assert_eq!(totals.runs, 3);

        // Per-agent rollup groups by agent, heaviest-cost first (agent-y at 0.04
        // before agent-x at 0.015).
        let rollup = UsageRepo::rollup_by_agent(pool, "ws-a").await.unwrap();
        assert_eq!(rollup.len(), 2);
        assert_eq!(rollup[0].agent_id, "agent-y");
        assert_eq!(rollup[0].input_tokens, 2000);
        assert_eq!(rollup[0].runs, 1);
        assert_eq!(rollup[1].agent_id, "agent-x");
        assert_eq!(rollup[1].input_tokens, 1500, "1000+500 grouped");
        assert_eq!(rollup[1].output_tokens, 300, "200+100 grouped");
        assert_eq!(rollup[1].runs, 2);
    }

    #[tokio::test]
    async fn record_is_upsert_keyed_by_task_so_a_rerun_replaces_not_doublecounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;
        seed_task(pool, "t1", "ws-a", "rt-a", "agent-x").await;

        UsageRepo::record(pool, &usage("t1", "ws-a", "agent-x", 1000, 200, 0.01))
            .await
            .unwrap();
        // Re-record the SAME task (a retry that produced different usage).
        UsageRepo::record(pool, &usage("t1", "ws-a", "agent-x", 1500, 300, 0.02))
            .await
            .unwrap();

        let totals = UsageRepo::workspace_totals(pool, "ws-a").await.unwrap();
        assert_eq!(totals.runs, 1, "the same task is one row, not two");
        assert_eq!(
            totals.input_tokens, 1500,
            "the re-record replaced the prior row"
        );
        assert!((totals.cost_usd - 0.02).abs() < 1e-9);
    }

    #[tokio::test]
    async fn totals_and_rollup_are_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed(pool, "ws-a", "rt-a", "agent-x").await;
        seed(pool, "ws-b", "rt-b", "agent-z").await;
        seed_task(pool, "ta", "ws-a", "rt-a", "agent-x").await;
        seed_task(pool, "tb", "ws-b", "rt-b", "agent-z").await;
        UsageRepo::record(pool, &usage("ta", "ws-a", "agent-x", 100, 50, 0.001))
            .await
            .unwrap();
        UsageRepo::record(pool, &usage("tb", "ws-b", "agent-z", 999, 999, 9.9))
            .await
            .unwrap();

        // ws-a sees only its own usage.
        let totals_a = UsageRepo::workspace_totals(pool, "ws-a").await.unwrap();
        assert_eq!(totals_a.input_tokens, 100);
        assert_eq!(totals_a.runs, 1);
        let rollup_a = UsageRepo::rollup_by_agent(pool, "ws-a").await.unwrap();
        assert_eq!(rollup_a.len(), 1);
        assert_eq!(rollup_a[0].agent_id, "agent-x");

        // An unknown workspace yields all-zero totals + an empty rollup.
        let totals_none = UsageRepo::workspace_totals(pool, "ws-nope").await.unwrap();
        assert_eq!(totals_none, UsageTotals::default());
        assert!(UsageRepo::rollup_by_agent(pool, "ws-nope").await.unwrap().is_empty());
    }
}
