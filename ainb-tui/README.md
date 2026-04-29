# AINB TUI

Terminal UI and CLI for Agents-in-a-Box.

## Usage Analytics

Open Stats in the TUI with `i`. The Usage screen includes Daily, Weekly, Project, Burndown, and Optimize tabs. Burndown supports Claude Code and Codex local session histories, period switching, provider filtering, include/exclude project filters, and read-only optimization findings.

Keys: `Tab` changes usage tab, `1`-`5` selects Today/Week/30 days/Month/All, `p` cycles All/Claude/Codex, `/` adds include filter, `x` adds exclude filter, `d` enters custom `YYYY-MM-DD YYYY-MM-DD` range, `c` clears filters, and `r` reloads.

CLI usage:

```bash
ainb usage report --period week
ainb usage report --from 2026-04-01 --to 2026-04-10 --format json
ainb usage export --format csv --output /tmp/ainb-usage.csv
ainb usage optimize --period 30days
ainb usage compare --period all
ainb usage yield --period week
```

AINB reads `~/.claude/projects/**/*.jsonl` and `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` or `$CODEX_HOME/sessions/...`. Parsing is read-only. Cost values are estimates; unknown model prices are omitted while tokens/calls remain visible.
