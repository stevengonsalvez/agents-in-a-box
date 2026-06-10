-- Hangar v1 schema, migration 0010: link a queue task back to its autopilot run.
--
-- P7.4's fire path inserts an `autopilot_run` row and an `agent_task_queue` row
-- in one transaction. This column is the back-link: `autopilot_run_id` points at
-- the firing run, or is NULL for ordinary issue / chat tasks. The 0004 comment
-- anticipated autopilot tasks as `issue_id IS NULL` placeholders; this completes
-- that design by recording *which* run a placeholder belongs to.
--
-- # The autopilot-task discriminator
--
-- An autopilot queue row is identified by `autopilot_run_id IS NOT NULL` (paired
-- with `issue_id IS NULL`) — there is deliberately no separate `kind` column.
-- The link is the minimal carrier of both "this is an autopilot task" and "which
-- run it belongs to", and it is what the finalize cascade follows to stamp
-- `autopilot_run.completed_at` when the task reaches a terminal state.
--
-- # Why ALTER (not a new table)
--
-- `agent_task_queue` already exists (migration 0004); this is a single additive
-- nullable column, so it ALTERs rather than rebuilding the table. PRAGMA
-- foreign_keys is off in this crate, so the REFERENCES clause documents the link
-- for readers and the future Postgres backend but is not engine-enforced here.

ALTER TABLE agent_task_queue
    ADD COLUMN autopilot_run_id TEXT REFERENCES autopilot_run(id);
