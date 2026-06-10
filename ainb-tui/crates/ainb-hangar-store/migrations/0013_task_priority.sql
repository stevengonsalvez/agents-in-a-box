-- Hangar v1 schema, migration 0013: task priority.
--
-- Adds `priority` to `agent_task_queue` so urgent work can jump the queue —
-- Multica's claim orders `priority DESC, created_at` while Hangar's was strict
-- FIFO (`created_at, id` only), with no expedite path (parity-review design
-- gap: "Claim has no priority dimension").
--
-- Scale: an integer 0..3 where HIGHER = MORE URGENT, mapping onto the
-- conventional P-levels in reverse:
--
--   priority 0  =  P3  (default — routine work, drained FIFO)
--   priority 1  =  P2
--   priority 2  =  P1
--   priority 3  =  P0  (most urgent — claimed first)
--
-- The default of 0 (P3) keeps every existing row and every enqueue path that
-- does not opt in behaving exactly as before: among equal priorities the claim
-- remains FIFO (`created_at, id` tiebreak). The claim path (CLAIM_SQL in
-- ainb-hangar-store `service/claim.rs`) orders
-- `ORDER BY priority DESC, created_at, id`. Decision recorded in
-- docs/hangar/architecture.md.
--
-- ALTER TABLE ... ADD COLUMN with a constant default is an O(1) catalog change
-- in SQLite (no table rewrite), so this is safe on populated databases.

ALTER TABLE agent_task_queue
    ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
