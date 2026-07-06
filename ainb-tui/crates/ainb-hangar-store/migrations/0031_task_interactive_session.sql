-- Hangar v1 schema, migration 0031: interactive-mode launch + tmux session name.
--
-- ccc / D6: a board card can launch in `headless` (`claude -p` / `codex exec`) or
-- `interactive` (a REAL, attachable tmux session per task) mode. The runner needs
-- to know which mode a claimed task wants, and — for interactive — to record the
-- exact tmux session name it spawned so an attach-from-card affordance can surface
-- a copyable `tmux attach -t <name>` and the card can prove the session is live.
--
-- Two columns land on `agent_task_queue`:
--   - `mode TEXT NOT NULL DEFAULT 'headless'
--      CHECK (mode IN ('headless', 'interactive'))` — the launch mode the D6
--     `Run ▾` menu picked; defaults to the pre-0031 behaviour (`headless`), so
--     every existing row and every enqueue path that does not opt in keeps
--     dispatching through the unchanged headless provider path;
--   - `session_name TEXT` — the exact tmux session name the interactive runner
--     spawned (`tmux_hangar-<task_id>`), NULL for a headless task or an
--     interactive task not yet dispatched. Recorded on the row the moment the
--     session is created so the attach affordance can reach it mid-run.
--
-- ALTER TABLE ... ADD COLUMN with a constant default is an O(1) catalog change in
-- SQLite (no table rewrite), so this is safe on populated databases (mirrors 0019
-- adding CHECK-constrained columns to `autopilot`).

ALTER TABLE agent_task_queue
    ADD COLUMN mode TEXT NOT NULL DEFAULT 'headless'
        CHECK (mode IN ('headless', 'interactive'));

ALTER TABLE agent_task_queue
    ADD COLUMN session_name TEXT;
