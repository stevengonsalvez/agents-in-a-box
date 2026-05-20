# Codex CLI adapter for reflect-kb

Installs the reflect plugin into Codex CLI's `~/.codex/` layout, mirroring
what `/plugin install reflect@agents-in-a-box` does for Claude Code.

Codex 0.129+ grew first-class hook parity with Claude (SessionStart,
PreCompact, PostCompact, PreToolUse, PostToolUse, PermissionRequest,
UserPromptSubmit, Stop). The adapter wires both default reflect hooks:

| Event | Hook | Effect |
|-------|------|--------|
| `SessionStart` | `recall/hooks/session_start_recall.py` | Inject top-3 relevant learnings into context |
| `SessionStart` | `reflect/hooks/reflect-drain-bg.sh` | Background-drain queued reflections (shells out to `claude -p`) |
| `PreCompact`   | `reflect/hooks/precompact_reflect.py --auto --verbose` | Capture learnings before context compaction |

Hooks land in `~/.codex/hooks.json` (codex's analogue of Claude's
`~/.claude/settings.json` hooks block — same nested matcher/hooks/command
structure).

## Usage

```bash
python codex_adapter.py install --dry-run    # preview
python codex_adapter.py install              # full install: skills + hooks
python codex_adapter.py install --no-hooks   # skill content only, skip hooks.json
python codex_adapter.py install --no-bg-drain  # SessionStart-recall only (no drain script)
python codex_adapter.py install --force      # replace hand-written sibling SKILL.md files
python codex_adapter.py uninstall            # remove only adapter-managed entries
```

## What gets deployed

```
~/.codex/
├── hooks.json                       # SessionStart + PreCompact entries (merged)
└── skills/
    ├── reflect/
    │   ├── SKILL.md                 # full plugin content + managed_by sentinel
    │   ├── hooks/                   # plugin-level hooks (drain, precompact)
    │   ├── scripts/                 # plugin-level scripts
    │   ├── assets/
    │   ├── references/
    │   └── reflect.toml
    ├── recall/
    │   ├── SKILL.md
    │   ├── hooks/                   # session_start_recall.py
    │   └── scripts/
    ├── reflect-status/SKILL.md
    ├── consolidate/SKILL.md
    └── ingest/SKILL.md
```

Plugin-level shared content (the top-level `hooks/`, `scripts/`,
`assets/`, `references/` and `reflect.toml`) lands under the `reflect`
umbrella skill, matching the layout the hook commands in `hooks.json`
expect.

## Safety

* Each adapter-installed SKILL.md carries
  `managed_by: reflect-kb/adapters/codex` in its frontmatter. Uninstall
  refuses to touch any SKILL.md missing that sentinel.
* `install` refuses to overwrite an existing hand-written SKILL.md
  unless `--force` is passed.
* `install` refuses to overwrite a corrupt `hooks.json` (exit 2 with
  the path in stderr) rather than silently rewriting it.
* Legacy `{{HOME_TOOL_DIR}}` literal entries from older buggy installs
  are swept out on the next install.

## Why this differs from the Claude adapter

Claude Code has a plugin runtime (`/plugin install`) that extracts the
whole plugin tree under `~/.claude/plugins/<name>/` and auto-wires hooks
via the plugin's `plugin.json`. Codex has no plugin runtime equivalent,
so this adapter physically copies the skill content into
`~/.codex/skills/` and merges hook entries into `hooks.json` itself.

## Caveat: drain script still needs `claude`

`reflect-drain-bg.sh` shells out to `claude -p /reflect <transcript>` to
turn queued transcripts into learning documents. On a codex-only machine
without `claude` on PATH, the drain logs a warning and exits 0 (so the
SessionStart hook never blocks codex startup). Pass `--no-bg-drain` to
omit the drain hook entirely in that case.
