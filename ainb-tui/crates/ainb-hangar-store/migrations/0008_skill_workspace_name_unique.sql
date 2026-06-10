-- Hangar v1 schema, migration 0008: enforce skill name uniqueness per workspace.
--
-- A skill is identified within a workspace by its normalised (lowercase
-- kebab-case) `name`. The P6.1 importer's `upsert_by_name` hot path depends on a
-- single canonical row per `(workspace_id, name)`: re-running a sync must update
-- the existing row, never fork a duplicate. SQLite's `INSERT ... ON CONFLICT`
-- requires a unique index to target, and per-connection `PRAGMA foreign_keys`
-- is off in this crate so app-level uniqueness cannot lean on FK behaviour —
-- this index is the authoritative guard.
--
-- Two workspaces may each own a skill named `commit`; the composite key scopes
-- the constraint to a single tenant, preventing any cross-tenant collision.

CREATE UNIQUE INDEX idx_skill_workspace_name
    ON skill(workspace_id, name);
