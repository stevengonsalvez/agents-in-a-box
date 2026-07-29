-- Workload projections advance independently from lifecycle observations.
-- A provider child-work update must not overwrite a newer lifecycle state.

ALTER TABLE fleet_session
    ADD COLUMN workload_updated_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE fleet_session
    ADD COLUMN workload_authority TEXT NOT NULL DEFAULT 'inferred'
        CHECK (workload_authority IN ('authoritative', 'inferred'));

-- One source event can change multiple parent relationships. SQLite cannot drop
-- the table-level UNIQUE constraint in place, so rebuild only this projection.
CREATE TABLE fleet_work_item_next (
    provider       TEXT NOT NULL CHECK (length(provider) > 0),
    session_key    TEXT NOT NULL CHECK (length(session_key) > 0),
    work_key       TEXT NOT NULL CHECK (length(work_key) > 0),
    kind           TEXT NOT NULL CHECK (kind IN ('subagent', 'task', 'child_thread')),
    state          TEXT NOT NULL CHECK (state IN ('ACTIVE', 'COMPLETE')),
    started_at     INTEGER NOT NULL,
    completed_at   INTEGER,
    last_event_id  TEXT NOT NULL CHECK (length(last_event_id) > 0),
    PRIMARY KEY (provider, session_key, work_key)
);
INSERT INTO fleet_work_item_next
    SELECT provider, session_key, work_key, kind, state, started_at, completed_at, last_event_id
    FROM fleet_work_item;
DROP TABLE fleet_work_item;
ALTER TABLE fleet_work_item_next RENAME TO fleet_work_item;
CREATE INDEX idx_fleet_work_item_active ON fleet_work_item(session_key, state);
