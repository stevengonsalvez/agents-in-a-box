-- Hangar v1 schema, migration 0057: the bare `api` autopilot trigger + a
-- first-class `skipped` run status (multica parity item 15).
--
-- Two independent, related changes:
--
--   (a) `autopilot.api_trigger_enabled` — the THIRD trigger surface after cron
--       (0009) and webhook (0018). Multica models triggers as rows in a separate
--       `autopilot_trigger` table with `kind IN ('schedule','webhook','api')`
--       (`server/migrations/042_autopilot.up.sql:31`); hangar collapses trigger
--       config into columns on `autopilot` — exactly how 0018 modelled webhook —
--       so `api` is one boolean column, not a new table.
--
--       `api` is the BARE programmatic fire: no cron expression, no webhook
--       token/HMAC. A caller with normal API access (the daemon RPC, or the
--       local CLI) fires the autopilot directly. Multica validates that only
--       `schedule` requires a cron expression (`handler/autopilot.go:445-448`);
--       `api` requires nothing beyond the autopilot existing.
--
--   (b) `autopilot_run` gains `skipped` as a terminal status, plus `source` and
--       `failure_reason`. Multica's rationale
--       (`079_autopilot_run_skipped_status.up.sql`): "the admission gate needs a
--       non-failure terminal status to record dispatches that were intentionally
--       declined … Reusing `failed` would pollute the failure-rate signal."
--       Before this migration a skip was an EVENT only (`SchedulerEvent::
--       TickSkipped`) and wrote no row, so declined dispatches were invisible to
--       any read path.
--
-- # (a) is a plain ADD COLUMN (catalog-only, no rewrite)
--
-- Default 0: every pre-existing autopilot stays api-OFF until an operator opts
-- in, mirroring `webhook_enabled` (0018) exactly — half-configured means
-- un-firable.
ALTER TABLE autopilot ADD COLUMN api_trigger_enabled INTEGER NOT NULL DEFAULT 0
    CHECK (api_trigger_enabled IN (0, 1));

-- # (b) needs a table rebuild, and the rebuild order matters
--
-- SQLite has no `ALTER TABLE … ADD/DROP CONSTRAINT`, so widening the 0009
-- `status` CHECK (which actively REJECTS 'skipped') requires recreating the
-- table. The trigger-based dodge used by `0049_issue_state_blocked_cancelled`
-- does not apply here — that column had no constraint to widen.
--
-- `PRAGMA foreign_keys` is ON in this crate (`src/store.rs`, asserted by
-- `fk_enforcement_is_on_and_rejects_orphans`) and
-- `agent_task_queue.autopilot_run_id REFERENCES autopilot_run(id)` (0010) is the
-- only inbound FK. That rules out the naive create-new / copy / drop / RENAME
-- sequence: the deferred-FK counter incremented by `DROP TABLE`'s implicit
-- delete is never decremented (the child rows were inserted BEFORE the drop), so
-- `COMMIT` fails with `FOREIGN KEY constraint failed`.
--
-- The sequence below is the verified one: copy → drop → recreate UNDER THE REAL
-- NAME → re-insert. Because the parent rows come back after the drop, the
-- deferred violation counter returns to zero and COMMIT succeeds. No
-- `ALTER … RENAME` is used, so there is no `legacy_alter_table` schema-reparse
-- hazard and `agent_task_queue`'s FK clause text is untouched.
--
-- sqlx runs each migration inside a transaction; `PRAGMA foreign_keys` cannot be
-- toggled inside one, but `defer_foreign_keys` CAN (the same trick as
-- `repo/workspace.rs`) and resets to OFF at commit.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE autopilot_run_copy AS SELECT * FROM autopilot_run;

DROP TABLE autopilot_run;

CREATE TABLE autopilot_run (
    id             TEXT PRIMARY KEY,
    autopilot_id   TEXT NOT NULL REFERENCES autopilot(id),
    started_at     INTEGER NOT NULL,
    completed_at   INTEGER,
    status         TEXT NOT NULL DEFAULT 'running'
                   CHECK (status IN ('running', 'completed', 'failed', 'cancelled', 'skipped')),
    -- Which trigger fired this run. Multica stamps the same discriminant on
    -- every dispatch (`042_autopilot.up.sql:54`,
    -- `DispatchAutopilot(ctx, autopilot, triggerID, source, payload)`).
    source         TEXT NOT NULL DEFAULT 'schedule'
                   CHECK (source IN ('schedule', 'manual', 'webhook', 'api')),
    -- Why a declined (`skipped`) dispatch was declined — the admission reason.
    -- NULL for every other status.
    failure_reason TEXT
);

-- Pre-existing rows backfill to `source = 'schedule'` (the only unattended path
-- that existed before this migration) and `failure_reason = NULL`. Manual and
-- webhook history is NOT retroactively re-attributed — we do not invent
-- provenance for rows that never recorded it.
INSERT INTO autopilot_run (id, autopilot_id, started_at, completed_at, status)
    SELECT id, autopilot_id, started_at, completed_at, status FROM autopilot_run_copy;

DROP TABLE autopilot_run_copy;

-- `idx_autopilot_run_autopilot_started` (0009) is dropped with the table and
-- MUST be recreated: it serves both `list_runs` and the scheduler's in-flight
-- count.
CREATE INDEX idx_autopilot_run_autopilot_started
    ON autopilot_run(autopilot_id, started_at DESC);

-- Note for writers: `skipped` is TERMINAL. Every writer MUST stamp
-- `completed_at`, otherwise the in-flight count (`WHERE completed_at IS NULL`)
-- would count skips as in flight and wedge the autopilot at its limit forever.
--
-- `autopilot_webhook_delivery.run_id` (0018) deliberately has no FK, so it is
-- unaffected by the rebuild.
