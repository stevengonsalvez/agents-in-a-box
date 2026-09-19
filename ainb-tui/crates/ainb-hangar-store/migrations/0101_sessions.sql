-- Hangar v1 schema, migration 0101: the sessions table (P6d, base spec :71, :286, :318).
--
-- Replaces ~/.agents-in-a-box/sessions.json with a daemon-owned SQLite table
-- behind RPC, with a one-time idempotent import at boot.
--
-- Carries all thirteen fields of SessionMetadata (session_manager.rs:94-124).

CREATE TABLE sessions (
    session_id          TEXT PRIMARY KEY NOT NULL,
    tmux_session_name   TEXT NOT NULL,
    worktree_path       TEXT NOT NULL,
    workspace_name      TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    agent_type          TEXT NOT NULL,
    headroom_enabled    INTEGER NOT NULL DEFAULT 0 CHECK (headroom_enabled IN (0, 1)),
    rtk_enabled         INTEGER NOT NULL DEFAULT 0 CHECK (rtk_enabled IN (0, 1)),
    skip_permissions    INTEGER CHECK (skip_permissions IS NULL OR skip_permissions IN (0, 1)),
    model               TEXT,
    model_source        TEXT NOT NULL DEFAULT 'LegacyTyped',
    codex_model         TEXT,
    codex_thread_id     TEXT
);

CREATE UNIQUE INDEX idx_sessions_tmux ON sessions(tmux_session_name);
CREATE INDEX idx_sessions_workspace ON sessions(workspace_name);
