-- Hangar v1 schema, migration 0064: AUTOPILOT SUBSCRIBERS + COLLABORATORS
-- (multica parity #27, reference migrations 120 + 128).
--
-- Two per-RULE actor sets, both mirroring `issue_subscriber` (0062) the way the
-- reference does:
--
--   * `autopilot_subscriber`  -- the standing NOTIFY list. Every issue this
--     autopilot SPAWNS auto-subscribes this set, so a human tracking a
--     recurring automation is notified per occurrence without watching each
--     spawned issue by hand.
--   * `autopilot_collaborator` -- explicit WRITE-GRANTS on the rule itself,
--     beyond the implicit owner / workspace-owner / workspace-admin.
--
-- DECISIONS
--
-- 1. NO FK on `autopilot_id` or on the actor columns. The reference's own rule
--    for both tables ("no-FK, app-layer integrity"), and hangar's established
--    one since 0058/0059/0061: tenant isolation is enforced in application SQL
--    (every write is scoped through a join to `autopilot`), and a best-effort
--    fan-out must never fail on a race with a concurrent delete. `workspace_id`
--    carries the only FK, as 0062.
--
-- 2. CHECK on `actor_type` (the 0060/0062 convention), NONE on `role`. SQLite
--    cannot widen a CHECK without a full table rebuild (see 0057's
--    copy/drop/recreate), and the role vocabulary is append-only by design; it
--    is enforced in Rust by `CollaboratorRole::as_db_str` (the only writer) and
--    a TOLERANT `::parse` (an unknown token from a newer daemon reads back as
--    `None` and renders raw, never poisons the read).
--
-- 3. PK is (autopilot_id, actor_type, actor_id) on BOTH tables -- set
--    MEMBERSHIP, exactly as `issue_subscriber`. Every writer is
--    `INSERT OR IGNORE`, so re-adding an existing collaborator keeps the
--    ORIGINAL row (first-grant-wins) instead of silently re-stamping
--    `created_at`. A ROLE CHANGE is therefore an explicit UPDATE, never an
--    accidental side effect of a re-add.
--
-- 4. `access_mode` on `autopilot` DEFAULTS TO 'open', and every pre-0064 row
--    reads back 'open'. This is the 0047 `permission_mode` precedent
--    ('private' | 'public_to') applied to rules. Defaulting to 'restricted'
--    would deny-by-default every autopilot that already exists on an upgrading
--    install -- including every solo install where nobody is a collaborator of
--    anything -- which is a silent lockout, not a security win. Collaborators
--    MEAN something only once an owner opts the rule into 'restricted'.
--    `ALTER TABLE ... ADD COLUMN` with a CONSTANT literal default is an O(1)
--    catalog-only change on SQLite (0047's note); no table rewrite.
--
-- 5. NO BACKFILL of either table. A pre-0064 autopilot gets no subscriber and
--    no collaborator rows. Inventing "the creator is a collaborator" would be a
--    fabricated grant record; the creator is already implicitly authorised (the
--    write predicate resolves it from the 0061 rule-version ledger), so the
--    fabrication would buy nothing. Same honesty rule as 0061 decision 5 and
--    0052's `archived_at`.
--
-- `created_at` is epoch MILLISECONDS (`HangarClock::now_ms`), matching every
-- other hangar timestamp column.

CREATE TABLE autopilot_subscriber (
    autopilot_id TEXT NOT NULL,                 -- NO FK, see decision 1
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('member','agent')),
    actor_id     TEXT NOT NULL,
    created_by   TEXT,                          -- actor ref; NULL = unattributed
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (autopilot_id, actor_type, actor_id)
);

-- "every autopilot this actor follows" -- the reference's idx_..._user.
CREATE INDEX idx_autopilot_subscriber_actor
    ON autopilot_subscriber(actor_type, actor_id);

CREATE TABLE autopilot_collaborator (
    autopilot_id TEXT NOT NULL,                 -- NO FK, see decision 1
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('member','agent')),
    actor_id     TEXT NOT NULL,
    role         TEXT NOT NULL,                 -- CollaboratorRole::as_db_str()
    created_by   TEXT,                          -- actor ref; NULL = unattributed
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (autopilot_id, actor_type, actor_id)
);

CREATE INDEX idx_autopilot_collaborator_actor
    ON autopilot_collaborator(actor_type, actor_id);

-- Who may WRITE this rule. 'open' (the default, and every pre-0064 row) = any
-- actor in the workspace, i.e. today's behaviour, unchanged. 'restricted' =
-- the owner / workspace owner+admin / an explicit collaborator only.
ALTER TABLE autopilot
    ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'open'
        CHECK (access_mode IN ('open','restricted'));
