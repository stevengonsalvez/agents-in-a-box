-- Hangar v1 schema, migration 0074: role-gated pull pipeline.
--
-- # THIS REVERSES DECISION D3 (migration 0053)
--
-- 0053 recorded, as "DEVIATION (D3)", that `squad_member.role` does NOT gate
-- dispatch: that `SquadRepo::member_agent_ids` stays role-blind, that every agent
-- member is dispatched to regardless of role, and that selective routing was
-- deferred to parity item #16. This migration reverses that deliberately.
--
-- Role-blind dispatch WAS the reported defect. Issue 01KY7SHDMWVMHE218DV5TQRN3R
-- produced four runs, three of them within two seconds on the assignee plus both
-- members of squad `team1`, each provisioning its own worktree for the same work.
-- `services_role` below is the gate that makes `role` finally mean something at
-- the dispatch seam.
--
-- 0053's own file cannot be corrected to say this: an applied migration's text is
-- frozen (`sqlx` checksums the full file, and editing one aborts boot with
-- "previously applied but has been modified"). The reversal is therefore recorded
-- in `src/lib.rs` alongside the other applied-migration corrections, and in the
-- `SquadRepo::member_agent_ids` doc comment.
--
-- Turns board columns into PULL QUEUES. Before this migration a squad dispatch
-- BROADCASTS: `SquadAssignService::assign_fanout` writes the leader brief plus one
-- `agent_task_queue` row per distinct agent member, so one issue becomes N
-- simultaneous runs all doing the same work in N worktrees. After it, the card in a
-- role-gated column IS the queue: exactly one eligible agent pulls the card, runs it,
-- and hands off to a DIFFERENT role at the next column.
--
-- # Why the queue row is created at pull time (and `agent_id` stays NOT NULL)
--
-- `agent_task_queue.agent_id` is `TEXT NOT NULL REFERENCES agent(id)`, so a queued row
-- always already names its agent: today's model is PUSH (the dispatcher picks the
-- agent; the claim loop merely executes that choice). True pull needs the agent chosen
-- AT CLAIM TIME. Making `agent_id` nullable would express that directly, but on SQLite
-- dropping a NOT NULL is a full 12-step table rebuild (new table, copy, drop, rename,
-- recreate every index and FK) against a populated production database, the single
-- riskiest operation available here. Instead the `board_card` row is the queue and the
-- `agent_task_queue` row is INSERTed by the pull statement once an eligible agent has
-- been selected, so `agent_id` is known by construction at insert time and the column
-- keeps its NOT NULL.
--
-- Every statement below is an additive `ALTER TABLE ... ADD COLUMN` with a CONSTANT
-- literal default (or no default at all, i.e. NULL), which is an O(1) catalog change in
-- SQLite with no table rewrite, the same shape 0047 and 0050 used, and safe on a
-- populated home.
--
-- NULL is meaningful on both new `board_column` columns, which is why neither takes a
-- NOT NULL default:
--   * `services_role IS NULL`  = the column is NOT role-gated and is therefore NOT a
--     pull queue. Backlog and Done are the shipped examples: work parks there and is
--     moved by a human or by the auto-advance, never pulled by an agent. Every
--     pre-existing column on an operator's board migrates to NULL and so keeps exactly
--     its current (unpullable) behaviour. This migration cannot change how any existing
--     board behaves.
--   * `wip_limit IS NULL`      = unlimited, the pre-existing semantics.
--
-- `excludes_prior_agent` is the data-driven form of the "a reviewer is never the agent
-- that implemented the card" invariant. Encoding it as a column property rather than
-- string-matching `services_role = 'reviewer'` keeps the rule a property of the
-- PIPELINE DEFINITION: an operator who names the stage "Peer check" or "Second pair"
-- still gets the guarantee, and a deployment that genuinely wants self-review (a
-- one-agent roster) can express that by leaving the flag at 0 rather than by renaming a
-- column to dodge a hard-coded string.

-- The role gate. Free text matched against `squad_member.role`, which migration 0053
-- added and which is documented "role-blind by design" at the dispatch seam
-- (`SquadRepo::member_agent_ids`), this is the column that finally gives that field a
-- consumer. A column's role is matched as a COMMA-SEPARATED TOKEN of the member's role
-- string, case-insensitively, so one membership can advertise several roles
-- ("implementer,reviewer") without a second table and without a schema change.
ALTER TABLE board_column ADD COLUMN services_role TEXT;

-- Work-in-progress cap: the maximum number of cards in this column that may hold an
-- active (queued / dispatched / running) task at once. NULL = unlimited.
ALTER TABLE board_column ADD COLUMN wip_limit INTEGER;

-- When 1, an agent that already holds a finished task on the card may NOT pull it at
-- this stage. This is the reviewer-is-never-the-implementer guarantee.
ALTER TABLE board_column ADD COLUMN excludes_prior_agent INTEGER NOT NULL DEFAULT 0;

-- Clusters the rows of ONE deliberate fan-out (`--redundant N`), so intentional
-- parallelism stays distinguishable from the accidental broadcast this migration
-- removes. NULL for an ordinary single-owner pull, which is every row written before
-- this migration and the overwhelming majority written after it.
ALTER TABLE agent_task_queue ADD COLUMN run_group TEXT;

-- The pull statement's hot filter. It scans role-gated columns of one board in stage
-- order, so `(board_id, services_role)` is the selective prefix; `ord` is carried in the
-- index because the pull always takes the EARLIEST eligible stage and this keeps that
-- an index-ordered read instead of a sort. Partial (`WHERE services_role IS NOT NULL`)
-- so the non-gated columns, Backlog, Done, and every column that predates this
-- migration, cost nothing to keep indexed.
CREATE INDEX idx_board_column_pull
    ON board_column (board_id, services_role, ord)
    WHERE services_role IS NOT NULL;

-- The pull statement's other half: given a candidate card it must ask "does this issue
-- already have an active task, and if so whose?", the one-owner-per-card guard, the
-- per-column WIP count, and the prior-agent exclusion all read this. The nearest
-- existing cover is `idx_one_pending_task_per_issue_agent` (migration 0012), but it is
-- partial on `status IN ('queued','dispatched')` and so cannot answer a probe whose
-- active set INCLUDES `running`, exactly the set all three predicates need. This index
-- is unfiltered on status, so the probe resolves entirely inside it rather than
-- fetching each candidate row to test its status.
CREATE INDEX idx_task_issue_status
    ON agent_task_queue (issue_id, status)
    WHERE issue_id IS NOT NULL;

-- Cluster lookup for `--redundant N`: "show me every run of this fan-out". Partial, so
-- the NULL run_group of every ordinary pull stays out of the index entirely.
CREATE INDEX idx_task_run_group
    ON agent_task_queue (run_group)
    WHERE run_group IS NOT NULL;
