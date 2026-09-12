-- Hangar v1 schema, migration 0097: the mutation ledger and the attention
-- fence (spec D18, critique amendments 15-19).
--
-- Three things land together because they are one contract:
--
--   1. `mutation_ledger`, the durable record of every mutation, keyed
--      `(host_id, principal, op_id)`, holding the serialized reply so a retry
--      after a lost reply is a READ, never a second execution.
--   2. `attention.version`, the fence `attention/answer` is fenced on. The
--      table had no optimistic-concurrency column at all, so "the row I read"
--      and "the row I am answering" could only be compared by state.
--   3. `attention.kind = 'delivery_unconfirmed'`, the row a `writing` receipt
--      becomes at boot. Widening a CHECK needs a table rebuild in SQLite, so
--      this migration does the rebuild and carries every existing column,
--      index and row across.
--
-- # The ledger key
--
-- `principal` is `local` on the unix leg and `device:<id>` off-box (R1). It is
-- part of the PRIMARY KEY because an op id from a DIFFERENT principal must not
-- reach this one's row: that is amendment 15's `rejected{op_id_foreign}`, and
-- the `idx_mutation_ledger_host_op` index is what makes the foreign case
-- detectable in one lookup rather than a scan.
--
-- `host_id` is a placeholder column today, defaulted to 'local'. R1 mints a
-- ULID per daemon (`daemon_identity`) and backfills it. It is in the key from
-- the start because retrofitting a column INTO a primary key is a second table
-- rebuild, and this is the cheap moment to pay for it.
--
-- # Retention is two-stage, deliberately
--
-- D18 wants "7 days or 100k rows, whichever first" AND "a retry after eviction
-- returns unknown{op_expired}". A hard DELETE cannot do both: once the row is
-- gone the daemon cannot tell an evicted op from one it has never seen, and
-- would happily execute it a second time. So stage one EXPIRES a row, drops
-- the stored reply, which is all the bulk, and sets `expired = 1`, leaving a
-- ~60-byte tombstone that still answers "you already sent this, I no longer
-- know what happened". Stage two deletes tombstones past a second, wider bound.
-- The storage ceiling is therefore the tombstone cap, not the reply corpus.
--
-- # Cost on a populated database
--
-- CREATE TABLE and its indexes are O(1) catalog changes. The `attention`
-- rebuild copies every row once: spike 8 measured the whole attention corpus at
-- well under the retention sweep's own per-pass cost, and this runs once, at
-- the boot that applies the migration.

CREATE TABLE mutation_ledger (
    -- The daemon this ledger belongs to. 'local' until R1 mints a ULID.
    host_id          TEXT NOT NULL DEFAULT 'local',
    -- 'local' on the unix leg; 'device:<id>' for a paired off-box device.
    principal        TEXT NOT NULL,
    -- The client-minted opaque op id. Never parsed, only compared.
    op_id            TEXT NOT NULL CHECK (length(op_id) > 0 AND length(op_id) <= 128),
    -- The wire method this op id belongs to.
    method           TEXT NOT NULL CHECK (length(method) > 0),
    -- sha256 of the canonical params body, minus the envelope. `adopted`
    -- requires this to match; a different body against a committed row is
    -- `rejected{already_answered_by}` (amendment 18).
    body_fingerprint TEXT NOT NULL CHECK (length(body_fingerprint) > 0),
    -- Which guarantee this method gets (proto `MutationTier`).
    tier             TEXT NOT NULL CHECK (tier IN ('dedupe', 'receipt')),
    -- What happened to the request (proto `MutationStatus`), plus one state
    -- the wire vocabulary has no word for: `in_flight`, the row a claim
    -- inserts BEFORE the handler runs. That row is what serialises two sockets
    -- retrying the same op id, and a row still `in_flight` at boot is a daemon
    -- that died mid-handler, resolved to `unknown{effects_ambiguous}` by the
    -- boot sweep, never re-executed.
    status           TEXT NOT NULL CHECK (status IN (
                         'in_flight', 'accepted', 'rejected', 'unknown'
                     )),
    -- Why, for a non-accepted status.
    reason           TEXT,
    -- The serialized JSON-RPC result, replayed verbatim. NULL while the first
    -- attempt is still running, and NULL again once the row is expired.
    reply            TEXT,
    -- The receipt lifecycle, for a tier-2 mutation only (proto `ReceiptState`).
    receipt_state    TEXT CHECK (receipt_state IN (
                         'claimed', 'writing', 'delivered', 'failed', 'unknown'
                     )),
    -- Operator-facing detail for a failed or unknown receipt.
    receipt_detail   TEXT,
    -- 1 once retention has dropped the reply. The key survives so a retry is
    -- answered `unknown{op_expired}` rather than executed again.
    expired          INTEGER NOT NULL DEFAULT 0 CHECK (expired IN (0, 1)),
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (host_id, principal, op_id)
);

