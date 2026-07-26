-- Hangar v1 schema, migration 0050: agent metadata columns + name uniqueness.
--
-- Multica parity (gap #23): migrations 046_agent_unique_name, 060_agent_description_length,
-- 163_agent_builder, 172_agent_system_identity_index, 212_agent_service_tier.
--
-- ALTER ... ADD COLUMN with a CONSTANT literal default is an O(1) catalog change in SQLite
-- (no table rewrite), safe on populated DBs — the same shape 0047 used for permission_mode.
--
-- DEVIATION from multica 046: multica DELETEs duplicate-named rows before adding the
-- constraint. Hangar agents are FK-pinned by agent_task_queue / usage / autopilot rows
-- (AgentRepo::delete's HasHistory guard), so a DELETE here would trip a FOREIGN KEY
-- constraint and abort the migration — which bricks daemon boot on a populated home.
-- Duplicates are RENAMED instead (lossless): the id is appended, which is unique by
-- construction, so the rename can never manufacture a fresh collision.
--
-- DEVIATION 2: `service_tier` is STORED + SURFACED ONLY in this milestone — no
-- dispatch-time Codex RPC override reads it yet (exactly how `token_budget` landed in
-- 0042). Do not assume it is wired.
--
-- DEVIATION 3: `kind` / `system_key` ship as schema + store + read-filter support only.
-- No RPC mints a system agent yet; the agent-builder that does is gap #9-rest.

ALTER TABLE agent ADD COLUMN description TEXT NOT NULL DEFAULT ''
    CHECK (length(description) <= 255);
ALTER TABLE agent ADD COLUMN avatar_url TEXT;
ALTER TABLE agent ADD COLUMN kind TEXT NOT NULL DEFAULT 'user'
    CHECK (kind IN ('user', 'system'));
ALTER TABLE agent ADD COLUMN system_key TEXT;
ALTER TABLE agent ADD COLUMN service_tier TEXT;

-- Pre-flight de-collision: keep the FIRST row (lowest rowid) under its name; every later
-- duplicate gets its id appended so the unique index below can be created.
UPDATE agent
   SET name = name || ' (' || id || ')'
 WHERE rowid NOT IN (SELECT MIN(rowid) FROM agent GROUP BY workspace_id, name);

-- Multica 046: one agent name per workspace, so create answers a clear conflict instead of
-- silently making a second identically-named actor the picker cannot disambiguate.
CREATE UNIQUE INDEX agent_workspace_name_unique ON agent (workspace_id, name);

-- Multica 172: a system agent's identity key is unique per (workspace, owner, runtime).
-- Partial (WHERE system_key IS NOT NULL) so the ordinary user agents — all NULL — are
-- unconstrained. Unblocks the agent-builder carrier lookup (gap #9-rest).
CREATE UNIQUE INDEX agent_system_identity_unique
    ON agent (workspace_id, owner_id, runtime_id, system_key)
    WHERE system_key IS NOT NULL;
