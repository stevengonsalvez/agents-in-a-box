# caveman-stats

> [!NOTE]
> **Canonical location moved:** This plugin has moved to [`ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit/tree/main/plugins/caveman-stats).
> Preferred install: `claude plugin install caveman-stats@ainb-toolkit`
> Kept here for backward compatibility with `caveman-stats@agents-in-a-box`.

Caveman mode lifecycle hooks for Claude Code.

## Overview

`caveman-stats` tracks estimated token savings in caveman mode and ensures caveman mode instructions survive context compaction.

- **`PostToolUse`**: Silently updates `.caveman-statusline-suffix` on each tool use.
- **`PreCompact`**: Synchronously re-injects the caveman ruleset into the compaction summary so caveman mode survives context compaction.

## Installation

```bash
claude plugin marketplace add stevengonsalvez/ainb-toolkit
claude plugin install caveman-stats@ainb-toolkit
```
