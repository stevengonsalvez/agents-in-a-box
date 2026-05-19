# Claude Code adapter for reflect-kb

Thin installer that plugs the reflect plugin's skills into a Claude Code
user's `~/.claude/` layout. Part of the v4 cross-harness adapter set alongside
the forthcoming Codex and Copilot adapters (see spec §Phase 2).

## What it does

1. Copies the **full** plugin `SKILL.md` content to `~/.claude/skills/<name>/`
   for each skill exposed by this plugin (`reflect`, `recall`,
   `reflect-status`, `consolidate`, `ingest`). Frontmatter is mutated to set
   `managed_by: reflect-kb/adapters/claude` so subsequent runs (or
   `uninstall`) can recognise the file as adapter-written. The rest of the
   SKILL.md (workflow guidance, examples, output-location rules) is
   preserved byte-for-byte so `/reflect`, `/recall` etc. load identical
   content to the namespaced plugin invocations (`/reflect:reflect`,
   `/reflect:recall`).
2. Merges a `SessionStart` hook entry into `~/.claude/settings.json` that
   runs `session_start_recall.py` on every new Claude Code session. Existing
   hooks are preserved; the merge is idempotent.

## Usage

```bash
# Dry-run (no filesystem changes, prints the plan)
python plugins/reflect/adapters/claude/claude_adapter.py \
    install --dry-run

# Real install
python plugins/reflect/adapters/claude/claude_adapter.py install

# Install only skill files, skip hook merge
python .../claude_adapter.py install --no-hooks

# Re-run after /plugin update to mirror the refreshed plugin content
python .../claude_adapter.py install --force

# Remove adapter-managed files and hook entry (leaves user content alone)
python .../claude_adapter.py uninstall
```

Tests set `--home /tmp/…` to exercise a clean `HOME` without touching the
real user config.

## Design notes

- **Why full content, not pointers:** earlier versions wrote a pointer stub
  whose frontmatter carried `source:` pointing back at the plugin SKILL.md.
  The intent was "edits propagate via the source path." In practice Claude
  Code's skill loader reads the file content directly and does **not**
  dereference the `source:` field — agents loading the skill saw 12 lines of
  "see source" and improvised everything else, including fabricated paths
  (e.g. `~/.reflect/docs/solutions/...`). The adapter now copies the full
  plugin SKILL.md content so the loader sees real workflow guidance.
- **Sync model:** `install --force` is now the canonical refresh after
  `/plugin update`. The plugin's deployed cache holds the source of truth
  (`~/.claude/plugins/cache/agents-in-a-box/reflect/<version>/skills/...`);
  the adapter mirrors it into `~/.claude/skills/`.
- **Idempotency:** re-running `install` never duplicates the
  `SessionStart` hook and never clobbers foreign files (`uninstall` only
  removes files bearing the `managed_by` sentinel; other user edits in the
  same skill dir survive). `--force` is required to overwrite a foreign
  SKILL.md (one without the `managed_by` sentinel — e.g. a hand-written
  v2.x standalone copy).
- **Failure mode:** if `~/.claude/settings.json` is not valid JSON, the
  adapter exits non-zero rather than overwriting it — users who hand-edit
  the file shouldn't lose work.
