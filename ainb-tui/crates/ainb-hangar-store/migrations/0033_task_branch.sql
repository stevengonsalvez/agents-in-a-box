-- Hangar v1 schema, migration 0033: the run's worktree branch (tcp T2).
--
-- A card run on a real repo executes in a volatile git worktree on branch
-- `ainb/<slug>` (migration 0032 + the F5 provisioner). When the run leaves
-- commits, `git worktree remove` (teardown) keeps that branch in the origin
-- repo — the durable artifact a reviewer inspects. This column records that
-- branch onto the task row so the board card + task detail can surface it
-- WITHOUT a git query at render time.
--
-- Written at finalize ONLY when the run produced commits (branch ahead of its
-- base > 0), so a NULL `branch` distinguishes "made no commits, nothing to
-- show" from "committed — here is the branch". Chat / autopilot / scratch /
-- in-tree runs never set it. A retry child starts NULL (a fresh run mints a
-- fresh worktree/branch and records its own), so the column is deliberately NOT
-- copied by the retry INSERT.
--
--   - `agent_task_queue.branch TEXT` — the `ainb/<slug>` branch the run committed
--     on, or NULL (no commits / not a worktree run).
--
-- ALTER TABLE ... ADD COLUMN with no default is an O(1) catalog change in
-- SQLite (no table rewrite), safe on populated databases (mirrors 0031 / 0032).
-- Every pre-existing row reads back NULL — byte-identical prior behaviour.

ALTER TABLE agent_task_queue
    ADD COLUMN branch TEXT;
