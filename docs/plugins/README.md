---
title: "Plugins — disambiguation"
---

> **Read this first.** The word "plugin" means two different things in this monorepo. Picking the wrong path costs you hours.

---

## Two systems, same word

### 1. **ainb v2 plugins** (subprocess plugins for the TUI host)

What the rest of this directory documents.

- **Where they live:** `ainb-tui/crates/ainb-plugin-*/`
- **Distribution:** staged into `dist/plugins/<name>/` by `just stage-plugins`
- **Runtime:** the `ainb` TUI spawns each plugin as a native child process and talks JSON-RPC 2.0 over framed stdio
- **What they can do:** own a TUI screen, claim a CLI subcommand tree, publish/subscribe to snapshot topics, paint statusline segments
- **Capability model:** deny-by-default; manifest declares grants for filesystem, network, subprocess, event bus
- **Reference plugins:** `burndown` (analytics), `notifyd` (notifications), `session-reader` (data backend), and `witr` (process causality, wraps an external binary) — all ship in-tree

If you want to **add a screen / CLI / dashboard to the TUI**, you want this kind of plugin. Continue to:
- [overview.md](overview.md) — what a v2 plugin is, conceptually
- [user-guide.md](user-guide.md) — install, configure, troubleshoot
- [authoring.md](authoring.md) — write one
- [spec-v2.md](spec-v2.md) — the wire contract

### 2. **Claude Code plugins** (Anthropic's plugin system)

A separate system owned by Claude Code itself. The monorepo ships **three** Claude Code plugins, all under `plugins/<name>/` at the repo root (not inside `ainb-tui/`):

- **[`reflect`](../toolkit/plugins/reflect.md)** — agent self-improvement + retrieval (skills + SessionStart/PostToolUse/Stop hooks). `claude plugin install reflect@agents-in-a-box`
- **[`ainb-fleet`](../toolkit/plugins/ainb-fleet.md)** — LLM-facing skill bundle teaching agents to drive `ainb fleet …` multi-session orchestration. `claude plugin install ainb-fleet@agents-in-a-box`
- **[`ainb-hooks`](../toolkit/plugins/ainb-hooks.md)** — emits Claude Code / Codex lifecycle events to the ainb notification inbox (pairs with the `notifyd` in-tree plugin); wired by `ainb-notifyd install`.

- **Distribution:** through `.claude-plugin/marketplace.json` at the repo root, which the Claude Code CLI reads
- **Runtime:** Claude Code itself loads them as skill/hook bundles; the ainb TUI is not involved
- **Full docs:** [Toolkit → Claude Code plugins](../toolkit/plugins/overview.md) — a page per plugin with how-it-works diagrams

If you want to **add behaviour to Claude Code itself**, you want a Claude Code plugin. Anthropic's authoritative reference is the [Claude Code plugin docs](https://docs.anthropic.com/en/docs/claude-code); the `plugins/*/` directories here are working examples.

---

## Quick decision tree

```
                  Are you trying to extend...
                              │
              ┌───────────────┴───────────────┐
              │                               │
        the ainb TUI?                  Claude Code itself?
              │                               │
              ▼                               ▼
        ainb v2 plugin                  Claude Code plugin
              │                               │
              ▼                               ▼
   /docs/plugins/authoring.md      /plugins/reflect/ (example)
                                   + upstream Anthropic docs
```

---

## Why the name collision?

Historical accident. ainb's plugin system pre-dated Claude Code shipping its own. Both names are entrenched in their respective communities, so the monorepo carries the ambiguity and pays for it with this disambiguation note.

If you ever see a plain reference to "the plugin" in this repo without context, look at where it lives:
- `ainb-tui/crates/ainb-plugin-*` → ainb v2 plugin
- `plugins/<name>/` at root → Claude Code plugin
- `toolkit/packages/plugins/` does **not** exist; was deprecated.
