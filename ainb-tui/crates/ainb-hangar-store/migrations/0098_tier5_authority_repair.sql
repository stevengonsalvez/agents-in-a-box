-- Hangar v1 schema, migration 0098: unfreeze the tier-5 rows a mislabelled
-- discovery event left claiming authority over state it only inferred (D14).
--
-- # What went wrong
--
-- `tmux_event` built its `fleet_session` patch with
-- `ObservationAuthority::Authoritative`. It was promoted so a provider's
-- terminal status footer could complete the half-empty model pair Codex hooks
-- write, and `apply_patch` stamps ONE authority onto every state group a patch
-- touches. That patch also carries `lifecycle_state`, `attention_state` and
-- `transport_health`, none of which a pane scan can do better than infer, so
-- every discovered row in a live store now records
-- `lifecycle_authority = 'authoritative'` for a reading that was never
-- authoritative.
--
-- # Why that has to be repaired here rather than just going forward
--
-- The daemon now labels the discovery event `inferred`, which is correct, and
-- that correction is what strands the existing rows. `should_replace` ranks
-- authority BEFORE it compares clocks, so an inferred observation over a
-- stored `authoritative` is refused outright; and the reconciler's own
-- `tmux_row_matches` treats an authoritative group as settled, so it does not
-- even emit the event.
--
-- A tier-5 row has no hook behind it by definition. Nothing else will ever
-- write those columns. Left alone, every pane discovered before this upgrade
-- pins at whatever lifecycle, attention and transport it held at the moment of
-- upgrade, permanently, including the UNAVAILABLE flip the missing-pane sweep
-- would otherwise write when the pane goes away. The fleet panel would keep
-- rendering a dead pane as whatever it was doing when the operator upgraded,
-- and nothing would look broken.
--
-- # Scope
--
-- `provider_session_id IS NULL` is the tier-5 test, and it is the strict one: a
-- discovery scan cannot learn a provider's own session id, so a row that has
-- one was written by a hook and its `authoritative` stamp is honest. Checking
-- `management_state` alone would be wrong in both directions, because
-- `apply_hook` promotes a row to `MANAGED` only for Claude and a Codex hook row
-- stays `DEGRADED`. That is the same conflation that let one agent bind another
-- agent's pane, so this migration does not repeat it.
--
-- The model group is deliberately untouched. `tmux_model_observed` is
-- authoritative on purpose: the footer is the running provider printing its own
-- model, which is a direct reading rather than an inference, and it is the only
-- thing that can complete a pair a Codex hook wrote half of.
--
-- Idempotent, and a no-op on a fresh database.

UPDATE fleet_session
SET lifecycle_authority = 'inferred',
    attention_authority = 'inferred',
    transport_authority = 'inferred'
WHERE provider_session_id IS NULL
  AND (
        lifecycle_authority = 'authoritative'
     OR attention_authority = 'authoritative'
     OR transport_authority = 'authoritative'
  );
