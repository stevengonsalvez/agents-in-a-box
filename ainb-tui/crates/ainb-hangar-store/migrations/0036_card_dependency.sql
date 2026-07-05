-- Hangar v1 schema, migration 0036: beads-style card dependencies (tcp T4 / F7).
--
-- F7 adds `depends-on` edges between cards. Card = issue (§4.5), so a dependency
-- is a directed edge between two issues in one workspace:
--
--   card_dependency(dependent_issue_id -> blocker_issue_id)
--       "the DEPENDENT card is blocked until the BLOCKER card finishes"
--
-- Semantics (enforced in the service layer, `CardDependencyRepo` + the run
-- handler):
--   - a card with an UNFINISHED blocker refuses to RUN (a clear message) and is
--     never auto-dispatched;
--   - when a blocker completes (its latest task reaches `done`) the finalize seam
--     re-checks each dependent and, once its LAST blocker is done, marks it
--     runnable (an event) and — only if the dependent's `auto_run` flag is on —
--     auto-launches it (respecting the claim-loop concurrency caps);
--   - an edge insert that would create a CYCLE is rejected (a DFS over the
--     existing edges before the write), as is a self-edge.
--
-- # Table shape
--
--   - composite PK `(dependent_issue_id, blocker_issue_id)` — a card depends on
--     another at most once (a re-add is an idempotent no-op, mirroring
--     `board_card`'s `ON CONFLICT`).
--   - `workspace_id` scopes every read/write to one tenant (a dependency never
--     crosses a workspace boundary; both endpoints live in the same workspace,
--     the service checks it).
--   - NO foreign key on either issue id: `PRAGMA foreign_keys` is off in this
--     crate (see 0002/0003), and the endpoints live in the `issue` table under
--     the same workspace — referential integrity is a service-layer concern (the
--     repo resolves both endpoints WITHIN the workspace before inserting), exactly
--     as `board_card.issue_id` is. The composite PK is the engine-enforced
--     invariant.
--   - `idx_card_dependency_blocker` serves the finalize-seam reverse lookup
--     ("which cards depend on the blocker that just finished?").
--
-- # Auto-run flag
--
-- `issue.auto_run` (per-card, default OFF) governs whether a card auto-launches
-- the instant its last blocker completes. Default OFF keeps EXPLICIT run the
-- default — a card only auto-runs when the user opts it in. A card with no
-- blockers ignores the flag (nothing to auto-run off).
--
-- `CREATE TABLE` / `CREATE INDEX` and `ALTER TABLE ... ADD COLUMN` with a constant
-- default are catalog-only changes (no table rewrite), so this is safe + O(1) on a
-- populated database: every pre-existing issue reads back `auto_run = 0` and the
-- workspace simply owns no dependency edges until one is created.

CREATE TABLE card_dependency (
    workspace_id       TEXT NOT NULL REFERENCES workspace(id),
    dependent_issue_id TEXT NOT NULL,
    blocker_issue_id   TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    PRIMARY KEY (dependent_issue_id, blocker_issue_id)
);

-- Reverse lookup for the finalize seam: given a blocker that just completed, find
-- every card that depends on it (so their runnable state can be re-evaluated).
CREATE INDEX idx_card_dependency_blocker ON card_dependency(blocker_issue_id);

ALTER TABLE issue
    ADD COLUMN auto_run INTEGER NOT NULL DEFAULT 0;
