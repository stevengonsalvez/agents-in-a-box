-- Hangar v1 schema, migration 0022: per-task provider usage (token/cost rollup,
-- e38.35).
--
-- Until now a provider run's token/cost usage was purely transient: the runner
-- streamed the agent's JSONL transcript to `{logs}/claude.jsonl` and pinned the
-- session id, but the final `result` line's `usage` (input/output tokens) and
-- `total_cost_usd` were written only to the log file — nothing PERSISTED or
-- AGGREGATED them. There was no in-Hangar view of token/cost spend, and no
-- per-agent rollup (the reference usage-dashboard pattern). This migration adds the
-- durable per-task aggregate the usage dashboard rolls up.
--
-- A `task_usage` row is one provider run's captured usage, written once at the
-- task's finalize seam:
--
--   - `task_id`       the task the usage is for — PRIMARY KEY, so exactly one
--                     usage row per task (a re-run replaces it via INSERT OR
--                     REPLACE). FK to `agent_task_queue(id)`.
--   - `workspace_id`  the owning workspace (resolved row id, never the slug), FK
--                     to `workspace(id)` so the rollup is tenant-isolated by the
--                     same scoping every snapshot enforces.
--   - `agent_id`      the agent that ran the task — the GROUP BY key the per-agent
--                     rollup buckets on. FK to `agent(id)`.
--   - `input_tokens`  prompt/input tokens the provider reported (the `usage`
--                     object's input total). INTEGER, defaults 0.
--   - `output_tokens` completion/output tokens the provider reported. INTEGER,
--                     defaults 0.
--   - `cost_usd`      total cost in US dollars the provider reported
--                     (`total_cost_usd`). REAL, defaults 0 — a provider that
--                     reports tokens but not cost lands 0.
--   - `created_at`    epoch milliseconds (INTEGER, like every Hangar timestamp).
--
-- ADD TABLE / ADD INDEX with no data backfill is an O(1) catalog change in
-- SQLite, so this is safe on a populated database: every pre-existing workspace
-- simply starts with zero recorded usage (an empty dashboard) until its next run
-- finalizes. Re-applying is a no-op via the migrator's version ledger.

CREATE TABLE task_usage (
    task_id       TEXT PRIMARY KEY REFERENCES agent_task_queue(id),
    workspace_id  TEXT NOT NULL REFERENCES workspace(id),
    agent_id      TEXT NOT NULL REFERENCES agent(id),
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd      REAL NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

-- The rollup reads "this workspace's usage, grouped by agent"; index the
-- (workspace_id, agent_id) pair the GROUP BY + total queries scan.
CREATE INDEX idx_task_usage_workspace_agent
    ON task_usage(workspace_id, agent_id);
