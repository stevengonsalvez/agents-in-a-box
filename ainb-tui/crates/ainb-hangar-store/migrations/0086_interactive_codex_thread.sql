-- Exact remote Codex thread identity for Interactive sessions.
--
-- A NULL thread_id is a durable allocation reservation. If the daemon dies
-- after thread/start but before this row is completed, retrying must fail
-- closed rather than minting a duplicate cloud-visible thread.
CREATE TABLE interactive_codex_thread (
    session_id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT,
    cwd TEXT NOT NULL,
    model TEXT,
    skip_permissions INTEGER NOT NULL DEFAULT 0
);
