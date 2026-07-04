-- Hangar v1 schema, migration 0032: task-create parity fields (spec F1-F5).
--
-- Card-create reaches New-Session parity: a card carries a REPO (a local path or
-- the literal `scratch`) and an AGENT (`claude`/`codex`/`copilot`). Those two
-- choices persist on the durable card (= the `issue`) so a reload / rerun keeps
-- them, and flow onto the dispatched task so the runner provisions the right
-- worktree and routes to the right provider. The F4 default cascade adds a
-- board-level and a workspace-level default agent (the global default + last-used
-- live in the generic `daemon_config` KV table, no schema needed).
--
-- Columns:
--   - `issue.repo_ref TEXT` — the card's repo: an absolute checkout path, or the
--     literal `scratch` (spec F2, the auto-created `~/.agents-in-a-box/scratch/
--     <slug>` repo). NULL for pre-0032 issues + non-card issues (they never ran a
--     card launch).
--   - `issue.agent_kind TEXT` — the card's chosen provider, or NULL to resolve via
--     the F4 cascade at run time.
--   - `agent_task_queue.repo_ref TEXT` — the run's repo, copied from the card at
--     dispatch so the worktree/scratch layer sees it without re-reading the issue.
--     NULL = no repo (a chat/autopilot task, the pre-0032 empty-workdir behaviour).
--   - `agent_task_queue.agent_kind TEXT NOT NULL DEFAULT 'claude'` — the RESOLVED
--     provider the run dispatches through (cascade output). Defaults to `claude`
--     so every existing row + every enqueue path that does not opt in is unchanged.
--   - `board.default_agent TEXT` — the F4 board-level default agent, or NULL.
--   - `workspace.default_agent TEXT` — the F4 workspace-level default agent, or NULL.
--
-- ALTER TABLE ... ADD COLUMN with no default (or a constant default) is an O(1)
-- catalog change in SQLite (no table rewrite), so this is safe on populated
-- databases (mirrors 0020 adding nullable config columns to `workspace` and 0031
-- adding a defaulted column to `agent_task_queue`). Every pre-existing row reads
-- back NULL (or `claude` for the task agent) — byte-identical prior behaviour.

ALTER TABLE issue
    ADD COLUMN repo_ref TEXT;

ALTER TABLE issue
    ADD COLUMN agent_kind TEXT;

ALTER TABLE agent_task_queue
    ADD COLUMN repo_ref TEXT;

ALTER TABLE agent_task_queue
    ADD COLUMN agent_kind TEXT NOT NULL DEFAULT 'claude';

ALTER TABLE board
    ADD COLUMN default_agent TEXT;

ALTER TABLE workspace
    ADD COLUMN default_agent TEXT;
