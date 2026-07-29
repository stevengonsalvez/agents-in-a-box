-- Durable provider-event source ledger. fleet_event remains the normalized
-- projection timeline; this table keeps every raw provider envelope so a failed
-- or upgraded reducer can replay without losing source fidelity.

CREATE TABLE fleet_provider_event (
    ingest_order        INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id            TEXT NOT NULL UNIQUE CHECK (length(event_id) > 0),
    provider            TEXT NOT NULL CHECK (length(provider) > 0),
    source              TEXT NOT NULL CHECK (length(source) > 0),
    session_key         TEXT,
    provider_session_id TEXT,
    observed_at         INTEGER NOT NULL,
    received_at         INTEGER NOT NULL,
    event_type          TEXT NOT NULL CHECK (length(event_type) > 0),
    raw_payload         TEXT NOT NULL,
    raw_blake3          TEXT NOT NULL CHECK (length(raw_blake3) = 64),
    projection_revision INTEGER REFERENCES fleet_event(revision)
);

CREATE INDEX idx_fleet_provider_event_session_order
    ON fleet_provider_event(session_key, ingest_order);
CREATE INDEX idx_fleet_provider_event_projection
    ON fleet_provider_event(projection_revision)
    WHERE projection_revision IS NULL;
