-- Hangar v1 schema, migration 0040: fold the ATC feed into the GLOBAL notify
-- defaults for the actionable attention kinds (journeys CH4/CH5 + D12).
--
-- The ATC brain nudges a monitored session only while that session has an
-- attention row routed to the `atc` channel (the heartbeat's channel gate, tcp
-- T5). But 0037/0038 seeded the actionable kinds (ask / approval / codex-request
-- / error) WITHOUT `atc`, so a freshly raised ask resolved its channels at emit
-- time to `phone,web,os` — no `atc` — and the ATC heartbeat filter dropped the
-- session. Actionable attention therefore never reached ATC by default: the
-- nudge loop was silent for exactly the kinds ATC exists to shepherd.
--
-- Fold `atc` into those four GLOBAL defaults so an out-of-the-box install routes
-- actionable attention to the ATC feed. `escalation` already pages a human via
-- phone and is deliberately left off this list; `waiting` stays board-only (a
-- passive state ATC must not be nudged about).
--
-- # Canonical order — append is order-safe
--
-- `channels` is stored in `ChannelSet` canonical order (`phone,web,os,atc`), with
-- `atc` LAST. Appending `,atc` to any existing value therefore yields a
-- byte-stable canonical string (`phone,web,os` -> `phone,web,os,atc`; `os` ->
-- `os,atc`), matching what `ChannelSet::to_db` would emit.
--
-- # Only untouched GLOBAL rows, idempotent
--
-- The `workspace_id IS NULL` predicate scopes this to the GLOBAL defaults, never
-- a per-workspace override an operator set. The `channels NOT LIKE '%atc%'` guard
-- makes the append idempotent (a row already carrying `atc` is skipped) and
-- preserves an operator's deliberate global edit that already added `atc`. This
-- is a data backfill (no schema change), a catalog-tiny UPDATE done as a
-- FOLLOW-UP migration (not a 0037/0038 edit) because the embedded
-- `sqlx::migrate!` checksums every applied migration — editing an earlier file in
-- place would break an already-upgraded install on the next boot.

UPDATE notify_rule
   SET channels = channels || ',atc'
 WHERE workspace_id IS NULL
   AND kind IN ('ask_user_question', 'approval', 'codex_request_user', 'error')
   AND channels NOT LIKE '%atc%';
