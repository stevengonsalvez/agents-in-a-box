-- 0067: COMMENT THREADING + TASK TRIGGER PROVENANCE (multica parity #2-rest).
--
-- The mention-routing layer needs two facts the schema could not express:
--
-- 1. WHICH comment a reply answers, so the router can walk multica's implicit
--    fallback chain (reply-to-parent-author -> thread-root owner -> assignee,
--    `server/internal/handler/comment.go` step 4) when a comment mentions
--    nobody. multica carries this as `comment.parent_id`
--    (`001_init.up.sql:97`); hangar's `comment` table (0003) had no such column,
--    so hangar had no comment threads at all.
--
-- 2. WHICH comment summoned a task, so the spawned agent reads the actual ask
--    and its reply threads under that comment (multica
--    `agent_task_queue.trigger_comment_id`, `server/internal/service/task.go:443`).
--
-- Both are plain `ADD COLUMN` with an implicit NULL default plus an index: O(1)
-- catalog changes in SQLite, safe on a populated database, no table rewrite and
-- no backfill. Every pre-0067 comment is a top-level comment (`parent_id IS
-- NULL`) and every pre-0067 task has no recorded trigger, which is the honest
-- reading of "we never stored this" -- neither is given a fabricated value.
--
-- Re-applying is a no-op via the version ledger: sqlx records the applied
-- version, so this file runs exactly once per database. It must therefore never
-- be edited after it has been applied anywhere (a byte-changed applied
-- migration stops the daemon booting with "previously applied but has been
-- modified"); a correction goes in a NEW numbered file.

-- Comment threading. Nullable with no default, so the ADD COLUMN may legally
-- carry the self-referencing FK in SQLite (a NOT NULL / non-constant default
-- could not).
--
-- ON DELETE SET NULL, deliberately, and NOT the omitted-clause default:
--   * omitting it means NO ACTION, which BLOCKS a parent delete while a reply
--     points at it. `IssueRepo::delete` and the workspace purge both run one
--     bulk `DELETE FROM comment WHERE issue_id = ?`, and SQLite enforces FKs
--     row-by-row inside a statement, so a thread whose parent happened to be
--     deleted first would fail the whole issue delete. That is a regression
--     0067 must not introduce.
--   * CASCADE would let deleting one comment silently take a subtree of
--     unrelated replies with it.
-- SET NULL detaches the reply instead: it becomes a top-level comment, still
-- readable, and the thread walk simply stops there. Same spirit as 0065's rule
-- that a delete never reaches into another table's ledger.
ALTER TABLE comment ADD COLUMN parent_id TEXT
    REFERENCES comment(id) ON DELETE SET NULL;

-- The reply lookup the fallback chain performs is "who authored the parent of
-- this comment", plus "list the replies under X" for the thread walk; both key
-- on parent_id.
CREATE INDEX IF NOT EXISTS idx_comment_parent ON comment(parent_id);

-- The comment that summoned this task. NO foreign key, deliberately: deleting
-- the comment must neither cascade the task away (the run happened; its history
-- is not the comment's to erase) nor block the delete. A stale id simply reads
-- back as "no trigger", the same tolerant-reader rule the origin provenance
-- columns (0056) use.
ALTER TABLE agent_task_queue ADD COLUMN trigger_comment_id TEXT;

-- Answers "which tasks did this comment spawn" for the audit / `issue why`
-- surfaces without a scan of the queue.
CREATE INDEX IF NOT EXISTS idx_task_trigger_comment
    ON agent_task_queue(trigger_comment_id);
