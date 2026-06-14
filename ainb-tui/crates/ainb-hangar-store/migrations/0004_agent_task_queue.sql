-- Hangar v1 schema, migration 0004: the agent task queue.
--
-- `agent_task_queue` is the durable work queue the daemon FSM (P1) drives: one
-- row per dispatched unit of agent work. A task always binds to a workspace, a
-- runtime, and an agent; it MAY bind to an issue (`issue_id` is nullable —
-- chat / autopilot tasks at v1 carry no issue and use the placeholder NULL).
--
-- `status` walks the lifecycle queued -> dispatched -> running ->
-- done|failed|cancelled. The terminal set is {done, failed, cancelled}; the
-- pending (non-terminal-and-not-yet-running) set is {queued, dispatched}.
-- P0 only writes the initial `queued` row; the typed state transitions land in
-- P1 (hangar:P1.x).
--
-- Retry bookkeeping: `attempt` / `max_attempts` cap re-dispatch; `parent_task_id`
-- is a self-FK chaining a retry back to the task it replaces; `failure_reason`
-- captures the last failure for the UI.
--
-- `result` is a JSON blob (the agent's structured output); `session_id` and
-- `work_dir` record where the run executed. All timestamps are epoch
-- milliseconds stored as INTEGER (see 0001).

CREATE TABLE agent_task_queue (
    id             TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL REFERENCES workspace(id),
    runtime_id     TEXT NOT NULL REFERENCES agent_runtime(id),
    agent_id       TEXT NOT NULL REFERENCES agent(id),
    issue_id       TEXT REFERENCES issue(id),
    status         TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','dispatched','running','done','failed','cancelled')),
    result         TEXT,
    session_id     TEXT,
    work_dir       TEXT,
    attempt        INTEGER NOT NULL DEFAULT 1,
    max_attempts   INTEGER NOT NULL DEFAULT 2,
    parent_task_id TEXT REFERENCES agent_task_queue(id),
    failure_reason TEXT,
    created_at     INTEGER NOT NULL,
    started_at     INTEGER,
    finished_at    INTEGER
);

-- Coalesce duplicate fires: at most one *pending* (queued or dispatched) task
-- per issue. The predicate is partial — rows that are terminal/running, or that
-- have no issue (issue_id IS NULL: chat / autopilot placeholders), are excluded
-- and therefore never collide. P1's enqueue path relies on this to deduplicate.
-- (Mirrors the reference's 022_task_lifecycle_guards.up.sql; the WHERE form is portable
-- to the future Postgres backend byte-for-byte.)
CREATE UNIQUE INDEX idx_one_pending_task_per_issue
    ON agent_task_queue(issue_id)
    WHERE issue_id IS NOT NULL AND status IN ('queued','dispatched');
