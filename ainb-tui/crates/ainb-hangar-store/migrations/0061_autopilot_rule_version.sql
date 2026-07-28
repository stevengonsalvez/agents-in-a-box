-- Hangar v1 schema, migration 0061: AUTOPILOT RULE VERSIONING + HUMAN
-- ATTRIBUTION (multica parity #14).
--
-- multica's `autopilot_rule_version` (migrations 186/187) is the ACCOUNTABILITY
-- LEDGER for an unattended automation: one append-only row per SUBSTANTIVE
-- publish (create / enable / resume / target change / instructions change),
-- each naming the accountable human and snapshotting the rule as published.
-- Dispatch reads the NEWEST row per rule to answer "who is accountable for
-- this unattended run". hangar had none of it: no versioning table, no
-- `published_by`, and (until this migration's companion repo work) no edit
-- surface at all -- an autopilot's cron / instructions / agent could not be
-- changed without hand-editing sqlite.
--
-- FIVE DELIBERATE SCHEMA DECISIONS:
--
-- 1. NO CHECK ON `change_kind`. Same rule 0058/0059 established: SQLite cannot
--    widen a CHECK without a full table rebuild, and this vocabulary is
--    append-only by design. The domain lives in Rust
--    (`ainb_hangar_core::autopilot::rule_version::RuleChangeKind`) whose parse
--    is TOLERANT -- an unknown token written by a newer daemon decodes to None
--    and renders as raw text rather than poisoning the read path.
--
-- 2. FK ONLY ON `workspace_id`. `autopilot_id` and `published_by` carry no FK:
--    the row is a historical FACT that must outlive the rule it describes
--    (deleting an autopilot must not erase who was accountable for its past
--    runs), and a best-effort recorder must never fail on a race with a
--    concurrent delete. Identical reasoning to `activity_log` (0059 decision 3)
--    and `dispatch_attempt` (0058).
--
-- 3. `config_summary` IS JSON TEXT, NOT JSONB. SQLite has no JSONB type. No
--    `json_valid` CHECK -- the only writer is `serde_json::to_string` on a
--    `Value::Object`, and a CHECK would be a rebuild-blocker for no gain (0059
--    decision 4).
--
-- 4. NOT TRIMMED. This is the accountability ledger; trimming would erase the
--    beginning of a rule's provenance. Growth is bounded by edit frequency
--    (single-digit rows per rule per year in practice).
--
-- 5. NO BACKFILL FOR EXISTING AUTOPILOTS. A pre-0061 autopilot gets NO v1 row.
--    Inventing `published_by = 'member:me'` + `created_at = now()` for a rule
--    created months ago would be a FABRICATED audit record. The read path
--    renders "unversioned" for such a rule, and the first substantive edit
--    after upgrade mints v1 (`MAX(version)+1` over an empty set = 1). Same
--    honesty rule 0052 applied to `archived_at`: an honest unknown, never a
--    fabricated now() stamp.
--
-- `published_by` is a canonical actor-ref TEXT (`member:<id>` / `agent:<id>`,
-- `ainb_hangar_core::actor::ActorRef`), the shape `comment.author`,
-- `issue.reporter` and `agent.archived_by` (0052 deviation D2) already use --
-- deliberately NOT multica's two-column `published_by_type`/`published_by_id`
-- polymorphism. NULL means unattributed (a legacy/automated caller supplied no
-- actor), never a fabricated human.
--
-- DELIBERATELY NOT PORTED: multica's separate `autopilot_trigger` table and its
-- per-trigger publisher (migration 189). hangar collapses trigger config into
-- columns on `autopilot` (0018 webhook, 0057 api), so arming/disarming a
-- trigger is a SUBSTANTIVE publish on the RULE's version chain instead.
--
-- `created_at` is epoch MILLISECONDS (`HangarClock::now_ms`), matching every
-- other hangar timestamp column.

CREATE TABLE autopilot_rule_version (
    id              TEXT PRIMARY KEY,                 -- ULID
    workspace_id    TEXT NOT NULL REFERENCES workspace(id),
    autopilot_id    TEXT NOT NULL,                    -- NO FK, see decision 2
    version         INTEGER NOT NULL,                 -- 1-based, monotonic per autopilot
    change_kind     TEXT NOT NULL,                    -- RuleChangeKind::as_db_str()
    published_by    TEXT,                             -- actor ref; NULL = unattributed
    config_summary  TEXT NOT NULL DEFAULT '{}',       -- serialised JSON object
    created_at      INTEGER NOT NULL                  -- epoch millis
);

-- The monotonic-sequence guard: two concurrent writers cannot mint the same
-- version. SQLite serialises writers, so the loser sees this constraint and the
-- whole mutation transaction rolls back -- correct, not silently duplicated.
CREATE UNIQUE INDEX idx_autopilot_rule_version_seq
    ON autopilot_rule_version(autopilot_id, version);

-- Serves the dispatch-time "newest version for this rule" read (multica 187).
CREATE INDEX idx_autopilot_rule_version_latest
    ON autopilot_rule_version(workspace_id, autopilot_id, version DESC);

-- ── Run attribution (multica's `originator` vs accountable-human fork) ──────
--
-- The accountable human for THIS run. For an unattended fire (cron / webhook /
-- api) this is the newest rule version's `published_by` (multica's
-- `rule_owner`); for a manual "run now" it is the human who clicked (multica's
-- `direct_human`). NULL when neither is resolvable -- an honest unknown, never
-- a fabricated actor. multica keeps its `originator_user_id` NULL for
-- unattended fires on purpose; that decoupling is exactly what these two
-- columns encode.
--
-- `ALTER TABLE ... ADD COLUMN` with no default is O(1) catalog-only on SQLite:
-- no table rewrite, and crucially no repeat of 0057's rebuild dance (which was
-- only needed to WIDEN a CHECK). Every pre-0061 run row reads NULL/NULL:
-- unknown, not misattributed.
ALTER TABLE autopilot_run ADD COLUMN accountable_actor TEXT;

-- HOW the accountable actor was resolved: 'rule_owner' | 'direct_human'.
-- NO CHECK (decision 1). NULL iff `accountable_actor` IS NULL.
ALTER TABLE autopilot_run ADD COLUMN attribution TEXT;
