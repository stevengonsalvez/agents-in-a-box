-- Hangar v1 schema, migration 0099: the D14 status identity that #934 deferred.
--
-- #934 landed the apply transaction, the normalizers, retention and the
-- authority repair, and declared two gaps. This closes both: the stored
-- evidence identity a row's tier and clocks come from (#960), and the
-- written-once pane binding that stops send-keys typing into a reused pane
-- (#961). They are one migration because the second needs the first: a binding
-- decision is only trustworthy if the row can say which incarnation made it.
--
-- # Why `tier` is stored and `provenance` is not
--
-- `agent_status::tier_of` derives a tier from `management_state` and
-- `provider_session_id`, which can only ever reach three of the six: hook, acp
-- and pane_text. A row written by an OSC frame, the process table or the
-- transcript is indistinguishable from a pane scrape, and no amount of reading
-- the row afterwards recovers what wrote it. Storing it is the only fix, and it
-- has to land before T0-section renders from the column.
--
-- `provenance` is NOT stored, deliberately, and this is a documented deviation
-- from the issue text. `fleet_session.provenance` ALREADY EXISTS on this table
-- with an unrelated meaning (`authoritative` / `inferred`, the observation
-- authority), so a D14 provenance column would be the second column of that
-- name on one row. It is also a pure function of the tier
-- (`agent_status::provenance_of`), so storing it would be a second source for
-- one fact, which is what this phase exists to remove. The derivation stays the
-- single mapping.
--
-- # The tier vocabulary
--
-- `unknown` is a real member here and is the DEFAULT, which is the whole point
-- of the backfill. Every row written before this migration was written by code
-- that recorded no tier, so the honest value is "nobody knows", not a tier
-- reconstructed from columns that cannot carry the answer. `tier_of` reads the
-- column and falls back to its old derivation only for `unknown`, so existing
-- rows keep exactly today's behaviour and new rows get the truth.
--
-- # `host_id`
--
-- Placeholder defaulted to `local`, the same shape and for the same reason as
-- `mutation_ledger.host_id` in 0097: R1 mints a real `HostId` ULID and
-- backfills. Present now so the `(host_id, session_key, tier, event_id)`
-- idempotency key the spec names can be written today rather than needing a
-- second rebuild of the event table later.
--
-- # Why `fleet_event.event_id` keeps its own UNIQUE
--
-- The spec's key is `(host_id, session_key, tier, event_id)`. That is WIDER
-- than the existing `event_id UNIQUE`, so honouring it exactly would mean
-- dropping a constraint, which in SQLite means rebuilding the table. Not done
-- here, on purpose: `fleet_event` carries the payloads that the 1 GB ceiling
-- and the rate-aware eviction path exist to bound, so it is the one table in
-- this schema that can be measured in gigabytes, and a rebuild of it at boot is
-- a multi-minute upgrade with the daemon down.
--
-- Keeping the narrower constraint is safe in the only direction that matters:
-- it is a SUPERSET guarantee. Every duplicate the spec key would reject is
-- already rejected, and the tuple is recorded and indexed so a reader can key
-- on it. What it costs is the ability to store the same `event_id` twice at
-- different tiers, which no producer does: every tier mints its own id space
-- (`att:<session>:<event_id>`, `tmux:discovered:...`, `tmux:model:...`).

ALTER TABLE fleet_session ADD COLUMN host_id TEXT NOT NULL DEFAULT 'local';

-- The evidence tier that last wrote this row's state.
ALTER TABLE fleet_session ADD COLUMN tier TEXT NOT NULL DEFAULT 'unknown'
    CHECK (tier IN (
        'hook', 'acp_feed', 'osc_frame', 'process', 'transcript', 'pane_text',
        'unknown'
    ));

-- The daemon's own clock when it took delivery of the evidence. Distinct from
-- `last_observed_at`, which is the SOURCE's clock: a replay moves this and must
-- never move that.
ALTER TABLE fleet_session ADD COLUMN received_at INTEGER NOT NULL DEFAULT 0;

-- When `state` last CHANGED, for "working since" and attention ordering. A
-- repeated observation of the same state must not move it, or every surface
-- reports a session that just started working every time it is re-observed.
ALTER TABLE fleet_session ADD COLUMN state_started_at INTEGER NOT NULL DEFAULT 0;

-- The fence. `process_start_fingerprint` for a tmux pane, the pool session id
-- for an ACP child. An event whose incarnation differs from the live row is a
-- restart (a new row) or a suppression (an older incarnation), never an
-- in-place update of a row that belongs to a process that has gone.
ALTER TABLE fleet_session ADD COLUMN session_incarnation TEXT;

-- Hydrated at boot and not yet confirmed by a tier 0/1 event in THIS daemon
-- incarnation. A restored row is a memory of what was true before the restart,
-- and rendering it as fact is how a dead session keeps showing as working.
ALTER TABLE fleet_session ADD COLUMN restored_unconfirmed INTEGER NOT NULL DEFAULT 0
    CHECK (restored_unconfirmed IN (0, 1));

-- The pane binding as a WRITTEN-ONCE decision (#961).
--
-- `tmux_target` is the live routing field and is cleared on invalidation.
-- These three record the decision that produced it: which pane was chosen,
-- which process was in it at the time, and when. Without the fingerprint there
-- is nothing to re-confirm against, so a correlated binding survived pane reuse
-- and send-keys typed into whichever agent holds the pane now.
ALTER TABLE fleet_session ADD COLUMN bound_target TEXT;
ALTER TABLE fleet_session ADD COLUMN bound_fingerprint TEXT;
ALTER TABLE fleet_session ADD COLUMN bound_at INTEGER NOT NULL DEFAULT 0;

ALTER TABLE fleet_event ADD COLUMN host_id TEXT NOT NULL DEFAULT 'local';
ALTER TABLE fleet_event ADD COLUMN tier TEXT NOT NULL DEFAULT 'unknown'
    CHECK (tier IN (
        'hook', 'acp_feed', 'osc_frame', 'process', 'transcript', 'pane_text',
        'unknown'
    ));

-- The spec's idempotency key, recorded and indexed. Deliberately NOT UNIQUE:
-- `UNIQUE(event_id)` already implies uniqueness of any tuple containing it, so
-- a unique index here would enforce nothing and still cost a second b-tree
-- write per insert on the highest-volume table in the schema. It exists so a
-- reader can key on the tuple, which is what the spec asks for.
CREATE INDEX IF NOT EXISTS idx_fleet_event_identity
    ON fleet_event (host_id, session_key, tier, event_id);

-- The binding invalidation sweep and `ainb doctor` both ask "which rows claim a
-- pane", and the answer is a small slice of a large table.
CREATE INDEX IF NOT EXISTS idx_fleet_session_bound_target
    ON fleet_session (bound_target) WHERE bound_target IS NOT NULL;
