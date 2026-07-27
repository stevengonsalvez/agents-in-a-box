-- Hangar v1 schema, migration 0059: GENERIC ACTIVITY LOG (multica parity #13).
--
-- multica's `activity_log` (001_init.up.sql:156) is the per-issue NARRATIVE:
-- every state change, re-assignment, priority/title/due-date edit and task
-- outcome, attributed to a polymorphic actor. hangar had three narrower logs
-- (`run_history` = execution cost, `dispatch_attempt` = admission decisions,
-- `event_log` = the replay outbox) and no per-issue story at all.
--
-- FIVE DELIBERATE SCHEMA DECISIONS:
--
-- 1. NO CHECK ON `action` (nor on `actor_type`). Same rule 0058 established:
--    SQLite cannot widen a CHECK without a full table rebuild, and this
--    vocabulary is append-only by design. The domain lives in Rust
--    (`ainb_hangar_core::activity::ActivityAction`), whose parse is TOLERANT --
--    an unknown token written by a newer daemon decodes to None and renders as
--    raw text rather than poisoning the read path.
--
-- 2. `actor_type` ADMITS 'system'. hangar's `ActorKind` is member|agent only,
--    and widening it would ripple into the CHECK constraints on issue, comment
--    and agent_task_queue. Instead this column is TEXT and the Rust writer uses
--    `ActivityActor::{Actor(ActorRef), System}`; a system row stores
--    actor_type='system', actor_id=NULL. This matches multica, whose own CHECK
--    admits 'system'.
--
-- 3. FK ONLY ON `workspace_id`. `issue_id` and `actor_id` carry no FK: the row
--    is a historical FACT that must survive the death of what it describes, and
--    a best-effort recorder must never fail on a race with a concurrent delete.
--    `IssueRepo::delete_cascade` is what reaps rows for a deleted issue,
--    matching how `dispatch_attempt` and `agent_task_queue` are handled.
--
-- 4. `details` IS JSON TEXT, NOT JSONB. SQLite has no JSONB type; the column
--    holds the serialised `serde_json::Value` object and defaults to '{}'. No
--    `json_valid` CHECK -- the only writer is `serde_json::to_string` on a
--    `Value::Object`, and a CHECK would be a rebuild-blocker for no gain.
--
-- 5. NOT TRIMMED. Unlike `dispatch_attempt` (0058, bounded to 20/issue) this log
--    is the narrative and must stay complete -- trimming would silently erase the
--    beginning of an issue's story. Growth is bounded in practice by the issue
--    count (a handful of rows per issue lifecycle) and reaped by the issue
--    delete cascade. If unbounded growth ever bites, the fix is a workspace-level
--    retention sweep, not a per-issue trim.
--
-- SCOPED OUT: the Beads bridge (`daemon/src/beads_sync/`) also writes issue
-- state. It is an external MIRROR reconcile that can rewrite state in bulk;
-- instrumenting it would flood the narrative with mirror noise, so it is
-- deliberately not recorded.
--
-- `created_at` is epoch MILLISECONDS (`HangarClock::now_ms`), matching every
-- other hangar timestamp column.

CREATE TABLE activity_log (
    id            TEXT PRIMARY KEY,                 -- ULID
    workspace_id  TEXT NOT NULL REFERENCES workspace(id),
    issue_id      TEXT,                             -- NULL for a future issue-less activity
    actor_type    TEXT,                             -- 'member' | 'agent' | 'system'
    actor_id      TEXT,                             -- NULL iff actor_type = 'system'
    action        TEXT NOT NULL,                    -- ActivityAction::as_db_str()
    details       TEXT NOT NULL DEFAULT '{}',       -- serialised JSON object
    created_at    INTEGER NOT NULL                  -- epoch millis
);

-- The per-issue timeline read (multica 068's keyset shape). ULIDs are monotonic
-- within a millisecond so `id` is a deterministic tiebreaker.
CREATE INDEX idx_activity_log_issue ON activity_log(issue_id, created_at DESC, id DESC);

-- The workspace-scoped feed (future "what happened today" surface).
CREATE INDEX idx_activity_log_ws    ON activity_log(workspace_id, created_at DESC);
