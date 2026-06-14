-- Hangar v1 schema, migration 0020: per-workspace config (e38.21).
--
-- At v1 the `workspace` table (migration 0001) carried only identity:
-- `id` / `slug` / `name` / `created_at`. There was no place to persist the
-- per-workspace agent-run configuration the reference workspaces carry (parity-review
-- design gap: "per-workspace context prompt + repo whitelist + issue prefix"):
--   - a CONTEXT PROMPT injected into every agent run in the workspace (written
--     into the per-task execenv as a `CLAUDE.md` so the agent actually sees it),
--   - a REPO WHITELIST gating which repositories a workspace task may check out,
--   - an ISSUE PREFIX applied to the title of every issue created in the
--     workspace (e.g. `[OPS] fix the build`).
--
-- This migration adds the persist + expose half — the three columns the
-- `hangar workspace config` CLI/RPC surface writes and dispatch/issue-create
-- read. All three are nullable TEXT (NULL = "not configured", the pre-existing
-- behaviour every upgrading workspace keeps):
--
--   - `context_prompt`  free-text agent context, TEXT, nullable. NULL = no
--                       `CLAUDE.md` is written into a task's execenv (the v1
--                       behaviour: the agent runs with no per-workspace context).
--
--   - `repo_whitelist`  the allowed repositories as a JSON array of strings
--                       (e.g. `["org/api","org/web"]`), TEXT, nullable. NULL =
--                       "no whitelist configured" (no gate). A repo-checkout flow
--                       does not exist yet, so this column is persisted + exposed
--                       + validated here; the gate point is documented in the
--                       repo layer for the bead that lands checkout.
--
--   - `issue_prefix`    a short prefix prepended to a newly-created issue's title,
--                       TEXT, nullable. NULL = no prefix (the v1 title is used
--                       verbatim).
--
-- ALTER TABLE ... ADD COLUMN with no default (or a constant default) is an O(1)
-- catalog change in SQLite (no table rewrite), so this is safe on populated
-- databases. Each pre-existing workspace reads back NULL for all three columns
-- until configured — its agent runs and issue titles are byte-identical to the
-- pre-0020 behaviour.

ALTER TABLE workspace
    ADD COLUMN context_prompt TEXT;

ALTER TABLE workspace
    ADD COLUMN repo_whitelist TEXT;

ALTER TABLE workspace
    ADD COLUMN issue_prefix TEXT;
