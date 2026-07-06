-- Hangar v1 schema, migration 0029: durable per-run observability history
-- (P10 / D19).
--
-- Until now a provider run's only durable footprint was the terminal
-- `agent_task_queue` row plus the per-task `task_usage` aggregate (migration
-- 0022). Neither preserves a RUN-scoped record: a task that is re-run replaces
-- its `task_usage` row (keyed by `task_id`), so the individual run's
-- provider / session / outcome / duration / token-cost is lost to history. The
-- observability surface (a per-workspace run timeline + cost rollups + OTLP
-- spans) needs every finished run kept, not just the latest per task.
--
-- A `run_history` row is one finished provider run, appended once at the run
-- loop's finalize seam (both the success and failure paths):
--
--   - `run_id`        a fresh id minted per run — PRIMARY KEY. Distinct from
--                     `task_id` so a retried task appends a SECOND run row
--                     rather than overwriting the first (the whole point of a
--                     history vs the `task_usage` upsert).
--   - `task_id`       the task the run executed, or NULL for a run with no task
--                     row (a future manual / interactive session). FK to
--                     `agent_task_queue(id)` when present. Nullable per the D19
--                     schema (`task_id?`).
--   - `workspace_id`  the owning workspace (resolved row id, never the slug), FK
--                     to `workspace(id)` — the tenant-scoping column every
--                     history read filters on, exactly as `task_usage` does.
--   - `session_id`    the provider session id the run used, or NULL if the
--                     provider opened none.
--   - `provider`      the provider that executed the run (`claude` / `codex`),
--                     the GROUP BY key the cost rollup buckets on.
--   - `profile`       the agent profile slug the run launched under, or NULL
--                     until the P5 profile plumbing threads it through dispatch
--                     (D16: board assignee slug == profile slug).
--   - `started_at`    when the run started (epoch ms), or NULL if it never
--                     reached `running` (defensive; the finalize seam always has
--                     a start).
--   - `finished_at`   when the run finished (epoch ms) — NOT NULL, the timeline
--                     sort key and the cost rollup's day bucket source.
--   - `outcome`       `success` | `failed`, the terminal FSM result.
--   - `input_tokens`  prompt/input tokens the provider reported (0 when none).
--   - `output_tokens` completion/output tokens the provider reported (0 when none).
--   - `cost_usd`      total run cost in USD the provider reported (0 when none).
--   - `diff_add`      lines added by the run's diff, or 0 until diff plumbing
--                     lands (the runner does not yet surface a diff stat).
--   - `diff_del`      lines removed by the run's diff, same caveat.
--
-- The `cost_rollup` VIEW is a live aggregate over `run_history` — no separate
-- write path, so it can never drift from the rows it sums. It buckets by
-- workspace + provider + UTC day (epoch-ms / 86_400_000), summing tokens + cost
-- and counting runs. A VIEW (not a table) is the right shape here: the rollup is
-- a pure function of the history rows, and SQLite computes it on read.
--
-- ADD TABLE / ADD INDEX / ADD VIEW with no data backfill is an O(1) catalog
-- change in SQLite, safe on a populated database: every pre-existing workspace
-- simply starts with an empty history (and therefore an empty rollup) until its
-- next run finalizes. Re-applying is a no-op via the migrator's version ledger.

CREATE TABLE run_history (
    run_id        TEXT PRIMARY KEY,
    task_id       TEXT REFERENCES agent_task_queue(id),
    workspace_id  TEXT NOT NULL REFERENCES workspace(id),
    session_id    TEXT,
    provider      TEXT NOT NULL,
    profile       TEXT,
    started_at    INTEGER,
    finished_at   INTEGER NOT NULL,
    outcome       TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd      REAL NOT NULL DEFAULT 0,
    diff_add      INTEGER NOT NULL DEFAULT 0,
    diff_del      INTEGER NOT NULL DEFAULT 0
);

-- The per-workspace timeline reads "this workspace's runs, newest first"; index
-- the (workspace_id, finished_at DESC) pair the history query scans + orders on.
CREATE INDEX idx_run_history_workspace_finished
    ON run_history(workspace_id, finished_at DESC);

-- cost_rollup: live daily per-(workspace, provider) tokens + cost aggregate over
-- run_history. A VIEW so it is always consistent with the rows it sums (no
-- second write path to keep in step). `day` is the UTC day bucket the run
-- finished in (epoch-ms integer-divided by ms-per-day).
CREATE VIEW cost_rollup AS
SELECT
    workspace_id,
    provider,
    finished_at / 86400000        AS day,
    SUM(input_tokens)             AS input_tokens,
    SUM(output_tokens)            AS output_tokens,
    SUM(cost_usd)                 AS cost_usd,
    COUNT(*)                      AS runs
FROM run_history
GROUP BY workspace_id, provider, finished_at / 86400000;
