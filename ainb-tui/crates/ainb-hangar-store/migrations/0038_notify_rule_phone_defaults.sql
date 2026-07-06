-- Hangar v1 schema, migration 0038: restore phone to the ask / approval /
-- codex_request_user notify defaults (holistic tcp T5 review).
--
-- 0037 seeded these three GLOBAL routing rules to `web,os`, dropping phone. But
-- the fleet bridge (the phone consumer) delivered ASK / approval / codex-request
-- notifications to the phone BEFORE T5 existed, so a bridge user silently stopped
-- receiving them on the upgrade to 0037. Restore the pre-T5 behaviour by folding
-- phone back into the seeded default for those three kinds.
--
-- This is a data backfill, not a schema change — done as a FOLLOW-UP migration
-- (not a 0037 edit) because the embedded `sqlx::migrate!` checksums every applied
-- migration: editing 0037 in place would break an already-upgraded install with a
-- checksum mismatch on the next boot.
--
-- Only rows STILL at the exact 0037 seed value (`web,os`) are updated, so an
-- operator who deliberately re-set one of these GLOBAL rules through
-- `notify_rule_set` keeps their edit; per-workspace override rows are untouched.
-- Idempotent: after the first apply the `channels = 'web,os'` predicate matches
-- nothing (and the migrator ledger blocks a re-apply anyway). `channels` is stored
-- in `ChannelSet` canonical order (`phone,web,os,atc`), so the new value is
-- byte-stable.

UPDATE notify_rule
   SET channels = 'phone,web,os'
 WHERE workspace_id IS NULL
   AND kind IN ('ask_user_question', 'approval', 'codex_request_user')
   AND channels = 'web,os';
