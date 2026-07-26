-- 0049: blocked + cancelled issue states (multica gap #19).
--
-- Multica's `issue.status` carries a 7-value CHECK
-- (001_init.up.sql:58): backlog|todo|in_progress|in_review|done|blocked|cancelled.
-- Hangar's `state` (mig 0003) is free TEXT with no constraint at all, and the
-- vocabulary stopped at five. This migration adds the two missing tokens to the
-- ACCEPTED set and, for the first time, enforces the set at the database.
--
-- WHY TRIGGERS, NOT A CHECK: SQLite has no `ALTER TABLE ... ADD CONSTRAINT`.
-- Adding a real CHECK means a 12-step table rebuild of `issue`, which now carries
-- a self-FK (parent_issue_id, mig 0046) plus inbound references. A BEFORE-write
-- trigger with RAISE(ABORT) rejects exactly the same writes at O(1) catalog cost
-- and touches zero existing rows.
--
-- WHY `open`/`closed` ARE STILL ADMITTED: the Beads bridge writes them
-- (beads_sync maps unknown bd statuses to `open`) and the CLI's
-- DEFAULT_ISSUE_STATE is still `open`. `IssueLifecycle::for_state` maps both
-- forward for display (open -> Todo, closed -> Done), so they are legal, not
-- garbage. Tightening the list to the 7 canonical tokens is a FOLLOW-UP gated on
-- the Beads adapter + CLI emitting canonical tokens; the constraint's job HERE is
-- to make a typo ('done ', 'cancelled_', 'blockd') fail loudly at the write
-- instead of silently minting a row that falls into Todo forever.
--
-- No column is added, so no backfill and no index change.

CREATE TRIGGER issue_state_vocab_insert
BEFORE INSERT ON issue
WHEN NEW.state NOT IN (
    'backlog','todo','in_progress','in_review','done','blocked','cancelled',
    'open','closed'
)
BEGIN
    SELECT RAISE(ABORT, 'invalid issue.state: expected one of backlog|todo|in_progress|in_review|done|blocked|cancelled (legacy open|closed tolerated)');
END;

CREATE TRIGGER issue_state_vocab_update
BEFORE UPDATE OF state ON issue
WHEN NEW.state NOT IN (
    'backlog','todo','in_progress','in_review','done','blocked','cancelled',
    'open','closed'
)
BEGIN
    SELECT RAISE(ABORT, 'invalid issue.state: expected one of backlog|todo|in_progress|in_review|done|blocked|cancelled (legacy open|closed tolerated)');
END;
