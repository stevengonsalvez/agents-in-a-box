-- Hangar v1 schema, migration 0009: autopilots (cron-scheduled repeating runs).
--
-- An `autopilot` is a stored `(cron_expr, agent, instructions)` triple the
-- daemon scheduler thread (P7.3) fires at each tick, enqueuing an
-- `agent_task_queue` row tagged with the firing `autopilot_run.id` (P7.4). The
-- the reference `skip|queue|replace` trigger model is collapsed at v1 to a single
-- `max_concurrent_runs` integer; the separate `autopilot_trigger` table is
-- deferred (webhook / api triggers are out of scope).
--
-- # Why CREATE (not ALTER)
--
-- The P0 schema (migrations 0001-0008) never materialised the `autopilot` /
-- `autopilot_run` tables — they were only ever referenced in column comments
-- (`agent_task_queue` placeholders). So this migration *creates* both, rather
-- than the `ALTER TABLE autopilot ...` the (stale) P7 plan text assumed.
--
-- # Time columns are epoch-millis INTEGER (see 0001)
--
-- The whole schema stores time as INTEGER epoch-milliseconds, not the SQLite
-- `DATETIME` affinity. `next_tick_at`, `started_at`, and `completed_at` follow
-- that convention. The cron math (P7.1) is chrono-typed; callers bridge with
-- `autopilot::cron::{millis_to_utc, utc_to_millis}` at the storage boundary.
--
-- # Booleans are INTEGER 0/1
--
-- SQLite has no native boolean; `enabled` is an INTEGER constrained to 0/1, the
-- standard idiom (cf. the engine's own `1`/`0` literals).

CREATE TABLE autopilot (
    id                   TEXT PRIMARY KEY,
    workspace_id         TEXT NOT NULL REFERENCES workspace(id),
    agent_id             TEXT NOT NULL REFERENCES agent(id),
    name                 TEXT NOT NULL,
    instructions         TEXT,
    cron_expr            TEXT NOT NULL,
    max_concurrent_runs  INTEGER NOT NULL DEFAULT 1,
    next_tick_at         INTEGER,
    enabled              INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at           INTEGER NOT NULL
);

-- A name is unique within a workspace: re-creating `daily-triage` in the same
-- tenant is a conflict, but two workspaces may each own one. PRAGMA
-- foreign_keys is off in this crate, so this index is the authoritative guard
-- (mirrors `idx_skill_workspace_name`, migration 0008).
CREATE UNIQUE INDEX idx_autopilot_workspace_name
    ON autopilot(workspace_id, name);

-- The scheduler's hot path: `ORDER BY next_tick_at ASC LIMIT 1` over the
-- enabled rows of a workspace. Partial (WHERE enabled = 1) so disabled rows
-- never bloat the index the tick loop scans.
CREATE INDEX idx_autopilot_next_tick
    ON autopilot(workspace_id, next_tick_at)
    WHERE enabled = 1;

CREATE TABLE autopilot_run (
    id           TEXT PRIMARY KEY,
    autopilot_id TEXT NOT NULL REFERENCES autopilot(id),
    started_at   INTEGER NOT NULL,
    completed_at INTEGER,
    status       TEXT NOT NULL DEFAULT 'running'
                 CHECK (status IN ('running', 'completed', 'failed', 'cancelled'))
);

-- Run-history reads (`list_runs`) and the scheduler's in-flight count both
-- filter by `autopilot_id`; the history read additionally orders by
-- `started_at DESC`. A composite index serves both.
CREATE INDEX idx_autopilot_run_autopilot_started
    ON autopilot_run(autopilot_id, started_at DESC);
