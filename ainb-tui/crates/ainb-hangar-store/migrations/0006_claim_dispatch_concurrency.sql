-- Hangar v1 schema, migration 0006: claim-time dispatch stamp + per-agent
-- concurrency cap.
--
-- P1.2 (ClaimTask service) needs two columns the P0 schema did not yet carry:
--
-- 1. `agent_task_queue.dispatched_at` — epoch-ms timestamp stamped when the
--    daemon claims a `queued` row (queued -> dispatched). The P1.4 sweepers key
--    the 90s reclaim window and the 5-minute dispatch TTL off this column, so it
--    is distinct from `created_at` (the queued-at time) and `started_at` (set
--    only once the run actually begins). Nullable: only set from the claim
--    onward. (Mirrors Multica `task.go`'s dispatched_at.)
--
-- 2. `agent.max_concurrent_tasks` — caps how many of an agent's tasks may be
--    `running` at once. The claim path counts the agent's running tasks and
--    refuses to dispatch a new one past this cap (`task.go:761`
--    `CountRunningTasks` parity). Defaults to 1, matching the conservative
--    Multica default for a fresh agent.
--
-- Both are forward-only `ALTER TABLE ADD COLUMN`s: SQLite appends columns
-- cheaply and the defaults keep every existing row valid.

ALTER TABLE agent_task_queue ADD COLUMN dispatched_at INTEGER;

ALTER TABLE agent ADD COLUMN max_concurrent_tasks INTEGER NOT NULL DEFAULT 1;
