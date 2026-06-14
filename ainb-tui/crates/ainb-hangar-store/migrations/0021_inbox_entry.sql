-- Hangar v1 schema, migration 0021: the aggregated notification inbox (e38.14).
--
-- Until now a `HangarEvent` was a purely live broadcast: the daemon fanned
-- issue / comment / task lifecycle events to subscribed plugins over the socket,
-- but nothing PERSISTED or AGGREGATED them. A plugin that was not attached at
-- the moment an event fired never saw it, and there was no notion of an unread
-- count or a mark-read sweep (the reference notifyd Inbox pattern). This migration
-- adds the durable aggregate the inbox screen reads from.
--
-- An `inbox_entry` is one aggregated, workspace-scoped notification derived from
-- a live issue / comment / task event:
--
--   - `id`            ULID primary key (minted by the daemon's IdGen, like every
--                     other row).
--   - `workspace_id`  the owning workspace (the resolved row id, never the slug),
--                     FK to `workspace(id)` so the inbox is tenant-isolated by the
--                     same scoping every snapshot enforces.
--   - `kind`          the entity family the event is about — one of `issue`,
--                     `comment`, `task`. CHECK-constrained so a malformed writer
--                     can never land an unknown family (mirrors the polymorphic
--                     actor `_type` CHECK in 0003).
--   - `event`         the wire event discriminant that produced this entry (e.g.
--                     `issue_created`, `comment_added`, `task_queued`). Free TEXT
--                     so a new HangarEvent variant needs no schema change.
--   - `subject_id`    the id of the issue / comment / task the entry is about, so
--                     the screen can deep-link to it.
--   - `summary`       a short pre-rendered human line for the list row.
--   - `created_at`    epoch milliseconds (INTEGER, like every Hangar timestamp).
--   - `read_at`       epoch milliseconds the entry was marked read, or NULL when
--                     UNREAD. NULL = unread is the whole basis of the unread
--                     count + the mark-read sweep (it sets `read_at` to "now").
--
-- ADD TABLE / ADD INDEX with no data backfill is an O(1) catalog change in
-- SQLite, so this is safe on a populated database: every pre-existing workspace
-- simply starts with an empty inbox (zero unread). Re-applying is a no-op via the
-- migrator's version ledger.

CREATE TABLE inbox_entry (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    kind         TEXT NOT NULL CHECK (kind IN ('issue','comment','task')),
    event        TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    summary      TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    read_at      INTEGER
);

-- The inbox list reads "this workspace's entries, newest first" and the unread
-- count reads "this workspace's entries where read_at IS NULL"; index the
-- (workspace_id, created_at) pair the list orders by.
CREATE INDEX idx_inbox_entry_workspace_created
    ON inbox_entry(workspace_id, created_at);

-- Partial index over just the unread rows so the unread-count query stays cheap
-- as read entries accumulate (the same shape notifyd uses for its unread index).
CREATE INDEX idx_inbox_entry_unread
    ON inbox_entry(workspace_id)
    WHERE read_at IS NULL;
