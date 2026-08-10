-- Hangar v1 schema, migration 0085: the live model and reasoning effort of a
-- Fleet session, as its own state group on fleet_session.
--
-- The roster can already say what a session IS doing but not what it is doing
-- it WITH. These four columns carry the provider's own answer to that, observed
-- from the hook payload, the transcript tail, or the Codex thread reply, and
-- delivered ONLY as a FleetSessionPatch through FleetRepo::apply_event so the
-- change bumps `version`, mints a `fleet_event` revision, and reaches every
-- subscriber. A direct UPDATE of these columns would do none of that and is
-- therefore never the right write.
--
-- NULL means NEVER OBSERVED. It does not mean "the default model", and nothing
-- may render it as one: a session whose provider never reported a model shows
-- no model at all. There is deliberately NO backfill; a historical guess is
-- indistinguishable from an observation once it is in the column, and the
-- correct value arrives on the session's next event anyway.
--
-- `model_updated_at` / `model_authority` make this an INDEPENDENT state group
-- rather than a fold into the metadata group. The metadata group is
-- authoritative on every hook, so an inferred producer could never land against
-- it; and a model-only observation must not reset an unrelated group's
-- freshness. They follow the workload / attention / transport precedent exactly,
-- including the 0 / 'inferred' defaults, which are the weakest possible prior so
-- the first real observation always wins its should_replace check.
--
-- No CHECK on the effort vocabulary, for the same reason 0082 gives for
-- fleet_acp_session.reasoning_effort: the vocabulary belongs to the provider and
-- differs between them, so it is validated at the RPC layer against the token
-- sanitiser, never frozen into the schema. Freezing it here would also owe
-- migration_upgrade_full_chain.rs a colliding-data seed for a constraint that
-- buys nothing.
--
-- Growth: four columns on a table with one row per known session, each bounded
-- by the 64-byte token clamp at the write path. No new rows, no new index — the
-- model is read on the row the roster already selects and is never a predicate.

-- Provider-reported model id, verbatim, e.g. 'claude-opus-5' or 'gpt-5.6-terra'.
-- NULL = never observed.
ALTER TABLE fleet_session ADD COLUMN model TEXT;

-- Provider-reported reasoning-effort token, verbatim, e.g. 'high'. NULL = never
-- observed. Not CHECKed; see the header.
ALTER TABLE fleet_session ADD COLUMN reasoning_effort TEXT;

-- Model state-group observation time, epoch milliseconds. 0 = never observed.
ALTER TABLE fleet_session ADD COLUMN model_updated_at INTEGER NOT NULL DEFAULT 0;

-- Model state-group authority, 'authoritative' or 'inferred'. The default is the
-- weakest value so any later observation can replace it.
ALTER TABLE fleet_session ADD COLUMN model_authority TEXT NOT NULL DEFAULT 'inferred';
