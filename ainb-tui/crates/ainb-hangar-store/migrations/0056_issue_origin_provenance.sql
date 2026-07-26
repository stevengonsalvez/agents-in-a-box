-- Hangar v1 schema, migration 0056: ISSUE ORIGIN PROVENANCE (multica parity #21).
--
-- multica stamps every platform-created issue with an (origin_type, origin_id)
-- pair — `042_autopilot.up.sql:74-77` for 'autopilot', widened to 'quick_create'
-- by `060_issue_origin_quick_create.up.sql`. It is not decorative: the completion
-- handler looks an issue up BY ORIGIN (`service/task.go:1836 GetIssueByOrigin`)
-- instead of "the agent's most recent issue", which races when one agent creates
-- several issues concurrently; and run/analytics attribution reads it
-- (`service/autopilot.go:251`, `service/task.go:257`).
--
-- hangar's kind set is {autopilot, comment_mention, manual}. `comment_mention` is
-- the structural analogue of multica's `quick_create`: hangar's agent-triggering
-- flow is the `@handle` comment mention, and the daemon injects that provenance
-- into the agent child's env so an issue the agent creates mid-run carries it.
--
-- origin_id semantics: autopilot -> autopilot.id (the RULE, as multica does —
-- `service/autopilot.go:145` passes `ap.ID`, not the run id); comment_mention ->
-- comment.id; manual -> NULL.
--
-- NO CHECK CONSTRAINT: SQLite cannot ALTER TABLE ... ADD CONSTRAINT, and this
-- crate already enforces column domains in the repo layer (see 0055's link_type).
-- `OriginKind::parse` is the single write-side gate; a store test asserts it.
--
-- NOT BACKFILLED: pre-0056 rows keep origin_type NULL, which reads as "provenance
-- unknown" — distinct from the explicit 'manual' a human create stamps from now
-- on. ALTER TABLE ... ADD COLUMN with no default is an O(1) catalog change in
-- SQLite (no table rewrite), safe on populated databases (mirrors 0043).

ALTER TABLE issue ADD COLUMN origin_type TEXT;
ALTER TABLE issue ADD COLUMN origin_id   TEXT;

-- The by-origin lookup (multica's GetIssueByOrigin) and "which issues did this
-- autopilot create" list filter. Partial: provenance-less rows stay out of it.
CREATE INDEX idx_issue_origin ON issue(origin_type, origin_id) WHERE origin_type IS NOT NULL;

-- The TASK carries the same pair so the daemon can hand the agent child its
-- provenance at dispatch (HANGAR_ORIGIN_TYPE / HANGAR_ORIGIN_ID) — the seam that
-- lets a mention-spawned run stamp the issues it creates. Mirrors the existing
-- `autopilot_run_id` linkage column on this table.
ALTER TABLE agent_task_queue ADD COLUMN origin_type TEXT;
ALTER TABLE agent_task_queue ADD COLUMN origin_id   TEXT;

CREATE INDEX idx_task_origin ON agent_task_queue(origin_type, origin_id) WHERE origin_type IS NOT NULL;
