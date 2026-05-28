-- Hangar v1 schema, migration 0003: issues and comments with polymorphic actors.
--
-- An `issue` is a unit of work in a workspace. Both its assignee and its creator
-- are *polymorphic actors*: each is a `(type, id)` pair where `type` is one of
-- `member` or `agent`. There is deliberately NO foreign key on the `_id`
-- columns — the actor may live in either the `member` or `agent` table, and
-- SQLite cannot express a conditional FK. The `_type` CHECK constraint is what
-- keeps the column honest; integrity of the `_id` half is enforced at the
-- service layer (per Multica architecture review §7).
--
-- The `assignee` is optional (an unassigned issue is valid), so both
-- `assignee_type` and `assignee_id` are nullable. The `creator` is mandatory.
--
-- A `comment` belongs to exactly one `issue` and cascades on issue deletion. Its
-- `author` is the same polymorphic actor pattern (mandatory).
--
-- All timestamps are epoch milliseconds stored as INTEGER (see 0001).

CREATE TABLE issue (
    id            TEXT PRIMARY KEY,
    workspace_id  TEXT NOT NULL REFERENCES workspace(id),
    title         TEXT NOT NULL,
    description   TEXT,
    state         TEXT NOT NULL DEFAULT 'open',
    assignee_type TEXT CHECK (assignee_type IN ('member','agent')),
    assignee_id   TEXT,
    creator_type  TEXT NOT NULL CHECK (creator_type IN ('member','agent')),
    creator_id    TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

-- Workspace-scoped state filter: the TUI's primary issue-list query is
-- "open issues in this workspace", so index the (workspace_id, state) pair.
CREATE INDEX idx_issue_workspace_state ON issue(workspace_id, state);

CREATE TABLE comment (
    id          TEXT PRIMARY KEY,
    issue_id    TEXT NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    author_type TEXT NOT NULL CHECK (author_type IN ('member','agent')),
    author_id   TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
