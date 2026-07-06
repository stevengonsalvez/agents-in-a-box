-- Hangar v1 schema, migration 0024: the durable event outbox (T1 / architecture
-- §4.1 `events(seq, ts, type, entity, payload)` + §4.2 event bus "snapshot then
-- deltas / catch-up FROM seq>N").
--
-- Migration 0021 added `inbox_entry`, but that is a DIFFERENT aggregate: a
-- workspace-scoped, read/unread DIGEST of the three notification families
-- (issue / comment / task) that feeds the Inbox screen. It is NOT a replayable
-- raw log — it drops the high-frequency events (progress heartbeats, transcript
-- lines, presence, autopilot/workspace housekeeping), carries no monotonic
-- sequence, and its ULID ids do not order a cross-workspace catch-up cursor.
--
-- This migration adds the missing piece the event bus needs to survive a
-- reconnect or a daemon restart: a durable, append-only outbox of EVERY
-- `HangarEvent` the daemon emits, keyed by a monotonic `seq`. A subscriber that
-- was absent while events fired (late join) or that dropped its socket (daemon
-- restart) resumes by asking for everything `seq > <its last cursor>`.
--
-- Columns:
--
--   - `seq`          INTEGER PRIMARY KEY AUTOINCREMENT — the monotonic cursor.
--                    AUTOINCREMENT (not a plain rowid alias) guarantees a seq is
--                    never reused even after the newest row is deleted, so a
--                    stored client cursor can never collide with a future event.
--   - `ts`           epoch milliseconds the event was logged (INTEGER, like every
--                    Hangar timestamp).
--   - `workspace_id` the owning workspace (resolved row id, never the slug), FK
--                    to `workspace(id)` so replay is tenant-isolated by the same
--                    scoping every snapshot enforces.
--   - `event_type`   the wire event discriminant (e.g. `task_finished`,
--                    `issue_created`) — the serde tag of the stored `HangarEvent`.
--                    Free TEXT so a new variant needs no schema change. Named
--                    `event_type` rather than the bare `type` in §4.1 to avoid
--                    any collision with SQLite's `type` usage.
--   - `entity`       the id of the subject the event is about (task / issue /
--                    comment / agent / autopilot / workspace), or NULL for the
--                    rare variant with no single subject. Indexed-adjacent so a
--                    per-entity history read stays cheap later.
--   - `payload`      the full serialised `HangarEvent` JSON — the exact object the
--                    daemon re-frames verbatim as the `params` of a replayed
--                    `hangar/event` notification, so replay needs no re-encode.
--
-- ADD TABLE / ADD INDEX with no backfill is an O(1) catalog change in SQLite, so
-- this is safe on a populated database: every pre-existing workspace simply
-- starts with an empty log (head cursor 0). Re-applying is a no-op via the
-- migrator's version ledger.

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    event_type   TEXT NOT NULL,
    entity       TEXT,
    payload      TEXT NOT NULL
);

-- Replay reads "this workspace's events with seq greater than my cursor, in seq
-- order"; the head-cursor read is "MAX(seq) for this workspace". A composite
-- (workspace_id, seq) index serves both: the range scan walks it directly and
-- the MAX is the index tail.
CREATE INDEX idx_event_log_workspace_seq
    ON event_log(workspace_id, seq);
