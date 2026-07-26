-- Hangar v1 schema, migration 0055: TYPED card dependency links (multica parity #20).
--
-- multica's `issue_dependency` carries `type IN ('blocks','blocked_by','related')`
-- (server/migrations/001_init.up.sql:88-94) — schema only, it ships no code over
-- that table. hangar's `card_dependency` row is already the `blocked_by` relation
-- (dependent -> blocker), with gating, a DFS cycle guard, and auto-run on top.
-- This migration adds the KIND dimension so `related` (a non-gating association)
-- can be recorded alongside it.
--
-- Domain AT REST: {'blocked_by','related'}. `blocks` is the SAME edge read from
-- the other end and is normalised at write into a swapped `blocked_by` row, so one
-- logical relation can never be stored two ways. (No CHECK constraint: SQLite
-- cannot add one via ALTER, and this crate keeps PRAGMA foreign_keys off — the
-- domain is enforced in `CardDependencyRepo` and asserted by a store test, exactly
-- as endpoint existence and workspace scoping already are for 0036.)
--
-- The composite PK `(dependent_issue_id, blocker_issue_id)` from 0036 is
-- UNCHANGED, so an ordered pair holds at most one relation: a pair is EITHER
-- gating OR related, never both. Re-adding a pair with a different kind replaces
-- the kind (`ON CONFLICT ... DO UPDATE SET link_type = excluded.link_type`).
--
-- BACKFILL IS THE DEFAULT: every pre-0055 row IS a blocked_by edge, so the
-- constant DEFAULT 'blocked_by' reproduces today's semantics for existing data
-- with no rewrite (ADD COLUMN with a constant default is catalog-only, O(1) on a
-- populated database).

ALTER TABLE card_dependency
    ADD COLUMN link_type TEXT NOT NULL DEFAULT 'blocked_by';

-- Serves the per-workspace typed graph read (`links_of_workspace`) and lets the
-- gating queries skip 'related' rows without a scan.
CREATE INDEX idx_card_dependency_ws_type ON card_dependency(workspace_id, link_type);
