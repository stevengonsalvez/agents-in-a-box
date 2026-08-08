-- Hangar migration 0082: collapse repeat hook firings for ONE logical request
-- onto ONE `attention` row.
--
-- Numbered 0082, not 0080 or 0081. Both of those versions are already applied
-- on live databases, from a sibling branch: 80 as "fleet event retention" and
-- 81 as "fleet event observed at". `_sqlx_migrations` records each with its own
-- checksum, so shipping different content under either version fails sqlx's
-- checksum validation and refuses to open the store on boot -- observed live as
-- `migration 81 was previously applied but has been modified`.
--
-- Pick the next free version, never backfill a lower one: a version below an
-- already-applied migration produces the sibling failure mode,
-- `migration N was previously applied but is missing in the resolved migrations`.
--
-- # The defect
--
-- The ingest keys a row `att:<session>:<event_id>`, where `event_id` is a fresh
-- UUID minted per hook invocation (`ainb fleet atc hook`). Claude re-fires its
-- `Notification` hook while a session stays blocked, so ONE question produced N
-- rows — measured live as three rows, three UUIDs, one question. The
-- check-then-insert on the row id cannot see that, because every firing carries
-- a genuinely new id.
--
-- # `request_key`
--
-- The stable identity of the REQUEST a row is about, minted by the raising
-- producer. Every hook firing that observes the same still-open request derives
-- the same key, so the second and later firings collapse onto the first row.
-- NULL for rows whose producer has no stable request identity (waiting / error /
-- escalation cards, and every row raised before this migration).
--
-- NOT the same value as `fleet_session.current_request_fingerprint`, and NOT
-- joinable to it. That column is fnv1a64 over the hook payload's
-- `{tool_use_id, tool_input}` — but the `Notification` hook line that raises most
-- ASK rows carries NEITHER field (its payload is just
-- `{message, notification_type, prompt_id, cwd, session_id, transcript_path}`),
-- so that fingerprint is simply unavailable at ingest time. The attention key is
-- derived from the CLASSIFIED request content instead. Two different hash
-- inputs; the names are deliberately different so nobody equates them.
--
-- # Uniqueness
--
-- Partial UNIQUE over the OPEN set only. Scoping to `state = 'open'` is what
-- makes a genuinely repeated request answerable again: once a row is closed, an
-- identical request asked later mints a fresh row rather than being silently
-- swallowed (a missing card strands a blocked session, which is strictly worse
-- than a duplicate one). `request_key IS NOT NULL` keeps every legacy and
-- keyless row out of the index entirely.
--
-- # Cost on a populated database
--
-- ADD COLUMN is an O(1) catalog change. The unique index is built over rows
-- matching its WHERE, and NO existing row has a non-NULL `request_key`, so it is
-- created empty. No backfill, no data rewrite.

ALTER TABLE attention ADD COLUMN request_key TEXT;

CREATE UNIQUE INDEX idx_attention_open_request_key
    ON attention (session_id, request_key)
    WHERE state = 'open' AND request_key IS NOT NULL;
