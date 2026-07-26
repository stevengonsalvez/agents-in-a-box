-- Hangar v1 schema, migration 0052: archive AUDIT TRAIL on agent + squad.
--
-- Multica parity (gap #26): migration 031 (agent.archived_at / archived_by) and
-- 085 (the same pair on squad). Before this migration hangar recorded THAT an
-- agent was archived (0015's `agent.archived` 0/1) but never WHO or WHEN, and
-- could not archive a squad at all.
--
-- DEVIATION (D1): `agent.archived` stays the AUTHORITATIVE discriminant.
-- Multica treats `archived_at IS NOT NULL` as the archived truth; here the 0/1
-- flag is already load-bearing in `list_by_workspace`, `runtime_agent_ids`,
-- `squad_briefing`, `bootstrap` and `search`. Re-basing truth on a nullable
-- column would be a large unrelated refactor AND would silently reclassify every
-- legacy `archived = 1` row (whose `archived_at` is NULL). `archived_at` /
-- `archived_by` are the audit SIDECAR. `squad` gets the same shape for symmetry.
--
-- DEVIATION (D2): `archived_by` is TEXT holding a canonical ACTOR-REF
-- (`member:<user.id>` / `agent:<id>`), not a `user(id)` FK. This is hangar's
-- polymorphic-actor convention (`comment.author`, `issue.reporter`), and a squad
-- or agent may in future be archived by an automation actor. FK-less by design,
-- matching `squad.leader_id`.
--
-- DEVIATION (D3): archiving stays IDEMPOTENT (multica answers 409 on an
-- already-archived agent). Re-archiving RE-STAMPS `archived_at` / `archived_by`
-- (last-archiver wins), which is still an honest audit record, and preserves the
-- existing `AgentRepo::set_archived` contract.
--
-- NO BACKFILL. A pre-0052 `agent.archived = 1` row keeps `archived_at IS NULL`
-- and `archived_by IS NULL` — an honest "unknown", never a fabricated `now()`
-- stamp for a historical archive. There is deliberately NO CHECK tying
-- `archived = 1` to a non-null `archived_at`, precisely because legacy rows
-- would violate it.
--
-- All five statements are `ALTER TABLE ... ADD COLUMN` with constant defaults →
-- O(1) catalog changes in SQLite (no table rewrite), safe on a populated DB, the
-- same shape 0047/0050/0051 used.
--
-- `archived_at` is epoch MILLISECONDS (`HangarClock::now_ms`), matching every
-- other hangar timestamp column (`squad.created_at`, `task.*_at`).

ALTER TABLE agent ADD COLUMN archived_at INTEGER;
ALTER TABLE agent ADD COLUMN archived_by TEXT;

ALTER TABLE squad ADD COLUMN archived INTEGER NOT NULL DEFAULT 0
    CHECK (archived IN (0, 1));
ALTER TABLE squad ADD COLUMN archived_at INTEGER;
ALTER TABLE squad ADD COLUMN archived_by TEXT;
