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
-- # Only the untouched GLOBAL seed rows, idempotent (mirrors 0038)
--
-- Like 0038, this updates ONLY rows STILL at their exact prior seed value, so an
-- operator who deliberately re-set one of these GLOBAL rules through
-- `notify_rule_set` keeps their edit verbatim (a value other than the seed no
-- longer matches). The exact-value predicate also means a custom board-only
-- (empty-string) global row is left untouched rather than corrupted into a
-- leading-comma `,atc`. Post-0038 the seeded values are `phone,web,os` for
-- ask / approval / codex_request_user and `os` for error; appending `,atc` keeps
-- `ChannelSet` canonical order. The `workspace_id IS NULL` predicate scopes this
-- to the GLOBAL defaults, never a per-workspace override.
--
-- Idempotent: after the first apply the seed-value predicate matches nothing (the
-- rows now carry the trailing `,atc`), and the migrator ledger blocks a re-apply
-- anyway. A data backfill (no schema change), done as a FOLLOW-UP migration (not
-- a 0037/0038 edit) because the embedded `sqlx::migrate!` checksums every applied
-- migration — editing an earlier file in place would break an already-upgraded
-- install on the next boot.

UPDATE notify_rule
   SET channels = channels || ',atc'
 WHERE workspace_id IS NULL
   AND channels NOT LIKE '%atc%'
   AND (
        (kind IN ('ask_user_question', 'approval', 'codex_request_user')
             AND channels = 'phone,web,os')
     OR (kind = 'error' AND channels = 'os')
       );
