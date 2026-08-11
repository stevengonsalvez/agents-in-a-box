-- A remote client creates its thread inside the shared app-server process,
-- so `thread/started.cwd` is that process directory, not the session worktree.
-- There is no caller correlation token. Serialize fresh launches globally on
-- Ainb's private endpoint, then claim the first event after its cursor.
DROP INDEX IF EXISTS interactive_codex_thread_one_pending_cwd;
CREATE UNIQUE INDEX interactive_codex_thread_one_pending_launch
    ON interactive_codex_thread(1)
    WHERE thread_id IS NULL;
