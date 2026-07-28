-- Hangar v1 schema, migration 0060: the inbox becomes ACTOR-POLYMORPHIC
-- (multica parity #1-rest).
--
-- Migration 0021 made `inbox_entry` workspace-scoped: every actor on a
-- workspace read the identical list, so a human "member" was a name on an
-- assignee, never a collaborator with an inbox of their own. The reference
-- (`inbox_item`, multica 001_init.up.sql:110) addresses every notification to
-- exactly one actor -- `recipient_type IN ('member','agent')` + `recipient_id`
-- -- and every read and every mark-read sweep is keyed on that pair.
--
--   - `recipient_type`  the actor family the entry is FOR: `member` or `agent`.
--                       CHECK-constrained like every other polymorphic actor
--                       column (0003's assignee/creator/author).
--   - `recipient_id`    the member's `user.id` or the agent's `agent.id`. No FK:
--                       the id may live in either table and SQLite cannot express
--                       a conditional FK (same rationale as 0003).
--
-- Both are NOT NULL with a constant DEFAULT so the ADD COLUMN backfills every
-- pre-existing row in one O(1) catalog change: legacy workspace-wide entries
-- become the LOCAL HUMAN's entries (`member:me`, the ref the TUI authors
-- everything as), so no upgrading install loses a notification. Re-applying is
-- a no-op via the migrator's version ledger.

ALTER TABLE inbox_entry ADD COLUMN recipient_type TEXT NOT NULL DEFAULT 'member'
    CHECK (recipient_type IN ('member','agent'));
ALTER TABLE inbox_entry ADD COLUMN recipient_id TEXT NOT NULL DEFAULT 'me';

-- Every read is now (workspace, recipient) + newest-first; this index replaces
-- 0021's workspace-only ordering index as the list's covering path.
CREATE INDEX idx_inbox_entry_recipient_created
    ON inbox_entry(workspace_id, recipient_type, recipient_id, created_at);

-- Partial index over just the unread rows for the per-recipient badge count.
CREATE INDEX idx_inbox_entry_recipient_unread
    ON inbox_entry(workspace_id, recipient_type, recipient_id)
    WHERE read_at IS NULL;
