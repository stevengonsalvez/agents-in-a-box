-- Hangar v1 schema, migration 0012: per-(issue, agent) pending-task scope.
--
-- Replaces the migration-0004 `idx_one_pending_task_per_issue` — which
-- serialized ONE pending task per issue across ALL agents — with the
-- per-(issue, agent) scope the reference's `ClaimAgentTask` was designed around
-- (`pkg/db/queries/agent.sql`): DIFFERENT agents may each hold a pending task
-- for the same issue and work it in parallel, while the SAME agent still
-- coalesces duplicate fires to one pending row per issue. Decision recorded in
-- docs/hangar/architecture.md (parity-review design gap 03).
--
-- The claim path pairs this index with a NOT EXISTS active-set guard
-- (CLAIM_SQL in ainb-hangar-store `service/claim.rs`): an agent with a
-- queued / dispatched / running task on an issue is never dispatched a second
-- one for that issue, but another agent can be.
--
-- DROP + CREATE is safe on populated databases: every dataset valid under the
-- old (stricter) index is valid under the new (looser) one, so the index build
-- cannot fail, and table rows are untouched. The WHERE form stays portable to
-- the future Postgres backend byte-for-byte.

DROP INDEX idx_one_pending_task_per_issue;

CREATE UNIQUE INDEX idx_one_pending_task_per_issue_agent
    ON agent_task_queue(issue_id, agent_id)
    WHERE issue_id IS NOT NULL AND status IN ('queued','dispatched');
