-- Hangar v1 schema, migration 0005: auth tokens + beads correlation.
--
-- Completes the 14-table v1 schema. Three independent tables:
--
-- `pat`           — personal access tokens. The raw token is never stored;
--                   `sha256_token` is its SHA-256 hex digest (UNIQUE for O(1)
--                   lookup-by-hash). `scope` is a nullable opaque scope string;
--                   `last_used` is touched on each successful auth (TTL cleanup
--                   is P5's concern). Owned by a `user`.
--
-- `daemon_token`  — per-runtime bearer tokens the daemon presents. Same hashing
--                   discipline as `pat`. Bound to one `agent_runtime`.
--
-- `beads_mapping` — correlates a Hangar entity (`issue` or `task`) with its
--                   mirrored beads (`bd`) id. Composite PK `(hangar_id, bd_id)`
--                   so the same pair is recorded once; `hangar_kind` is CHECK-
--                   constrained to keep the discriminant honest. P2 owns the
--                   sync loop that writes `last_synced`.
--
-- All timestamps are epoch milliseconds stored as INTEGER (see 0001).

CREATE TABLE pat (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES user(id),
    sha256_token TEXT NOT NULL UNIQUE,
    scope        TEXT,
    created_at   INTEGER NOT NULL,
    last_used    INTEGER
);

CREATE TABLE daemon_token (
    id           TEXT PRIMARY KEY,
    sha256_token TEXT NOT NULL UNIQUE,
    runtime_id   TEXT NOT NULL REFERENCES agent_runtime(id),
    created_at   INTEGER NOT NULL
);

CREATE TABLE beads_mapping (
    hangar_id   TEXT NOT NULL,
    bd_id       TEXT NOT NULL,
    hangar_kind TEXT NOT NULL CHECK (hangar_kind IN ('issue','task')),
    bd_kind     TEXT NOT NULL,
    last_synced INTEGER NOT NULL,
    PRIMARY KEY (hangar_id, bd_id)
);
