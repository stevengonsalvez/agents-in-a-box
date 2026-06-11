-- Hangar v1 schema, migration 0019: autopilot execution modes + concurrency
-- policies (e38.19).
--
-- An autopilot's tick model was hard-coded at v1: the fire path always enqueued
-- a task with `issue_id = NULL` (migrations 0009/0010, autopilot_run.rs), and the
-- scheduler's concurrency control was a single integer (`max_concurrent_runs`)
-- with only ONE policy — skip-when-at-limit (scheduler.rs). Multica shipped two
-- orthogonal knobs the 0009 comment collapsed away; this migration restores them
-- as two `autopilot` config columns.
--
-- # Two new `autopilot` columns (ALTER, not a rewrite)
--
--   - `execution_mode` TEXT — what the FIRE path materialises per tick:
--       * `run_only`     (the v1 hard-coded path): enqueue a task with
--                        `issue_id = NULL`. A background run that produces no
--                        tracked issue. THE DEFAULT — every pre-existing
--                        autopilot keeps its exact current behaviour.
--       * `create_issue`: the fired tick first creates a fresh `issue` (authored
--                        by the autopilot's agent) and then enqueues the task
--                        AGAINST that issue, so the run has a tracked work item.
--     The `CHECK` keeps the discriminant honest.
--
--   - `concurrency_policy` TEXT — what the SCHEDULER does when a tick comes due
--     while the autopilot is already at `max_concurrent_runs` in-flight runs:
--       * `skip`    (the v1 behaviour): drop the tick — no run, no task; warn +
--                   emit `tick_skipped`; reschedule to the next slot. THE
--                   DEFAULT.
--       * `queue`:  fire the tick ANYWAY — create the run + task. The shared
--                   claim/dispatch queue (agent_task_queue) then drains the
--                   enqueued tasks in order, so the second tick RUNS AFTER the
--                   in-flight one rather than being dropped.
--       * `replace`: supersede the in-flight run(s) — mark each open
--                   `autopilot_run` `cancelled` (stamping `completed_at`) and
--                   cancel its task — then fire a fresh run + task. The latest
--                   tick wins; the stale in-flight work is abandoned.
--     The `CHECK` keeps the discriminant honest.
--
-- `ALTER TABLE ... ADD COLUMN` with a constant default is a catalog-only change
-- on SQLite (no table rewrite): every pre-existing `autopilot` row is untouched
-- and simply reads the column default (`run_only` + `skip` = the exact v1
-- behaviour) until an operator changes it.

ALTER TABLE autopilot ADD COLUMN execution_mode TEXT NOT NULL DEFAULT 'run_only'
    CHECK (execution_mode IN ('create_issue', 'run_only'));
ALTER TABLE autopilot ADD COLUMN concurrency_policy TEXT NOT NULL DEFAULT 'skip'
    CHECK (concurrency_policy IN ('skip', 'queue', 'replace'));
