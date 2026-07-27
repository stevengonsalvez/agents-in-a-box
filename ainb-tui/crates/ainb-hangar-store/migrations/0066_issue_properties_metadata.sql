-- Hangar v1 schema, migration 0066: CUSTOM PROPERTY CATALOG + per-issue
-- METADATA SCRATCH BAG (multica parity #17 = reference migrations
-- 191_issue_properties + 105_issue_metadata).
--
-- TWO surfaces on purpose, exactly as the reference:
--   * `issue.properties` -- USER-facing typed custom fields, validated against
--     the `issue_property` catalog. Keyed by DEFINITION ID, never by name, so
--     renaming a property is a catalog-only write (zero issue rows touched).
--   * `issue.metadata`   -- AGENT-internal flat KV scratch (pr_number,
--     pipeline_status, waiting_on...). No catalog, primitives only.
-- Collapsing them into one bag would mean either the agent scratch inherits
-- catalog validation (it must not) or the user fields lose it (they must not).
--
-- DECISIONS
-- 1. Two plain `TEXT NOT NULL DEFAULT '{}'` columns, NOT CHECK-constrained.
--    The reference's `jsonb_typeof` / `pg_column_size` CHECKs are Postgres
--    defence-in-depth; in SQLite a column CHECK added by ALTER TABLE can never
--    be widened without a full table rebuild (see 0057's copy/drop/recreate),
--    and 0058/0062 set the precedent of enforcing shape + vocabulary in RUST
--    instead. `ainb_hangar_core::properties` is the only writer and the
--    tolerant reader. ADD COLUMN with a constant default is O(1) in SQLite.
-- 2. `kind` carries NO CHECK, same rule: the reference's seven kinds are an
--    APPEND-ONLY vocabulary (`PropertyKind::as_db_str` writes, `::parse`
--    reads tolerantly, an unknown token renders as raw text).
-- 3. `archived_at` nullable, never a DELETE. The reference archives and never
--    hard-deletes, so an issue's stored value can always be re-resolved.
-- 4. FK on `workspace_id` only; the per-issue values live in a column on
--    `issue`, so they are reaped by the existing `IssueRepo::delete_cascade`
--    with no new cascade step.
-- 5. NO backfill. Both bags start empty on every existing row -- there is no
--    prior data that means "a custom property".

CREATE TABLE issue_property (
    id           TEXT PRIMARY KEY,            -- ULID, minted by the caller's IdGen
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    key          TEXT NOT NULL,               -- stable slug; how CLI/RPC address it
    name         TEXT NOT NULL,               -- display label; renameable, free
    kind         TEXT NOT NULL,               -- PropertyKind::as_db_str()
    options      TEXT NOT NULL DEFAULT '[]',  -- JSON array of option strings
    position     INTEGER NOT NULL DEFAULT 0,  -- render order within the workspace
    archived_at  INTEGER,                     -- NULL = active; never hard-deleted
    created_at   INTEGER NOT NULL             -- epoch millis
);

-- One definition per (workspace, key). The engine-enforced invariant that makes
-- `define` an idempotent resolve-or-update, mirroring idx_label_workspace_name.
CREATE UNIQUE INDEX idx_issue_property_workspace_key
    ON issue_property(workspace_id, key);

-- "the active catalog, in render order" -- the read every list/validate does.
CREATE INDEX idx_issue_property_workspace_active
    ON issue_property(workspace_id, archived_at, position);

-- Per-issue value bag: {"<issue_property.id>": <primitive|array>}.
ALTER TABLE issue ADD COLUMN properties TEXT NOT NULL DEFAULT '{}';

-- Per-issue agent scratch: {"<key>": <string|number|bool>}.
ALTER TABLE issue ADD COLUMN metadata   TEXT NOT NULL DEFAULT '{}';
