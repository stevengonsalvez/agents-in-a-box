-- Hangar v1 schema, migration 0079: chat bus (buzz-port part 1).
--
-- Three new tables plus one index recreation:
--   * fleet_message           - persisted chat messages, one commit-ordered stream
--   * fleet_message_delivery  - one receipt row per (message, recipient)
--   * fleet_acp_session       - ACP session identity adjunct to fleet_session
-- and idx_fleet_provider_event_projection is re-keyed so ACP transcript rows
-- (which land in fleet_provider_event with source='acp') stay out of the
-- pending-recovery scan's way.

-- Chat messages. Scope is a minted string: "session:<key>", "broadcast:<ulid>";
-- part 2 mints "channel:<id>" without schema change.
--
-- `seq` is the ONE cursor for the message stream. SQLite assigns it inside the
-- write transaction and serialises writers, so seq order IS commit order and a
-- page-to-head forwarder cannot skip a row. `id` is the stable external
-- identity used by the wire, by threading, and by clients; it is NEVER a
-- cursor. AUTOINCREMENT (not bare rowid) so a deleted tail cannot hand the
-- same seq to a later row, matching fleet_provider_event.ingest_order.
--
-- `request_fingerprint` is the replay guard: a reused `request_id` whose
-- stored fingerprint differs is REJECTED, mirroring the fleet action/start
-- receipt contract, never silently absorbed.
CREATE TABLE fleet_message (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,  -- commit-ordered cursor
    id          TEXT NOT NULL UNIQUE,               -- daemon-minted ULID, stable external identity
    request_id  TEXT UNIQUE,                        -- client idempotency token (NULL for daemon-authored rows)
    request_fingerprint TEXT,                       -- stable hash of (scope_key, targets, body)
    scope_key   TEXT NOT NULL CHECK (length(scope_key) > 0),
    origin_message_id TEXT REFERENCES fleet_message(id),  -- replies only: the message this row answers (thread join)
    sender      TEXT NOT NULL,                      -- "operator" | session_key
    kind        TEXT NOT NULL CHECK (kind IN ('user','agent','marker')),
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX idx_fleet_message_scope ON fleet_message(scope_key, seq);
CREATE INDEX idx_fleet_message_origin ON fleet_message(origin_message_id, seq)
    WHERE origin_message_id IS NOT NULL;
-- Outbound half of the resume re-prime delivery join (sender = session_key).
CREATE INDEX idx_fleet_message_sender ON fleet_message(sender, seq);

-- One row per (message, recipient): the delivery join broadcast receipts
-- require. States mirror the existing broadcast receipt vocabulary (fleet.rs).
CREATE TABLE fleet_message_delivery (
    message_id  TEXT NOT NULL REFERENCES fleet_message(id),
    session_key TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('PENDING','DELIVERED','FAILED','UNKNOWN','REJECTED')),
    fingerprint TEXT,                          -- receipt-claim fingerprint (single-winner resolve)
    detail      TEXT,                          -- incl. resume-path fingerprint (loaded|reprimed)
    resolved_at INTEGER,
    PRIMARY KEY (message_id, session_key)
);
-- Inbound half of the resume re-prime delivery join, and every per-session
-- receipt query. Without this, a session_key lookup scans the whole PK index
-- because the PK leads with message_id.
CREATE INDEX idx_fleet_message_delivery_session
    ON fleet_message_delivery(session_key, message_id);
-- Boot and runtime convergence scan for stuck legs.
CREATE INDEX idx_fleet_message_delivery_pending
    ON fleet_message_delivery(session_key) WHERE state = 'PENDING';

-- ACP session identity. session_key is daemon-minted and STABLE
-- ('acp:' || ulid); acp_session_id is the adapter's MUTABLE id, swapped on
-- rebuild. Every ACP session also gets a fleet_session row under the SAME
-- session_key (the fleet's one session identity); this table is the
-- ACP-specific adjunct only.
CREATE TABLE fleet_acp_session (
    session_key    TEXT PRIMARY KEY,
    scope_key      TEXT NOT NULL,
    provider       TEXT NOT NULL CHECK (length(provider) > 0),  -- adapter token; validated against the adapter registry at the RPC layer, NOT the schema (0071 `source` style), so the next adapter needs no migration
    provider_version TEXT,                     -- agentInfo version at the last successful initialize; NULL until first spawn
    acp_session_id TEXT,                       -- NULL until session/new succeeds
    cwd            TEXT NOT NULL,
    permission_mode TEXT NOT NULL,             -- the mode PINNED at session/new and re-asserted after load; never inherited
    state          TEXT NOT NULL CHECK (state IN ('ACTIVE','IDLE','EVICTED','DEAD')),
    open_turn_id   TEXT,                       -- non-NULL while a turn is in flight (convergence input)
    open_turn_started_at INTEGER,              -- turn deadline input
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_fleet_acp_session_scope_active
    ON fleet_acp_session(scope_key) WHERE state IN ('ACTIVE','IDLE');
CREATE INDEX idx_fleet_acp_session_open_turn
    ON fleet_acp_session(open_turn_id) WHERE open_turn_id IS NOT NULL;

-- Keep the pending-recovery contract's PREDICATE identical so the existing
-- consumer query can still use the index, and push ACP rows out of its way
-- with the KEY instead. The only reader is FleetProviderEventRepo::unprojected,
-- `WHERE provider = ? AND source = ? AND projection_revision IS NULL`. SQLite
-- may only use a partial index when the query's WHERE provably implies the
-- index's WHERE; `source = ?` is a bound parameter, so a `source <> 'acp'`
-- predicate would NOT be provable at plan time and a source-scoped index would
-- simply never be chosen, degrading the recovery scan to a full table scan
-- over the table ACP transcripts are about to inflate.
DROP INDEX idx_fleet_provider_event_projection;
CREATE INDEX idx_fleet_provider_event_projection
    ON fleet_provider_event(source, provider, projection_revision)
    WHERE projection_revision IS NULL;
