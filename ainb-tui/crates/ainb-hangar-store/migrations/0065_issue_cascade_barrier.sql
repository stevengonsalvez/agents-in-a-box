-- 0065: CHILD-DONE BARRIER CLAIM LEDGER (multica parity #3-rest, MUL-4155).
--
-- One row per (parent, stage barrier) that has ALREADY produced a cascade
-- comment. The UNIQUE PK is the whole mechanism: a barrier can be claimed
-- exactly once, so N sibling completions that close the same barrier -- in one
-- batch, or concurrently from two agent-task completions -- collapse to ONE
-- aggregated comment on the parent instead of N.
--
-- stage_key encodes the barrier identity, NOT just the stage ordinal:
--   'unstaged:{n}'   the implicit single stage of an unstaged sibling set of n
--   'stage:{s}:{n}'  staged barrier s with n members
-- The member count is part of the key so a sibling set that GROWS after closing
-- forms a NEW barrier and still fires (preserving pre-0065 behaviour) rather
-- than being silently suppressed by a stale claim. Known edge case: DELETING a
-- child from a closed stage changes the key and can permit one re-fire. That is
-- accepted -- deleting a completed sub-issue is rare, and a duplicated
-- informational comment is a strictly better failure mode than a suppressed one.
--
-- comment_id records WHICH comment reported the barrier; several rows may share
-- one comment_id (that is exactly the aggregation this migration exists for).
-- No FK on comment_id: the claim and the comment are written in one transaction
-- and a later comment delete must never block or cascade into the ledger.
--
-- NO BACKFILL. Barriers closed before this migration have no row; they cannot
-- re-fire because re-firing requires a fresh non-terminal -> terminal
-- transition, which a fully-terminal barrier can no longer produce.
CREATE TABLE issue_cascade_barrier (
    parent_issue_id TEXT NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    workspace_id    TEXT NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    stage_key       TEXT NOT NULL,
    comment_id      TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (parent_issue_id, stage_key)
);

CREATE INDEX idx_issue_cascade_barrier_ws ON issue_cascade_barrier(workspace_id);
