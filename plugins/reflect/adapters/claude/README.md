# Claude Code adapter for reflect-kb

Thin installer that plugs the reflect plugin's skills into a Claude Code
user's `~/.claude/` layout. Part of the v4 cross-harness adapter set alongside
the forthcoming Codex and Copilot adapters (see spec §Phase 2).

## What it does

1. Writes lightweight pointer `SKILL.md` files to `~/.claude/skills/<name>/`
   for each skill exposed by this plugin (`reflect`, `recall`,
   `reflect-status`, `consolidate`, `ingest`). The pointer preserves the
   upstream `name` / `description` so skill discovery works, and carries a
   `managed_by: reflect-kb/adapters/claude` marker so subsequent runs (or
   `uninstall`) can recognise its own files.
2. Merges a `SessionStart` hook entry into `~/.claude/settings.json` that
   runs `session_start_recall.py` on every new Claude Code session. Existing
   hooks are preserved; the merge is idempotent.

## Usage

```bash
# Dry-run (no filesystem changes, prints the plan)
python toolkit/packages/plugins/reflect/adapters/claude/claude_adapter.py \
    install --dry-run

# Real install
python toolkit/packages/plugins/reflect/adapters/claude/claude_adapter.py install

# Install only pointers, skip hook merge
python .../claude_adapter.py install --no-hooks

# Remove adapter-managed pointers and hook entry (leaves user content alone)
python .../claude_adapter.py uninstall
```

Tests set `--home /tmp/…` to exercise a clean `HOME` without touching the
real user config.

## Design notes

- **Why pointers, not copies:** the canonical `SKILL.md` lives with the
  plugin source. Copying the whole tree would double-source the content and
  require a reinstall on every skill update. Pointer files reference the
  source path so edits propagate.
- **Idempotency:** re-running `install` never duplicates the
  `SessionStart` hook and never clobbers foreign files (`uninstall` only
  removes files bearing the `managed_by` sentinel; other user edits in the
  same skill dir survive).
- **Failure mode:** if `~/.claude/settings.json` is not valid JSON, the
  adapter exits non-zero rather than overwriting it — users who hand-edit
  the file shouldn't lose work.
