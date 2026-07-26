-- Hangar v1 schema, migration 0051: per-agent skill enable/disable toggle.
--
-- Multica parity (gap #24): migrations 161 (agent_skill.enabled) and
-- 206 (agent.disabled_runtime_skills).
--
-- SQLite has no native boolean; `enabled` is an INTEGER constrained to 0/1, the
-- same shape as autopilot.enabled (0009) and atc_instance.enabled (0028).
--
-- ALTER ... ADD COLUMN with a CONSTANT literal default is an O(1) catalog change
-- in SQLite (no table rewrite), safe on a populated DB — the same shape 0047/0050
-- used. DEFAULT 1 means every pre-existing attachment keeps materialising exactly
-- as it did before this migration.
--
-- DEVIATION (D1): `disabled_runtime_skills` is honored at DISPATCH-TIME
-- MATERIALISATION (daemon/src/materialise.rs), not at a live tool registry —
-- hangar has no runtime tool-registry to gate. Same observable outcome: the
-- named skill never reaches the agent.
--
-- DEVIATION (D2): attach does NOT re-enable a disabled link. `attach_to_agent`
-- keeps `ON CONFLICT DO NOTHING` because daemon seed/templates re-attach on every
-- re-run; a re-enabling attach would silently undo an operator's disable.
--
-- DEVIATION (D3): `used_skill_ids` (the Used/Unused filter chips) stays
-- attachment-based, not enablement-based — a disabled link is still attached.

ALTER TABLE agent_skill ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1
    CHECK (enabled IN (0, 1));

-- Multica 206: per-agent runtime-level skill/tool suppression by NAME, distinct
-- from the (agent, skill) junction. JSON array of kebab-case skill names, same
-- persistence shape as issue.labels (0014) / issue.acceptance_criteria (0048).
ALTER TABLE agent ADD COLUMN disabled_runtime_skills TEXT NOT NULL DEFAULT '[]';

-- Partial index over the enabled links only: the dispatch-time read
-- (SkillRepo::skills_for_agent) always filters `enabled = 1`, so a workspace with
-- many disabled attachments never scans them. Mirrors 0009's
-- `WHERE enabled = 1` partial index on autopilot.
CREATE INDEX idx_agent_skill_agent_enabled ON agent_skill (agent_id)
    WHERE enabled = 1;
