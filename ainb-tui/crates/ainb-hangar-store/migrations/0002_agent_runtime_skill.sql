-- Hangar v1 schema, migration 0002: agent runtimes, agents, and skills.
--
-- `agent_runtime` is a registered provider endpoint (a daemon advertising a
-- provider such as `claude`/`codex`) within a workspace. An `agent` ALWAYS
-- binds to exactly one runtime — `agent.runtime_id` is NOT NULL by the Multica
-- pattern: an agent with no place to run is meaningless, so the FK is required
-- rather than nullable.
--
-- `skill` + `skill_file` + `agent_skill` model reusable instruction bundles
-- (Anthropic-style skills): a `skill` carries top-level `content`, owns zero or
-- more `skill_file` rows (composite PK `(skill_id, path)`), and is attached to
-- agents through the `agent_skill` join (composite PK `(agent_id, skill_id)`).
--
-- All timestamps are epoch milliseconds stored as INTEGER (see 0001).

CREATE TABLE agent_runtime (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    daemon_id    TEXT NOT NULL,
    provider     TEXT NOT NULL,
    runtime_mode TEXT NOT NULL CHECK (runtime_mode IN ('local','cloud')),
    last_seen_at INTEGER,
    status       TEXT NOT NULL DEFAULT 'offline'
);

-- One runtime row per (workspace, daemon, provider) tuple: a given daemon
-- advertises each provider at most once within a workspace.
CREATE UNIQUE INDEX idx_agent_runtime_workspace_daemon_provider
    ON agent_runtime(workspace_id, daemon_id, provider);

CREATE TABLE agent (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    name         TEXT NOT NULL,
    runtime_id   TEXT NOT NULL REFERENCES agent_runtime(id),
    instructions TEXT,
    visibility   TEXT NOT NULL CHECK (visibility IN ('workspace','private')),
    owner_id     TEXT NOT NULL REFERENCES user(id)
);

CREATE TABLE skill (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    name         TEXT NOT NULL,
    description  TEXT,
    content      TEXT
);

CREATE TABLE skill_file (
    skill_id TEXT NOT NULL REFERENCES skill(id),
    path     TEXT NOT NULL,
    content  TEXT,
    PRIMARY KEY (skill_id, path)
);

CREATE TABLE agent_skill (
    agent_id TEXT NOT NULL REFERENCES agent(id),
    skill_id TEXT NOT NULL REFERENCES skill(id),
    PRIMARY KEY (agent_id, skill_id)
);
