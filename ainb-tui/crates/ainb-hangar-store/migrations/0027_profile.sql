-- Hangar v1 schema, migration 0027: the `profile` INDEX table (P5, D14-D16 /
-- architecture §4.1 `profile(slug, tier, mtime)` + tech-register T6).
--
-- Agent profiles are **files on disk** — canonical Claude-subagent `.md` masters
-- under `~/.agents-in-a-box/profiles/<slug>.md` (human-editable, git-able, matching the
-- `~/.claude/agents` workflow). This table is deliberately only an INDEX over
-- that filesystem, not the storage: the daemon's fs-watch reconciler keeps one
-- row per master so a surface can list/resolve profiles over RPC without a
-- directory scan, and so a stale row is dropped when its file disappears. The
-- authoritative body always lives on disk (T6: DB-only rows were rejected).
--
-- Columns:
--
--   - `slug`   the profile identity and PRIMARY KEY — the file stem, the board
--              assignee slug (D16), and the `[profiles.<slug>]` / `<slug>.md`
--              compile-target name. Kebab-case, validated by the core parser
--              (`ainb_hangar_core::profile::is_valid_slug`) before an upsert.
--   - `tier`   the logical model tier (D15), CHECK-constrained to the three the
--              vocabulary enumerates. Free TEXT would let a typo escape the
--              tier→model resolution; the CHECK makes an unknown tier a hard
--              insert failure at the boundary. Mirrors the resolved-per-provider
--              band (`premium`→opus/gpt-5, `balanced`→sonnet/gpt-5-codex,
--              `fast`→haiku/gpt-5-mini).
--   - `mtime`  the master file's last-modified time (epoch milliseconds). The
--              fs-watch reconciler compares it against the on-disk mtime to skip
--              re-reading an unchanged master, and a surface can show "edited N
--              ago". INTEGER like every Hangar timestamp.
--
-- Profiles are HOST-scoped, not workspace-scoped: a single master drives runs in
-- any workspace (D16 — the assignee slug is the profile slug regardless of
-- tenant), so there is deliberately no `workspace_id` column or FK here. The slug
-- PRIMARY KEY is the sole engine-enforced invariant; slug shape + tier validity
-- are enforced by the core parser before the row is written.
--
-- ADD TABLE with no backfill is an O(1) catalog change in SQLite, so this is safe
-- on a populated database: the index simply starts empty and the daemon's first
-- reconcile pass fills it from whatever masters already exist on disk.
-- Re-applying is a no-op via the migrator's version ledger.

CREATE TABLE profile (
    slug  TEXT PRIMARY KEY,
    tier  TEXT NOT NULL CHECK (tier IN ('premium', 'balanced', 'fast')),
    mtime INTEGER NOT NULL
);
