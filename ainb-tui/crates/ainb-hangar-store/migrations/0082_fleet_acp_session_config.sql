-- Hangar v1 schema, migration 0080: per-session ACP config (buzz-port part 2).
--
-- Part 1 shipped STATIC daemon adapter config by design and delegated the
-- per-session override here; `fleet/copilot_configure` is the write side.
-- Three nullable columns on fleet_acp_session, not a side table: the config
-- belongs to exactly one session, is read on the same row the resume path
-- already loads, and NULL means "no override, use the daemon's static config",
-- so every pre-0080 row keeps its current behaviour with no backfill.
--
-- `permission_mode` is deliberately NOT among them and stays 0079's column.
-- Part 1 pins the mode at session/new and re-asserts it after load precisely
-- because an ambient bypassPermissions silently disables the entire permission
-- surface; a per-session override would be a remote off-switch for the
-- guardrails, reachable by anyone holding `fleet.copilot.configure`. The
-- override covers model, reasoning effort and persona only.
--
-- Growth: three nullable TEXT columns on a table with one row per ACP session,
-- capped by the persona bound below. No new rows, no new index.

-- Adapter model id, e.g. 'claude-sonnet-4-5'. NULL = daemon static config.
ALTER TABLE fleet_acp_session ADD COLUMN model TEXT;

-- Adapter reasoning-effort token. NULL = daemon static config. Not CHECKed
-- against a value list: the vocabulary is the ADAPTER's and differs per
-- provider, so it is validated at the RPC layer against the adapter registry
-- (the 0071 `source` / 0079 `provider` style), never frozen into the schema.
ALTER TABLE fleet_acp_session ADD COLUMN reasoning_effort TEXT;

-- Operator-supplied system prompt. PRIVILEGED: it is a system prompt for an
-- agent holding destructive tools, so it is gated behind
-- `fleet.copilot.configure`, logged to the activity feed on every change, and
-- bounded here as well as at the RPC layer (FLEET_COPILOT_PERSONA_MAX, 8 KiB).
-- The bound is in the schema too because this blob is replayed into every
-- session start and resume, so an unbounded one is unbounded work per spawn,
-- not just one large row. The CAST makes it OCTETS, matching the byte bound
-- the wire const states; bare `length()` on TEXT counts characters, which
-- would admit a 4x larger blob through a multibyte persona.
ALTER TABLE fleet_acp_session ADD COLUMN persona TEXT
    CHECK (persona IS NULL OR length(CAST(persona AS BLOB)) <= 8192);
