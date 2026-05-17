# Codex CLI adapter for reflect-kb

Thin installer that plugs the reflect plugin's skills into Codex CLI's
`~/.codex/` layout. Sister to the Claude adapter, but invocation-only:
Codex has no SessionStart hook parity, so the adapter only writes pointer
skill files — users invoke `/recall`, `/reflect` etc. manually (per v4
spec §"Invocation Per Harness").

## Usage

```bash
python codex_adapter.py install --dry-run    # preview
python codex_adapter.py install              # write ~/.codex/skills/*/SKILL.md
python codex_adapter.py uninstall            # remove only adapter-managed files
```

The pointer carries a `managed_by: reflect-kb/adapters/codex` sentinel so
`uninstall` never touches hand-written sibling files in the same skill
directory.
