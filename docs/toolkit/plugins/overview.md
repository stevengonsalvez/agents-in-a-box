---
title: "Claude Code plugins"
description: "The host-agent plugins this repo ships or documents: reflect, ainb-fleet, ainb-hooks, caveman-stats, and illustration."
---

These are **host-agent plugins** — bundles of skills, hooks, and commands that Claude Code, Codex, or Copilot load. They are a different system from [ainb v2 plugins](../../plugins/overview.md), which are subprocess plugins for the **ainb TUI**. If you're unsure which you want, read the [plugins disambiguation](../../plugins/readme.md) first.

The repo publishes the in-tree Claude Code plugins through `.claude-plugin/marketplace.json` at the repo root; the source lives under `plugins/<name>/`. Add the marketplace, then install:

```bash
claude plugin marketplace add https://github.com/stevengonsalvez/agents-in-a-box.git
claude plugin install ainb-fleet@agents-in-a-box

# (or `stevengonsalvez/agents-in-a-box` shorthand if you have GitHub SSH keys set up —
#  the shorthand resolves to SSH and fails with "Host key verification failed" otherwise)
```

> `reflect` is no longer in this marketplace — it was extracted to [stevengonsalvez/ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory) and ships from that repo's `plugin/` dir (or run `ainb reflect bootstrap`).

## The plugins

| Plugin | What it does | Install |
|---|---|---|
| [reflect](./reflect.md) | Agent self-improvement + retrieval — captures learnings and auto-injects relevant prior ones at session start. **(extracted → [ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory))** | `ainb reflect bootstrap` |
| [ainb-fleet](./ainb-fleet.md) | LLM-facing skill bundle teaching agents to drive `ainb fleet …` multi-session orchestration (broadcast, sequence, needs, daemon). | `claude plugin install ainb-fleet@agents-in-a-box` |
| [ainb-hooks](./ainb-hooks.md) | Emits Claude Code / Codex / Copilot lifecycle events to the ainb notification inbox (consumed by the [Inbox & notifications](../../tui/inbox-notifications.md) daemon — host code, not a plugin). | `ainb-notifyd install --claude --codex --copilot` |
| `caveman-stats` | Lifecycle hooks for caveman mode: statusline token-savings suffix and compaction survival. **Claude-only** — the hook *events* exist in all three harnesses (see matrix), but the code reads Claude session JSONL (`CLAUDE_CONFIG_DIR`) and writes the Claude statusline suffix, and no Codex/Copilot statusline consumes it. | `claude plugin install caveman-stats@agents-in-a-box` |
| `illustration` | Mascot-driven illustration skill bundle; no lifecycle hooks. | `claude plugin install illustration@agents-in-a-box` |

## Hook parity target

Hook names are shown as `PascalCase / camelCase` where Copilot uses a different spelling. `YES` means the harness has a native event shape for the target behavior; `N/A` means no matching hook exists in that harness. Codex column reflects the current Codex CLI hook enum, including `PermissionRequest`, `PreToolUse`, `PostCompact`, and `Subagent*`.

| Plugin / owner | Hook | Claude | Codex | Copilot | Target behavior |
|---|---|---:|---:|---:|---|
| `reflect` | `SessionStart / sessionStart` | YES | YES | YES | Recall inject + detached drain |
| `reflect` | `UserPromptSubmit / userPromptSubmitted` | YES | YES | YES | Prompt recall + mini-learning completion |
| `ainb-hooks` | `Notification / notification` | YES | N/A | YES | Awaiting-input watcher |
| `reflect` / policy | `PreToolUse / preToolUse` | YES | YES | YES | Tool policy/context before risky calls |
| `ainb-hooks` | `PermissionRequest / permissionRequest` | YES | YES | YES | Permission-pattern lookup + watcher |
| `reflect` / `caveman-stats` | `PostToolUse / postToolUse` | YES | YES | YES | Tool-result signal watcher |
| `reflect` | `PostToolUseFailure / postToolUseFailure` | YES | N/A | YES | Explicit failure mini-learning watcher |
| `reflect` / `caveman-stats` | `PreCompact / preCompact` | YES | YES | YES | Silent queue producer |
| `reflect` | `PostCompact / postCompact` | YES | YES | N/A | Breadcrumb/cleanup only, no drain |
| `reflect` | `SubagentStart / subagentStart` | YES | YES | YES | Subagent-scoped recall inject |
| `reflect` | `SubagentStop / subagentStop` | YES | YES | YES | Queue subagent transcript |
| `ainb-hooks` / `reflect` | `Stop / agentStop` | YES | YES | YES | Slot update + queue fallback |
| `reflect` | `SessionEnd / sessionEnd` | YES | N/A | YES | Final cleanup + final queue producer |
| `reflect` | `errorOccurred` | N/A | N/A | YES | Error breadcrumb + optional watcher |
| `ainb-fleet` | *(none)* | N/A | N/A | N/A | Pure skill/workflow bundle; no lifecycle hooks |
| `illustration` | *(none)* | N/A | N/A | N/A | Pure illustration skill bundle; no lifecycle hooks |

Key: only the `SessionStart` drain runs `/reflect`. `PreCompact`, `Stop`, `SubagentStop`, and `SessionEnd` only queue. `PostCompact` stays bookkeeping only.

Each page below has a `/fireworks-tech-graph` diagram of how the plugin works, the exact hooks/skills it registers, and how to use it.
