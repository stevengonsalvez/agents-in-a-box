-- Hangar migration 0043: authoritative host Fleet read model and change log.
--
-- fleet_session is current canonical state for every managed or discovered
-- provider session. Identity is session_key, never cwd. Each independently
-- changing state group carries its own authority and observation timestamp so a
-- tmux inference cannot replace a hook or provider-authoritative value.
--
-- fleet_event is global revision order for snapshot plus delta delivery. The
-- unique event_id makes hook replay and daemon restart idempotent. Revisions are
-- never reused, giving every subscriber one durable resume cursor.

CREATE TABLE fleet_session (
    session_key               TEXT PRIMARY KEY CHECK (length(session_key) > 0),
    provider                  TEXT NOT NULL DEFAULT 'unknown',
    provider_session_id       TEXT,
    tmux_target               TEXT,
    process_start_fingerprint TEXT,
    cwd                       TEXT NOT NULL DEFAULT '',
    display_name              TEXT,
    lifecycle_state           TEXT NOT NULL DEFAULT 'UNKNOWN' CHECK (lifecycle_state IN (
                                  'STARTING', 'RUNNING', 'TURN_COMPLETE', 'IDLE',
                                  'EXITED', 'UNKNOWN'
                              )),
    attention_state           TEXT NOT NULL DEFAULT 'NONE' CHECK (attention_state IN (
                                  'NONE', 'ASK', 'APPROVAL', 'WAITING', 'ERROR'
                              )),
    current_request_fingerprint TEXT,
    management_state          TEXT NOT NULL DEFAULT 'DEGRADED' CHECK (management_state IN (
                                  'MANAGED', 'DEGRADED'
                              )),
    transport_health          TEXT NOT NULL DEFAULT 'UNKNOWN' CHECK (transport_health IN (
                                  'HEALTHY', 'DEGRADED', 'UNAVAILABLE', 'UNKNOWN'
                              )),
    capabilities              TEXT NOT NULL DEFAULT '{}',
    provenance                TEXT NOT NULL DEFAULT 'inferred' CHECK (provenance IN (
                                  'authoritative', 'inferred'
                              )),
    confidence                TEXT NOT NULL DEFAULT 'LOW' CHECK (confidence IN (
                                  'HIGH', 'MEDIUM', 'LOW'
                              )),
    discovered_at             INTEGER NOT NULL,
    last_observed_at          INTEGER NOT NULL,
    metadata_updated_at       INTEGER NOT NULL DEFAULT 0,
    metadata_authority        TEXT NOT NULL DEFAULT 'inferred' CHECK (metadata_authority IN (
                                  'authoritative', 'inferred'
                              )),
    lifecycle_updated_at      INTEGER NOT NULL DEFAULT 0,
    lifecycle_authority       TEXT NOT NULL DEFAULT 'inferred' CHECK (lifecycle_authority IN (
                                  'authoritative', 'inferred'
                              )),
    attention_updated_at      INTEGER NOT NULL DEFAULT 0,
    attention_authority       TEXT NOT NULL DEFAULT 'inferred' CHECK (attention_authority IN (
                                  'authoritative', 'inferred'
                              )),
    transport_updated_at      INTEGER NOT NULL DEFAULT 0,
    transport_authority       TEXT NOT NULL DEFAULT 'inferred' CHECK (transport_authority IN (
                                  'authoritative', 'inferred'
                              )),
    version                   INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_revision          INTEGER NOT NULL DEFAULT 0 CHECK (updated_revision >= 0),
    superseded_by             TEXT REFERENCES fleet_session(session_key),
    visible                   INTEGER NOT NULL DEFAULT 1 CHECK (visible IN (0, 1))
);

CREATE INDEX idx_fleet_session_provider_id
    ON fleet_session(provider, provider_session_id);
CREATE INDEX idx_fleet_session_tmux
    ON fleet_session(tmux_target);

CREATE TABLE fleet_event (
    revision        INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id        TEXT NOT NULL UNIQUE CHECK (length(event_id) > 0),
    session_key     TEXT NOT NULL REFERENCES fleet_session(session_key),
    observed_at     INTEGER NOT NULL,
    authority       TEXT NOT NULL CHECK (authority IN ('authoritative', 'inferred')),
    event_type      TEXT NOT NULL CHECK (length(event_type) > 0),
    payload         TEXT NOT NULL DEFAULT '{}',
    session_version INTEGER NOT NULL CHECK (session_version >= 1),
    applied         INTEGER NOT NULL DEFAULT 1 CHECK (applied IN (0, 1))
);

CREATE INDEX idx_fleet_event_session_revision
    ON fleet_event(session_key, revision);

-- Action receipts are durable, honest delivery outcomes. request_id is the
-- idempotency boundary for a single target action. Broadcast callers mint one
-- request_id per target and share their own idempotency_key in protocol state.
CREATE TABLE fleet_action_receipt (
    request_id         TEXT PRIMARY KEY CHECK (length(request_id) > 0),
    session_key        TEXT NOT NULL,
    action_kind        TEXT NOT NULL,
    action_fingerprint TEXT NOT NULL CHECK (length(action_fingerprint) > 0),
    expected_version   INTEGER NOT NULL CHECK (expected_version >= 1),
    idempotency_key    TEXT,
    status             TEXT NOT NULL CHECK (status IN (
                           'PENDING', 'DELIVERED', 'FAILED', 'UNKNOWN', 'REJECTED'
                       )),
    detail             TEXT,
    session_version    INTEGER,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);

CREATE INDEX idx_fleet_action_receipt_session
    ON fleet_action_receipt(session_key, updated_at DESC);
