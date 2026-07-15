-- Hangar v1 schema, migration 0041: per-agent provider override.
--
-- Until now an agent had no provider of its own: the daemon resolves which
-- provider exec path a task takes from the agent's RUNTIME (`agent_runtime.provider`),
-- and the single self-registered host runtime advertises `claude`. The
-- `hangar/agent_create` surface (fresh-home bootstrap) lets a user pick a
-- provider (`claude`/`codex`/`copilot`) at create time, so the choice must be
-- recorded ON the agent row rather than inferred from the shared runtime.
--
-- `provider` is nullable TEXT (no default): NULL means "no per-agent override —
-- use the runtime's advertised provider", so every pre-existing agent keeps its
-- current behaviour untouched. A freshly-created agent records the chosen
-- provider verbatim. Following migration 0015's precedent, an ALTER TABLE ...
-- ADD COLUMN with no non-constant default is an O(1) catalog change (no table
-- rewrite) and is non-destructive.
ALTER TABLE agent
    ADD COLUMN provider TEXT;
