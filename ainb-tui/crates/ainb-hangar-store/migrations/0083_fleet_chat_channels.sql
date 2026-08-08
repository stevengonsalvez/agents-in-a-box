-- Hangar v1 schema, migration 0081: channels, guardrail confirm cards and the
-- copilot activity feed (buzz-port part 2, phase A2).
--
-- Three tables plus one membership join:
--   * fleet_channel         - a named channel and the `channel:<id>` scope it mints
--   * fleet_channel_member  - the recipient set a channel-scoped send may address
--   * fleet_confirm         - one copilot tool call held for an operator
--   * fleet_activity        - the append-only copilot action log
--
-- Forward-only, like every migration here: the back-out is a database file
-- restore, never an in-place downgrade.

-- A channel is a scope with a name and a membership. The scope string is
-- 'channel:' || id and is UNIQUE, because a scope names exactly one channel:
-- the channel-scope rule in `fleet/message_send` resolves membership through
-- it, and two channels sharing a scope would make "is this session a member"
-- ambiguous in the one place it must fail closed.
CREATE TABLE fleet_channel (
    id         TEXT PRIMARY KEY,                                   -- daemon-minted ULID
    kind       TEXT NOT NULL CHECK (kind IN ('copilot','broadcast')),
    name       TEXT NOT NULL CHECK (length(CAST(name AS BLOB)) BETWEEN 1 AND 128),
    scope_key  TEXT NOT NULL UNIQUE,                               -- 'channel:' || id
    created_at INTEGER NOT NULL
);

-- Membership. The wire caps a recipient set at the send ceiling
-- (FLEET_CHANNEL_RECIPIENTS_MAX) and the RPC layer enforces it, so the growth
-- story is bounded rows per channel, not per message.
CREATE TABLE fleet_channel_member (
    channel_id  TEXT NOT NULL REFERENCES fleet_channel(id) ON DELETE CASCADE,
    session_key TEXT NOT NULL,
    PRIMARY KEY (channel_id, session_key)
);

-- One guardrail confirm card: a copilot tool call parked for a human.
--
-- NOT an ACP permission request; those stay part 1's attention rows.
--
-- `arguments` is stored ALREADY PROJECTED to the tool's declared schema keys
-- (ainb_fleet_tools::server::project_arguments). The projection happens before
-- the insert deliberately: this blob is rendered on the operator's approve
-- dialog, so an undeclared model-authored key riding along would be the model
-- arguing its own case to the person approving a destructive action. Persisting
-- raw and filtering on read would leave the unfiltered copy one query away.
--
-- `state` is single-use: the answer path flips 'open' to a terminal value under
-- a WHERE state = 'open' guard, so a second answer (or an expiry racing an
-- answer) affects zero rows and is a typed error rather than a second execution.
--
-- Growth: one row per confirm-class copilot tool call, terminal within
-- `expires_at` (10 minutes, strictly shorter than the 30-minute per-turn
-- deadline so the deadline never converges a turn out from under an open card).
-- Revisit if the open-card count on `hangar/daemon_health` stops returning to
-- zero, which is the signal that cards are being minted faster than answered.
CREATE TABLE fleet_confirm (
    confirm_id         TEXT PRIMARY KEY,                           -- daemon-minted ULID
    scope_key          TEXT NOT NULL CHECK (length(scope_key) > 0),
    tool               TEXT NOT NULL,
    arguments          TEXT NOT NULL,                              -- projected JSON object
    target_session_key TEXT,
    state              TEXT NOT NULL CHECK (state IN ('open','approved','denied','expired')),
    edited_arguments   TEXT,                                       -- non-NULL only for an `edit` answer
    created_at         INTEGER NOT NULL,
    expires_at         INTEGER NOT NULL,
    answered_at        INTEGER
);
-- The operator's list view and the expiry sweep both ask the same question.
CREATE INDEX idx_fleet_confirm_open ON fleet_confirm(created_at) WHERE state = 'open';

-- The append-only copilot activity log: every copilot tool invocation, with the
-- class the classifier assigned and how it ended.
--
-- `seq` is the cursor, assigned by SQLite inside the write transaction exactly
-- as `fleet_message.seq` is, so seq order IS commit order; `id` is the stable
-- external identity and is never an ordering key. AUTOINCREMENT so a deleted
-- tail cannot hand the same seq to a later row.
--
-- `detail` is a SHORT enumerated token plus daemon-authored text; the model's
-- justification is never persisted here, for the same reason the classifier
-- never reads it.
--
-- Growth: one row per copilot tool invocation, append-only, never swept.
-- Revisit trigger: this table is the copilot's audit trail, so retention is an
-- explicit operator-invoked prune (the `fleet_provider_event` pattern), not a
-- timer, and it is not built until the row count justifies it.
CREATE TABLE fleet_activity (
    seq                INTEGER PRIMARY KEY AUTOINCREMENT,          -- commit-ordered cursor
    id                 TEXT NOT NULL UNIQUE,                       -- daemon-minted ULID
    scope_key          TEXT NOT NULL CHECK (length(scope_key) > 0),
    tool               TEXT NOT NULL,
    class              TEXT NOT NULL CHECK (class IN ('read','write','destructive')),
    target_session_key TEXT,
    outcome            TEXT NOT NULL CHECK (outcome IN ('ok','denied','expired','error')),
    detail             TEXT,
    created_at         INTEGER NOT NULL
);
CREATE INDEX idx_fleet_activity_scope ON fleet_activity(scope_key, seq);
