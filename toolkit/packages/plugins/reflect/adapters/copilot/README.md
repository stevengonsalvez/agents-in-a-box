# GitHub Copilot adapter for reflect-kb

Thin installer that plugs the reflect plugin's skills into Copilot's
`~/.copilot/` layout. Sister to the Claude and Codex adapters, but
invocation-only: Copilot has no SessionStart hook system, so the adapter
only writes pointer skill files — users invoke `/recall`, `/reflect` etc.
manually (per v4 spec §"Invocation Per Harness").

## Usage

```bash
python copilot_adapter.py install --dry-run    # preview
python copilot_adapter.py install              # write ~/.copilot/skills/*/SKILL.md
python copilot_adapter.py uninstall            # remove only adapter-managed files
```

The pointer carries a `managed_by: reflect-kb/adapters/copilot` sentinel so
`uninstall` never touches hand-written sibling files in the same skill
directory.
