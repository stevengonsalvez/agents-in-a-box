-- Durable child work. Parent session projection carries active_work_count.

ALTER TABLE fleet_session
    ADD COLUMN active_work_count INTEGER NOT NULL DEFAULT 0 CHECK (active_work_count >= 0);

CREATE TABLE fleet_work_item (
    provider       TEXT NOT NULL CHECK (length(provider) > 0),
    session_key    TEXT NOT NULL CHECK (length(session_key) > 0),
    work_key       TEXT NOT NULL CHECK (length(work_key) > 0),
    kind           TEXT NOT NULL CHECK (kind IN ('subagent', 'task', 'child_thread')),
    state          TEXT NOT NULL CHECK (state IN ('ACTIVE', 'COMPLETE')),
    started_at     INTEGER NOT NULL,
    completed_at   INTEGER,
    last_event_id  TEXT NOT NULL UNIQUE CHECK (length(last_event_id) > 0),
    PRIMARY KEY (provider, session_key, work_key)
);

CREATE INDEX idx_fleet_work_item_active
    ON fleet_work_item(session_key, state);
