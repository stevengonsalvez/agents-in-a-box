-- Hangar v1 schema, migration 0062: ISSUE SUBSCRIBERS + REACTIONS (multica
-- parity #22).
--
-- Until now `inbox_aggregator::recipients_for` APPROXIMATED "who is notified"
-- as the issue's participants (creator + assignee) minus the actor, and said so
-- in its own doc comment: "hangar has no `issue_subscriber` table". That
-- approximation cannot express the two cases the reference exists for: a
-- WATCHER who is neither creator nor assignee (the human who asked an agent to
-- do the work, multica `service/task.go:1873`), and an actor who wants OUT of a
-- thread they created. This migration is that table
-- (multica `015_issue_subscriber` + `016_backfill_subscribers`), plus the emoji
-- reactions from the reference's `027_issue_reactions`.
--
-- DECISIONS
-- 1. `reason` is PROVENANCE, membership is the state. PK (issue_id, actor_type,
--    actor_id) + ON CONFLICT DO NOTHING => first reason wins, exactly as the
--    reference (`pkg/db/queries/subscriber.sql:1-4`): an actor who created the
--    issue and later commented stays `creator`.
-- 2. `actor_type`/`actor_id` on BOTH tables. The reference names the subscriber
--    columns `user_*` and the reaction columns `actor_*` (an accident of two
--    authors two migrations apart); hangar has one polymorphic-actor convention
--    (0003, 0060) and one Rust type (`ActorRef`).
-- 3. CHECK on `actor_type` (as 0060), NONE on `reason` or `emoji`. SQLite cannot
--    widen a CHECK without a table rebuild (see 0057's copy/drop/recreate); 0058
--    set the precedent of enforcing an append-only vocabulary in Rust instead --
--    `SubscribeReason::as_db_str` is the only writer, `::parse` the tolerant
--    reader. `emoji` is free text by nature.
-- 4. FK on workspace_id only; `issue_id` carries none (0058's rule). The
--    explicit cascade in `IssueRepo::delete_cascade` reaps both tables.
-- 5. NO `muted` flag -- matched to the reference. Unsubscribing then commenting
--    again re-subscribes you. Divergence would be a silent behaviour fork; a
--    `muted` column is a clean future append if we ever want one.

CREATE TABLE issue_subscriber (
    issue_id   TEXT NOT NULL,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('member','agent')),
    actor_id   TEXT NOT NULL,
    reason     TEXT NOT NULL,               -- SubscribeReason::as_db_str()
    created_at INTEGER NOT NULL,            -- epoch millis
    PRIMARY KEY (issue_id, actor_type, actor_id)
);

-- "every issue this actor watches" -- the reference's idx_issue_subscriber_user.
CREATE INDEX idx_issue_subscriber_actor ON issue_subscriber(actor_type, actor_id);

CREATE TABLE issue_reaction (
    id           TEXT PRIMARY KEY,          -- ULID, minted by the caller's IdGen
    issue_id     TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('member','agent')),
    actor_id     TEXT NOT NULL,
    emoji        TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (issue_id, actor_type, actor_id, emoji)
);

CREATE INDEX idx_issue_reaction_issue ON issue_reaction(issue_id);

-- BACKFILL (the reference's 016_backfill_subscribers): an upgrading install is
-- not born empty, so `recipients_for` reads a real subscriber set for issues
-- that predate this migration instead of silently falling back to participants
-- forever. Three statements, first-reason-wins ordering (creator, then assignee,
-- then commenter), each OR IGNORE.
INSERT OR IGNORE INTO issue_subscriber (issue_id, actor_type, actor_id, reason, created_at)
SELECT id, creator_type, creator_id, 'creator', created_at FROM issue;

INSERT OR IGNORE INTO issue_subscriber (issue_id, actor_type, actor_id, reason, created_at)
SELECT id, assignee_type, assignee_id, 'assignee', created_at
FROM issue WHERE assignee_type IS NOT NULL AND assignee_id IS NOT NULL;

INSERT OR IGNORE INTO issue_subscriber (issue_id, actor_type, actor_id, reason, created_at)
SELECT c.issue_id, c.author_type, c.author_id, 'commenter', MIN(c.created_at)
FROM comment c GROUP BY c.issue_id, c.author_type, c.author_id;
