-- Hangar v1 schema, migration 0058: DISPATCH ATTEMPT AUDIT (multica parity #12).
--
-- multica carries an admission-time reason vocabulary in a dedicated leaf
-- package (`server/internal/dispatch/reason.go`, MUL-4525) so the layer that
-- DECIDES a dispatch and the layer that SERIALIZES the decision share one set of
-- codes and cannot drift. hangar had the decision (`CardRunError` in the daemon's
-- `run_card`) but threw it away at every call site: two paths turned it into an
-- ephemeral JSON-RPC error string, three others logged it at debug/info and
-- returned, and the CLI assign path swallowed it as a bare `Ok(None)`. Nothing
-- was persisted, so "why is my card not running" had no answer.
--
-- This table is that answer: one row per dispatch attempt, carrying the stable
-- code (`DispatchReason::as_db_str`), a free-text detail, and which trigger
-- surface made the attempt (`DispatchSource::as_db_str`).
--
-- The precedent is migration 0057, which gave `autopilot_run` a non-failure
-- `skipped` status for exactly this reason ("reusing `failed` would pollute the
-- failure-rate signal"). 0058 generalises it from autopilot concurrency skips to
-- EVERY admission decision, and upgrades free-text to a stable code + detail.
--
-- THREE DELIBERATE SCHEMA DECISIONS:
--
-- 1. NO CHECK CONSTRAINT on `reason` (nor on `source`). SQLite cannot widen a
--    CHECK without a full table rebuild — 0057 needed a defer_foreign_keys +
--    copy/drop/recreate dance for precisely that. This vocabulary is append-only
--    by design, so the rebuild would recur on every future variant. The domain is
--    enforced in Rust instead: `DispatchReason::as_db_str` is the only writer and
--    `DispatchReason::parse` is the tolerant reader (an unknown token decodes to
--    None and renders as raw text rather than poisoning the read path).
--
-- 2. FK ONLY ON `workspace_id`. `issue_id` / `agent_id` / `runtime_id` /
--    `task_id` carry no FK deliberately: the audit row is a historical FACT that
--    must survive the death of what it describes (a declined dispatch of an issue
--    deleted next week still happened), and a recorder must never fail on a race
--    with a concurrent delete. The explicit cascade in `IssueRepo::delete` is what
--    reaps rows for a deleted issue, matching how `agent_task_queue` is handled.
--
-- 3. BOUNDED BY CONSTRUCTION, NOT BY A SWEEPER. `DispatchAttemptRepo::record`
--    trims to the newest DISPATCH_ATTEMPT_KEEP (20) rows per issue in the same
--    statement batch as the insert, so a hot auto-run cascade cannot grow the
--    table without bound and no new sweeper job is needed.

CREATE TABLE dispatch_attempt (
    id            TEXT PRIMARY KEY,              -- ULID
    workspace_id  TEXT NOT NULL REFERENCES workspace(id),
    issue_id      TEXT,                          -- NULL for a future issue-less trigger
    agent_id      TEXT,                          -- the resolved target, when one resolved
    runtime_id    TEXT,
    task_id       TEXT,                          -- set iff a task row was actually written
    reason        TEXT NOT NULL,                 -- DispatchReason::as_db_str()
    detail        TEXT,
    source        TEXT NOT NULL DEFAULT 'manual',-- DispatchSource::as_db_str()
    created_at    INTEGER NOT NULL               -- epoch millis
);

-- The per-card read ("why is THIS not running"): newest-first, id as the
-- tiebreaker so two attempts inside one millisecond still order deterministically
-- (ULIDs are monotonic within a millisecond).
CREATE INDEX idx_dispatch_attempt_issue ON dispatch_attempt(issue_id, created_at DESC, id DESC);

-- The workspace-scoped feed behind `hangar/dispatch_attempts_list`.
CREATE INDEX idx_dispatch_attempt_ws    ON dispatch_attempt(workspace_id, created_at DESC);
