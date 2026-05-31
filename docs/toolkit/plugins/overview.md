---
title: "Claude Code plugins"
description: "The Claude Code plugins this repo ships — reflect, ainb-fleet, ainb-hooks — installable from the agents-in-a-box marketplace."
---

These are **Claude Code plugins** — bundles of skills, hooks, and commands that **Claude Code** (Anthropic's CLI) loads. They are a different system from [ainb v2 plugins](../../plugins/overview.md), which are subprocess plugins for the **ainb TUI**. If you're unsure which you want, read the [plugins disambiguation](../../plugins/readme.md) first.

The repo publishes them through `.claude-plugin/marketplace.json` at the repo root; the source lives under `plugins/<name>/`. Add the marketplace, then install:

```bash
claude plugin marketplace add stevengonsalvez/agents-in-a-box
claude plugin install reflect@agents-in-a-box
claude plugin install ainb-fleet@agents-in-a-box
```

## The plugins

| Plugin | What it does | Install |
|---|---|---|
| [reflect](./reflect.md) | Agent self-improvement + retrieval — captures learnings and auto-injects relevant prior ones at session start. | `claude plugin install reflect@agents-in-a-box` |
| [ainb-fleet](./ainb-fleet.md) | LLM-facing skill bundle teaching agents to drive `ainb fleet …` multi-session orchestration (broadcast, sequence, needs, daemon). | `claude plugin install ainb-fleet@agents-in-a-box` |
| [ainb-hooks](./ainb-hooks.md) | Emits Claude Code / Codex lifecycle events to the ainb notification inbox (pairs with the [`notifyd`](../../plugins/notifyd.md) in-tree plugin). | `ainb-notifyd install --claude --codex` |

Each page below has a `/fireworks-tech-graph` diagram of how the plugin works, the exact hooks/skills it registers, and how to use it.
