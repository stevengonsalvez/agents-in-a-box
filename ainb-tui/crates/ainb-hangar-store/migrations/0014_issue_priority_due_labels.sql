-- Hangar v1 schema, migration 0014: issue priority, due date, and labels.
--
-- The issue create flow only persisted title/description/state/assignee
-- (parity-review design gap: "create-issue partial — no priority/dates/
-- labels"). This adds the three attributes a created issue needs to carry:
--
--   - `priority`  urgency, 0..3 where HIGHER = MORE URGENT, mapping onto the
--                 conventional P-levels in reverse (the SAME scale as
--                 `agent_task_queue.priority`, migration 0013):
--                   priority 0  =  P3  (default — routine)
--                   priority 1  =  P2
--                   priority 2  =  P1
--                   priority 3  =  P0  (most urgent)
--                 The default of 0 (P3) keeps every existing issue unchanged.
--
--   - `due_date`  an optional deadline as epoch milliseconds (INTEGER, see
--                 0001 for the timestamp convention). NULL = no due date, the
--                 default for every existing and non-opting issue.
--
--   - `labels`    a JSON array of label strings (e.g. `["bug","p1"]`),
--                 defaulting to the empty array `'[]'`. This is the MINIMAL
--                 persistence the create flow needs — the full labels table
--                 with attach/detach RPC + chips is a SEPARATE later bead
--                 (e38.10). Storing the labels create supplies as a single
--                 JSON column avoids a junction table here while keeping the
--                 created-issue snapshot complete.
--
-- ALTER TABLE ... ADD COLUMN with a constant default is an O(1) catalog change
-- in SQLite (no table rewrite), so this is safe on populated databases. Each
-- pre-existing row reads back the column default until explicitly updated.

ALTER TABLE issue
    ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;

ALTER TABLE issue
    ADD COLUMN due_date INTEGER;

ALTER TABLE issue
    ADD COLUMN labels TEXT NOT NULL DEFAULT '[]';
