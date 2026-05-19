---
title: "Plugins — disambiguation"
---

# Plugins — disambiguation

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
- **Reference plugins:** `burndown` (analytics) + `session-reader` (data backend) — both ship in-tree

If you want to **add a screen / CLI / dashboard to the TUI**, you want this kind of plugin. Continue to:
- [overview.md](overview.md) — what a v2 plugin is, conceptually
- [user-guide.md](user-guide.md) — install, configure, troubleshoot
- [authoring.md](authoring.md) — write one
- [spec-v2.md](spec-v2.md) — the wire contract

### 2. **Claude Code plugins** (Anthropic's plugin system)

A separate system owned by Claude Code itself. The monorepo ships **one** Claude Code plugin: `reflect`.

- **Where it lives:** `plugins/reflect/` at the repo root (not inside `ainb-tui/`)
- **Distribution:** through `.claude-plugin/marketplace.json` at the repo root, which the Claude Code CLI reads
- **Runtime:** Claude Code itself loads it as a skill/hook bundle; ainb is not involved
- **What it does:** captures and retrieves learnings inside Claude Code sessions via the `reflect-kb` library
- **Install:** `claude plugin install reflect@agents-in-a-box`

If you want to **add behaviour to Claude Code itself**, you want a Claude Code plugin. Documentation for that system is upstream:
- [Claude Code plugin docs](https://docs.anthropic.com/en/docs/claude-code) — Anthropic's authoritative reference
- The `plugins/reflect/` directory in this repo is a working example you can read

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
