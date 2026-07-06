-- Hangar v1 schema, migration 0015: agent archive flag + runtime config knobs.
--
-- At v1 the `agent` table (migration 0002) carried only identity + instructions:
-- there was no way to ARCHIVE an agent (hide it from the active picker without a
-- hard delete) and no place to persist the per-agent runtime configuration the
-- the reference agents carry (parity-review design gap: "create/edit/archive agents +
-- configure runtime/model/args partial"). This migration adds both — the persist
-- + expose half. Wiring these knobs into the provider EXEC is a SEPARATE bead
-- (e38.16); this migration only lands the columns the edit/archive RPCs write.
--
-- Columns added to `agent`:
--
--   - `archived`    a 0/1 flag (INTEGER, the SQLite boolean convention shared by
--                   `autopilot.enabled`, migration 0009). DEFAULT 0 means every
--                   pre-existing and freshly-created agent is ACTIVE; an archived
--                   agent is excluded from the active picker (the repo's
--                   `list_by_workspace` stays active-only and a new
--                   `list_by_workspace_including_archived` returns the full set).
--                   A CHECK keeps the discriminant honest (only 0 or 1).
--
--   - `model`       an optional provider model override (e.g. `claude-opus-4`),
--                   TEXT, nullable — NULL = use the runtime's provider default.
--
--   - `cli_args`    extra provider CLI arguments as a JSON array of strings
--                   (e.g. `["--verbose","--max-turns","5"]`), defaulting to the
--                   empty array `'[]'` — mirrors `issue.labels` (migration 0014).
--
--   - `mcp_config`  the agent's MCP-server configuration as a JSON object
--                   (e.g. `{"servers":{...}}`), defaulting to the empty object
--                   `'{}'` — a map, so the JSON-object shape (vs `cli_args`'s
--                   array) is intentional.
--
--   - `thinking`    an optional reasoning/thinking level token (e.g. `low` /
--                   `medium` / `high`), TEXT, nullable — NULL = provider default.
--
--   - `agent_env`   per-agent environment variables as a JSON object map
--                   (e.g. `{"FOO":"bar"}`), defaulting to the empty object `'{}'`.
--                   These are agent-scoped overrides layered atop the daemon's
--                   ambient env allowlist when EXEC is wired (e38.16).
--
-- ALTER TABLE ... ADD COLUMN with a constant default is an O(1) catalog change
-- in SQLite (no table rewrite), so this is safe on populated databases. Each
-- pre-existing agent reads back the column defaults (archived 0, model NULL,
-- cli_args '[]', mcp_config '{}', thinking NULL, agent_env '{}') until edited.

ALTER TABLE agent
    ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1));

ALTER TABLE agent
    ADD COLUMN model TEXT;

ALTER TABLE agent
    ADD COLUMN cli_args TEXT NOT NULL DEFAULT '[]';

ALTER TABLE agent
    ADD COLUMN mcp_config TEXT NOT NULL DEFAULT '{}';

ALTER TABLE agent
    ADD COLUMN thinking TEXT;

ALTER TABLE agent
    ADD COLUMN agent_env TEXT NOT NULL DEFAULT '{}';