-- Amendment 15's foreign-op-id rule, ENFORCED rather than looked up.
--
-- UNIQUE, not a plain index, and that is the whole mutual exclusion. The
-- primary key contains `principal`, so two principals racing one op id collide
-- on nothing: each inserts under its own key and each is told `Fresh`, and both
-- execute. A SELECT-then-INSERT cannot close that, they are two autocommit
-- statements, and the window between them is exactly where the race lives.
--
-- One op id is one operation on this host, whoever presents it. The second
-- presenter loses the insert and is re-read as `Foreign`, which is what
-- amendment 15 asks for and what the concurrent-claim test pins.
CREATE UNIQUE INDEX idx_mutation_ledger_host_op
    ON mutation_ledger (host_id, op_id);

-- The retention sweep scans oldest-first.
CREATE INDEX idx_mutation_ledger_age
    ON mutation_ledger (created_at);

-- The boot sweep reads exactly the rows still mid-write. A partial index keeps
-- that O(rows actually writing), which is zero on a clean shutdown.
CREATE INDEX idx_mutation_ledger_writing
    ON mutation_ledger (updated_at)
    WHERE receipt_state = 'writing';

-- The same sweep also has to find rows that never reached a terminal status,
-- receipt tier or not: a `dedupe` mutation whose handler committed and whose
-- reply was never recorded is exactly as ambiguous as a `writing` receipt.
CREATE INDEX idx_mutation_ledger_in_flight
    ON mutation_ledger (updated_at)
    WHERE status = 'in_flight';

-- ── attention: the fence column and the seventh kind ────────────────────────
--
-- SQLite cannot widen a CHECK in place, so this is the standard rebuild:
-- create, copy, drop, rename, re-index. Column order and every existing
-- default are preserved so a `SELECT *` reader sees the same shape.

CREATE TABLE attention_new (
    id               TEXT PRIMARY KEY,
    session_id       TEXT NOT NULL,
    cwd              TEXT NOT NULL DEFAULT '',
    workspace_id     TEXT REFERENCES workspace(id),
    kind             TEXT NOT NULL CHECK (kind IN (
                         'ask_user_question',
                         'approval',
                         'codex_request_user',
                         'error',
                         'waiting',
                         'escalation',
                         -- A receipt that was still `writing` when the daemon
                         -- died. The row is NOT reopened (a retry could
                         -- double-type) and NOT closed silently: only an
                         -- operator closes it.
                         'delivery_unconfirmed'
                     )),
    payload          TEXT NOT NULL,
    state            TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'answered')),
    degraded         INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    answered_by      TEXT,
    answer           TEXT,
    answered_at      INTEGER,
    raise_transcript TEXT,
    channels         TEXT NOT NULL DEFAULT '',
    request_key      TEXT,
    -- The D18 fence for `attention/answer`. Bumped on every state change, so a
    -- client that read version N and answers at version N is answering the row
    -- it saw. Existing rows start at 1.
    version          INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1)
);

INSERT INTO attention_new (
    id, session_id, cwd, workspace_id, kind, payload, state, degraded,
    created_at, answered_by, answer, answered_at, raise_transcript, channels,
    request_key, version
)
SELECT
    id, session_id, cwd, workspace_id, kind, payload, state, degraded,
    created_at, answered_by, answer, answered_at, raise_transcript, channels,
    request_key, 1
FROM attention;

DROP TABLE attention;

ALTER TABLE attention_new RENAME TO attention;

CREATE INDEX idx_attention_state_created
    ON attention (state, created_at);

CREATE INDEX idx_attention_session
    ON attention (session_id);

CREATE INDEX idx_attention_open
    ON attention (created_at)
    WHERE state = 'open';

CREATE UNIQUE INDEX idx_attention_open_request_key
    ON attention (session_id, request_key)
    WHERE state = 'open' AND request_key IS NOT NULL;

-- ── notify_rule: the same seventh kind ──────────────────────────────────────
--
-- `attention.kind` and `notify_rule.kind` are two CHECKs over ONE vocabulary,
-- and widening only the first is how a kind becomes raisable but unroutable:
-- `hangar/notify_rule_set` validates against the proto enum, passes, and then
-- dies on a constraint violation that surfaces as a store fault rather than a
-- clean rejection. So the rule table is rebuilt with the same list. No default
-- row is inserted for the new kind: a `delivery_unconfirmed` row is for an
-- operator sitting at a surface, and pushing it to a phone before anyone has
-- asked for that is a decision for whoever turns the routing on.

CREATE TABLE notify_rule_new (
    workspace_id TEXT REFERENCES workspace(id),
    kind         TEXT NOT NULL CHECK (kind IN (
                     'ask_user_question',
                     'approval',
                     'codex_request_user',
                     'error',
                     'waiting',
                     'escalation',
                     'delivery_unconfirmed'
                 )),
    channels     TEXT NOT NULL
);

INSERT INTO notify_rule_new (workspace_id, kind, channels)
SELECT workspace_id, kind, channels FROM notify_rule;

DROP TABLE notify_rule;

ALTER TABLE notify_rule_new RENAME TO notify_rule;

CREATE UNIQUE INDEX idx_notify_rule_global
    ON notify_rule (kind)
    WHERE workspace_id IS NULL;

CREATE UNIQUE INDEX idx_notify_rule_workspace
    ON notify_rule (workspace_id, kind)
    WHERE workspace_id IS NOT NULL;
