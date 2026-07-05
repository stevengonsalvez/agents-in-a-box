-- Hangar v1 schema, migration 0039: the run generation marker (tcp 8ln).
--
-- A card = an issue (§4.5); every Run / rerun / squad fan-out enqueues one or
-- more tasks onto that one issue. Before this column the card-state aggregates
-- folded the issue's ENTIRE task history, so old terminal rows POISONED a rerun:
--   - `issue_aggregate_terminal_state` (the terminal auto-move) reported a stale
--     `failed`/`cancelled` even after a clean rerun succeeded, so a
--     failed-then-rerun-successful card auto-moved to the failed column;
--   - `unfinished_blockers_of` (the blocker-finished gate) kept an old `done`
--     row, so a succeeded-then-rerun-failing blocker stayed "finished" and its
--     dependent unblocked against a now-broken blocker;
--   - the board card chip rendered from the newest single task, not the run's
--     aggregate.
--
-- `generation` scopes those folds to the LATEST run. Every fresh Run / rerun /
-- fan-out of an issue bumps that issue's generation (via
-- `TaskRepo::next_generation_for_issue` = MAX(generation)+1); the leader + all
-- members of one fan-out SHARE a generation (they are one run), and an infra
-- retry child COPIES its parent's generation (a new attempt of the SAME run).
-- The aggregate / blocker-finished / auto-move / card-chip reads then scope to
-- `generation = (SELECT MAX(generation) ... FOR THAT ISSUE)`, so prior-run rows
-- no longer poison the current run's state.
--
--   - `agent_task_queue.generation INTEGER NOT NULL DEFAULT 0` — the run epoch;
--     `0` for every pre-existing row (all folded as one legacy generation) and
--     for issueless chat / autopilot tasks (which no card aggregate reads).
--
-- The composite index keys the MAX(generation) probe + the generation-scoped
-- scans off `(issue_id, generation)`, so the added subqueries stay indexed
-- lookups. ALTER TABLE ... ADD COLUMN with a constant default is an O(1) catalog
-- change in SQLite (no table rewrite), safe on populated databases (mirrors
-- 0031 / 0032 / 0033); every pre-existing row reads back `0`.

ALTER TABLE agent_task_queue
    ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_task_issue_generation
    ON agent_task_queue(issue_id, generation)
    WHERE issue_id IS NOT NULL;
