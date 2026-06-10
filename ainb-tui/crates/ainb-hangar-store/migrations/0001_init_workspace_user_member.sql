-- Hangar v1 schema, migration 0001: tenancy primitives.
--
-- Multi-tenancy is a day-1 primitive (build-plan round 1): `workspace` is the
-- root of every later table's `workspace_id` FK. `user` + `member` model the
-- single-user-at-v1 reality without a refactor cost when multi-user lands.
--
-- All timestamps are epoch milliseconds stored as INTEGER (SQLite has no native
-- temporal type; epoch-ms keeps us Postgres-portable via `to_timestamp/1000`).

CREATE TABLE workspace (
    id         TEXT PRIMARY KEY,
    slug       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE user (
    id         TEXT PRIMARY KEY,
    email      TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE member (
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    user_id      TEXT NOT NULL REFERENCES user(id),
    role         TEXT NOT NULL CHECK (role IN ('owner','admin','member')),
    PRIMARY KEY (workspace_id, user_id)
);
