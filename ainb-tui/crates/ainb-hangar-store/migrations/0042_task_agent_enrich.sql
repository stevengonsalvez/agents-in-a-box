-- 0042: enriched task + agent model (plan: plans/hangar-task-agent-model.md).
--
-- Agent: optional token budget (rtk/headroom). NULL = unlimited. Stored +
-- surfaced only in this milestone; dispatch-time enforcement is a later
-- feature.
ALTER TABLE agent ADD COLUMN token_budget INTEGER;

-- Task inputs: the branch a run branches FROM (resolved at dispatch,
-- default `main` when NULL), and the branch a future PR lands INTO
-- (stored now, consumed by later PR automation). Distinct from
-- agent_task_queue.branch, which is the PRODUCED `ainb/<slug>` output
-- branch recorded post-run.
ALTER TABLE issue ADD COLUMN source_branch TEXT;
ALTER TABLE issue ADD COLUMN target_branch TEXT;
ALTER TABLE agent_task_queue ADD COLUMN source_branch TEXT;
