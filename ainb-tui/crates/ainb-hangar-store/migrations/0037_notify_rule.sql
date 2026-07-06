-- Hangar v1 schema, migration 0037: per-attention-kind notification routing
-- rules (tcp T5).
--
-- Every attention row (0025) can be fanned out to zero or more PUSH channels on
-- top of always appearing on the control-centre board. T5 makes that fan-out
-- configurable per attention KIND, with a GLOBAL default and optional
-- PER-WORKSPACE overrides. The daemon owns this state (the "daemon owns state"
-- invariant), so it lives in a store table, not a config file — config.toml only
-- carries the bootstrap default a fresh install ships with, and that default is
-- exactly the seed below.
--
-- # Table shape — one nullable-workspace rule table (mirrors `attention`)
--
--   notify_rule(workspace_id?, kind, channels)
--
--   - `workspace_id`  the owning workspace's resolved row id, OR NULL for the
--                     GLOBAL default row (the same nullable-workspace pattern the
--                     `attention` table uses: the FK is enforced only when
--                     non-NULL, so a global row inserts freely). A workspace row
--                     OVERRIDES the global row for the same kind; absence of a
--                     workspace row falls back to global (resolution lives in
--                     `NotifyRuleRepo::resolve`).
--   - `kind`          the attention family, CHECK-constrained to the SAME six the
--                     `attention` table enumerates (0025). A typo would silently
--                     escape routing; the CHECK makes it a hard insert failure.
--   - `channels`      the resolved channel SET as a canonical comma-separated
--                     token string (`phone` / `web` / `os` / `atc`, in that
--                     order), e.g. `web,os`. The EMPTY string is "board-only" (no
--                     push) — a real, valid rule (the `waiting` default), never a
--                     dropped row. Matches `ChannelSet::to_db` / `from_db`.
--
-- # Uniqueness — two PARTIAL unique indexes, not a composite PK
--
-- A composite `PRIMARY KEY (workspace_id, kind)` would NOT enforce one global row
-- per kind: SQLite treats NULLs as DISTINCT in a unique constraint, so two
-- `(NULL, 'error')` rows would both be allowed. Instead:
--
--   - `idx_notify_rule_global`    UNIQUE(kind)         WHERE workspace_id IS NULL
--       → at most ONE global row per kind.
--   - `idx_notify_rule_workspace` UNIQUE(workspace_id, kind) WHERE workspace_id IS NOT NULL
--       → at most ONE row per (workspace, kind).
--
-- Together they make an upsert keyed on "(global|workspace) × kind" well-defined,
-- which the repo's `set` relies on (`ON CONFLICT` over the matching index).
--
-- # Seeded global defaults (the T5 spec defaults)
--
--   escalation           → phone, web, os   (loudest — a human is being paged)
--   ask_user_question    → web, os          (phone opt-in per workspace)
--   approval             → web, os
--   codex_request_user   → web, os
--   error                → os               (local heads-up, no phone/web buzz)
--   waiting              → (board-only)     (no push — the board is enough)
--
-- The channel strings below are already in `ChannelSet` canonical order
-- (`phone,web,os,atc`), so a round-trip through the repo is byte-stable.
--
-- # Attention rows carry their RESOLVED channels (compute-once-at-emit)
--
-- The routing decision is made ONCE, when the attention row is raised, and
-- stamped onto the row (and the `AttentionRaised` event). Every consumer then
-- filters on that stamped set rather than re-resolving — so a rule edit
-- mid-flight can never split-brain the fan-out (one consumer sending, another
-- suppressing the SAME attention). Legacy rows raised before this migration read
-- back the empty default (`''` = board-only): they still show on the board and
-- simply do not retro-push, which is correct for an already-open backlog item.
--
-- `CREATE TABLE` / `CREATE INDEX`, `ALTER TABLE ... ADD COLUMN` with a constant
-- default, and the fixed six-row INSERT are all catalog-or-tiny changes (no table
-- rewrite), so this is safe + O(1) on a populated database, and a re-apply is a
-- no-op via the migrator ledger.

CREATE TABLE notify_rule (
    workspace_id TEXT REFERENCES workspace(id),
    kind         TEXT NOT NULL CHECK (kind IN (
                     'ask_user_question',
                     'approval',
                     'codex_request_user',
                     'error',
                     'waiting',
                     'escalation'
                 )),
    channels     TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_notify_rule_global
    ON notify_rule (kind)
    WHERE workspace_id IS NULL;

CREATE UNIQUE INDEX idx_notify_rule_workspace
    ON notify_rule (workspace_id, kind)
    WHERE workspace_id IS NOT NULL;

-- The global default rows (workspace_id NULL). These are the config.toml bootstrap
-- defaults made durable so the resolver always has a global fallback for every
-- kind, even before any workspace override exists.
INSERT INTO notify_rule (workspace_id, kind, channels) VALUES
    (NULL, 'escalation',         'phone,web,os'),
    (NULL, 'ask_user_question',  'web,os'),
    (NULL, 'approval',           'web,os'),
    (NULL, 'codex_request_user', 'web,os'),
    (NULL, 'error',              'os'),
    (NULL, 'waiting',            '');

-- The resolved channel SET stamped onto each attention row at raise time. NOT
-- NULL DEFAULT '' so an ADD COLUMN on a populated DB is an O(1) catalog change:
-- every legacy row reads back the board-only empty set (it never retro-pushes).
ALTER TABLE attention
    ADD COLUMN channels TEXT NOT NULL DEFAULT '';
