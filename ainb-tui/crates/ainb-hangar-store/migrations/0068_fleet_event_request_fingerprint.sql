-- Migration 0068. Persist exact request identity beside its durable event payload. A session's
-- active request fingerprint can outlive unrelated later events, so projection
-- must select the matching request body rather than the latest applied one.
ALTER TABLE fleet_event ADD COLUMN request_fingerprint TEXT;

CREATE INDEX idx_fleet_event_request_fingerprint
    ON fleet_event(session_key, request_fingerprint, revision DESC)
    WHERE applied = 1 AND request_fingerprint IS NOT NULL;
