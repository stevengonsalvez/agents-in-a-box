-- Hangar v1 schema, migration 0047: agent invocation-permission system.
--
-- Multica migration 130 parity (gap #8). Splits "who may INVOKE an agent" out of
-- the inert `visibility` label into an explicit, extensible model:
--   * `agent.permission_mode` is the AUTHORITATIVE invoke-permission source
--     ('private' = owner-only deny-by-default; 'public_to' = the allow-list decides).
--   * `agent_invocation_target` is the FK-less allow-list of rows a `public_to`
--     agent admits (a workspace, a specific member, or a reserved team).
-- `visibility` is left in place and kept in sync by the store as a DERIVED legacy
-- field so nothing that still reads it regresses.
--
-- ALTER ... ADD COLUMN with a CONSTANT literal default is an O(1) catalog change in
-- SQLite (no table rewrite), safe on populated DBs. Every pre-existing agent reads
-- back permission_mode = 'private' before the backfill below runs.

ALTER TABLE agent
    ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'private'
        CHECK (permission_mode IN ('private', 'public_to'));

-- Allow-list rows for public_to agents. FK-less by design (matches hangar's
-- (actor_type, actor_id) convention in the actor module and the hot queue tables):
-- agent_id / created_by / member target_id referential integrity is maintained in
-- the application layer. A `workspace` target stores the agent's workspace_id in
-- target_id (NOT NULL) so the UNIQUE constraint dedups (SQL treats NULLs as
-- distinct, which would defeat it).
CREATE TABLE agent_invocation_target (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT NOT NULL,
    target_type TEXT NOT NULL CHECK (target_type IN ('workspace', 'member', 'team')),
    target_id   TEXT NOT NULL,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    UNIQUE (agent_id, target_type, target_id)
);

CREATE INDEX agent_invocation_target_agent_id_idx
    ON agent_invocation_target (agent_id);
CREATE INDEX agent_invocation_target_target_idx
    ON agent_invocation_target (target_type, target_id);

-- Lossless backfill from the legacy visibility label: a 'workspace'-visible agent
-- becomes public_to with one workspace-scoped allow row; a 'private' agent stays
-- private with no row. The backfill row's id is minted via randomblob (these rows
-- are created outside app code); created_at is epoch-ms via strftime (SQLite bans a
-- non-constant column DEFAULT, but a literal in an INSERT is fine).
UPDATE agent SET permission_mode = 'public_to' WHERE visibility = 'workspace';

INSERT INTO agent_invocation_target (id, agent_id, target_type, target_id, created_by, created_at)
SELECT lower(hex(randomblob(16))), id, 'workspace', workspace_id, NULL,
       CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM agent
WHERE visibility = 'workspace';
