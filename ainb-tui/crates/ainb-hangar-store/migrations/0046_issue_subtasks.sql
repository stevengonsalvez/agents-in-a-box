-- 0046: sub-issues. A `parent_issue_id` self-link groups child issues under a
-- parent; completing the last child of the lowest unfinished `stage` cascades a
-- system-style comment onto the parent (and wakes an agent/squad parent). NULL
-- parent = a top-level issue; NULL stage = unstaged (one implicit stage).
--
-- Self-FK ON DELETE SET NULL: deleting a parent orphans its children (their
-- parent_issue_id goes NULL) rather than blocking the delete — parity with
-- multica 001_init. ADD COLUMN with a NULL-default REFERENCES clause is an O(1)
-- catalog change (no table rewrite), safe on populated DBs (mirrors 0043).
ALTER TABLE issue ADD COLUMN parent_issue_id TEXT REFERENCES issue(id) ON DELETE SET NULL;

-- Stage ordinal: 1-based barrier groups among siblings sharing a parent. NULL =
-- unstaged. CHECK keeps the "unstaged" sentinel unambiguous (mirrors mig 123).
ALTER TABLE issue ADD COLUMN stage INTEGER CHECK (stage IS NULL OR stage >= 1);

-- Child lookups + the roll-up count both scan by parent.
CREATE INDEX idx_issue_parent ON issue(parent_issue_id);
