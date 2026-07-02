-- Hangar v1 schema, migration 0025: the `attention` table — the control-plane
-- inbox (architecture §4.1 / §4.3, spec P2).
--
-- Every input request from EVERY session on the host lands here: an
-- AskUserQuestion, an approval prompt, a Codex request-user, an error, a
-- waiting marker, or an escalation. This is the single answerable inbox the
-- converged control centre (TUI / web / phone / ATC) reads and answers through
-- the one `answer` RPC. Where `event_log` (0024) is the raw replayable bus and
-- `inbox_entry` (0021) is a read/unread digest of issue/comment/task, THIS table
-- is the durable set of open questions the human (or ATC) must resolve.
--
-- Columns:
--
--   - `id`           the attention id (ULID string), minted by the daemon's
--                    ingest producer. TEXT PK like every other Hangar aggregate.
--   - `session_id`   the id of the session that raised the request — the
--                    classifier's `Session.id` (hook claude-session-id or a
--                    discovered id). The answer router resolves this back to a
--                    live session for last-mile delivery.
--   - `cwd`          the raising session's working directory. Carried so the
--                    answer router can run the C1 cwd-ambiguity guard WITHOUT a
--                    second discovery pass at answer time (the id alone is often
--                    a hook id that no discovered session matches, so cwd
--                    correlation — guarded — is the fallback). Empty string when
--                    unknown (never correlates, by design).
--   - `workspace_id` the owning workspace's resolved row id (never the slug), or
--                    NULL for a host session that belongs to no ainb workspace
--                    (hand-started fleet sessions). The FK is enforced only when
--                    non-NULL, so a fleet-wide session inserts freely.
--   - `kind`         the request family, CHECK-constrained to the six the spec
--                    enumerates. Free-form would let a typo silently escape the
--                    control centre; the CHECK makes an unknown kind a hard
--                    insert failure at the boundary.
--   - `payload`      the full serialised request context JSON (the classifier's
--                    `NeedsContext`, or a Codex request-user body). The surfaces
--                    render this; the daemon never re-parses it on the hot path.
--   - `state`        `open` (awaiting an answer) or `answered`. CHECK-guarded.
--                    Defaults to `open` — a freshly-ingested request is always
--                    open. The open→answered flip is the first-answer-wins guard
--                    (a conditional UPDATE, see repo `mark_answered_if_open`).
--   - `degraded`     1 when the row came from the pane-classifier fallback for an
--                    UNHOOKED session (degraded fidelity, still answerable), 0
--                    for the full-fidelity hook path. The surfaces badge degraded
--                    rows so the human knows the source is a heuristic.
--   - `created_at`   epoch milliseconds the request was ingested (INTEGER, like
--                    every Hangar timestamp).
--   - `answered_by`  which surface/actor answered (`tui` / `web` / `bridge` /
--                    `atc` / a user handle), or NULL while open.
--   - `answer`       the answer text delivered into the session, or NULL while
--                    open. Kept for audit + the "answered by X: <answer>" render.
--   - `answered_at`  epoch milliseconds the answer landed, or NULL while open.
--
-- ADD TABLE / ADD INDEX with no backfill is an O(1) catalog change in SQLite, so
-- this is safe on a populated database: every existing workspace simply starts
-- with an empty attention inbox. Re-applying is a no-op via the migrator ledger.

CREATE TABLE attention (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL,
    cwd          TEXT NOT NULL DEFAULT '',
    workspace_id TEXT REFERENCES workspace(id),
    kind         TEXT NOT NULL CHECK (kind IN (
                     'ask_user_question',
                     'approval',
                     'codex_request_user',
                     'error',
                     'waiting',
                     'escalation'
                 )),
    payload      TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'answered')),
    degraded     INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    answered_by  TEXT,
    answer       TEXT,
    answered_at  INTEGER
);

-- The control centre lists "open questions, oldest first" (the longest-waiting
-- request is the most urgent). A composite (state, created_at) index serves the
-- state-filtered, created_at-ordered scan directly, for both the workspace-scoped
-- and the fleet-wide list.
CREATE INDEX idx_attention_state_created
    ON attention (state, created_at);

-- The answer router and the ingest de-dupe look up a session's rows by
-- session_id; index it so those stay cheap as the answered backlog grows.
CREATE INDEX idx_attention_session
    ON attention (session_id);

-- The hot read is exclusively "open rows" (answered rows are audit history). A
-- PARTIAL index over just the open set keeps that scan tight regardless of how
-- much answered history accumulates.
CREATE INDEX idx_attention_open
    ON attention (created_at)
    WHERE state = 'open';
