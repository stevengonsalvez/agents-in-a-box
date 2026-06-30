# Harness plugins

This folder holds **harness plugins** — extensions that plug into the *coding
agent itself* (Claude Code, Codex CLI, GitHub Copilot CLI). They add skills,
lifecycle hooks, and notification wiring to whichever agent harness you run.

They are installable through the `agents-in-a-box` plugin marketplace
(`.claude-plugin/marketplace.json` at the repo root) or provisioned by the
`ainb` setup wizard.

> Think of the broader ecosystem's harness plugins — Codexplored, Hermes, and
> friends. Same genre: they ride *inside* the agent. The ones here are ours.

## What these are NOT

These are **not** ainb-tui plugins.

```
┌──────────────────────────────┐        ┌──────────────────────────────┐
│  HARNESS plugins (this dir)   │        │  TUI plugins (ainb-tui/)      │
│  extend the coding AGENT      │   vs   │  extend the ainb TUI itself   │
│  Claude / Codex / Copilot     │        │  ratatui screens + commands   │
│  skills · hooks · notifs      │        │  JSON-RPC subprocess, ABI v2  │
│  installed via marketplace    │        │  compiled / bundled in `ainb` │
└──────────────────────────────┘        └──────────────────────────────┘
```

TUI plugins live in `ainb-tui/crates/ainb-plugin-*` (burndown, notifyd,
session-reader, …) and are loaded by the TUI's plugin runtime, not by any
agent harness. See `ainb-tui/CLAUDE.md` for that system.

### ⚠️ `hangar-tui/` is the exception in this folder

`plugins/hangar-tui/` is a **TUI plugin**, not a harness plugin. It is a Rust
crate (`Cargo.toml` + `manifest.toml`, ABI v2, JSON-RPC over a Unix socket)
and a member of the `ainb-tui` Cargo workspace
(`ainb-tui/Cargo.toml` → `"../plugins/hangar-tui"`). It is **compiled into the
`ainb` binary** — there is nothing to "install" from the marketplace. It sits
here only because that's where its source path was placed; ignore it when
reasoning about harness plugins.

## The harness plugins

| Plugin           | Kind   | What it does                                                          | Marketplace install                          |
|------------------|--------|----------------------------------------------------------------------|----------------------------------------------|
| `ainb-fleet`     | skills | Teaches agents the `ainb fleet …` multi-session orchestration verbs   | `ainb-fleet@agents-in-a-box`                 |
| `ainb-hooks`     | hooks  | Emits Stop / Notification / PermissionRequest events to the ainb inbox| `ainb-hooks@agents-in-a-box`                 |
| `caveman-stats`  | hooks  | Caveman token-savings statusline + compaction-survival re-inject      | `caveman-stats@agents-in-a-box`              |
| `illustration`   | skills | Popa-mascot brand-illustration workflow (sketchnote generation)       | `illustration@agents-in-a-box`               |

Two more harness plugins ship in this marketplace but are **sourced
externally** (not files in this folder):

| Plugin     | Source                                              | What it does                                          |
|------------|-----------------------------------------------------|-------------------------------------------------------|
| `reflect`  | `github:stevengonsalvez/ainb-reflect-memory` (pinned) | Long-term memory: SessionStart recall + capture hooks |
| `caveman`  | external `caveman` marketplace                      | Ultra-compressed token-saving response mode           |

## Install

### 1. Add the marketplace (once)

```bash
claude plugin marketplace add stevengonsalvez/agents-in-a-box
```

### 2. Install a plugin (Claude Code)

```bash
claude plugin install ainb-fleet@agents-in-a-box
claude plugin install ainb-hooks@agents-in-a-box
claude plugin install caveman-stats@agents-in-a-box
claude plugin install illustration@agents-in-a-box
```

`reflect` carries its own marketplace, so add that source first:

```bash
claude plugin marketplace add stevengonsalvez/ainb-reflect-memory
claude plugin install reflect@ainb-reflect-memory
```

### 3. Or let the wizard do it

```bash
ainb init          # interactive setup, offers every plugin above
ainb init --script # print the install script instead of running it
```

The catalog the wizard drives lives in
`ainb-tui/crates/ainb-core/src/setup/catalog.rs` (single source of truth for
TUI onboarding **and** the `ainb init` CLI).

## Cross-harness support

Codex CLI and Copilot CLI now expose the same lifecycle hook events as Claude
Code (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`,
and — on recent builds — compaction). So cross-harness support is no longer an
event problem; it's a wiring problem, and the wiring exists. The marketplace
(`claude plugin install`) is still Claude-only, but Codex and Copilot reach the
same functionality through these mechanisms:

- **Skill plugins** (`ainb-fleet`, `illustration`) are portable Markdown.
  Install each skill unit into the Codex/Copilot skill homes:

  ```bash
  ainb skill install gh:stevengonsalvez/agents-in-a-box@main/plugins/ainb-fleet/skills/<unit> \
    --targets codex,copilot
  ```

- **Hook plugins** (`ainb-hooks`) need per-harness hook wiring. One command
  writes all three formats (and `ainb init` now calls it with `--all`):

  ```bash
  ainb notifyd install --all          # = --claude --codex --copilot
  ```

  (`ainb-hooks` ships `codex/hooks.json` and `copilot/hooks.json` next to the
  Claude manifest, plus a universal `notify.sh`.)

### Support matrix

| Plugin          | Claude Code | Codex CLI            | Copilot CLI          | Notes                                                                 |
|-----------------|-------------|----------------------|----------------------|----------------------------------------------------------------------|
| `ainb-fleet`    | ✅          | ➜ via `skill install`| ➜ via `skill install`| skills are harness-portable Markdown                                 |
| `ainb-hooks`    | ✅          | ✅                   | ✅                   | ships all three hook formats; `ainb notifyd install --all`           |
| `caveman-stats` | ✅          | ❌                   | ❌                   | **Claude-first.** The hook *events* exist everywhere, but the code is Claude-coupled (reads Claude JSONL via `CLAUDE_CONFIG_DIR`, writes the Claude statusline file) and no Codex/Copilot statusline consumes the token-savings suffix — porting would be dead code |
| `illustration`  | ✅          | ➜ via `skill install`| ➜ via `skill install`| skills are harness-portable Markdown                                 |
| `reflect`       | ✅          | ✅ Codex adapter     | ✅ Copilot adapter   | full parity; `plugin/adapters/{codex,copilot}` write native hook configs. Copilot ignores `userPromptSubmitted` output → per-prompt recall is manual `/recall` (SessionStart auto-recall works) |
| `caveman`       | ✅          | ➜ via `skill install`| ➜ via `skill install`| external marketplace                                                 |

Legend: ✅ first-class · ➜ supported via a sync/adapter step · ❌ not available / not worthwhile.

## Authoring a new harness plugin

1. Create `plugins/<name>/.claude-plugin/plugin.json` (name, version, hooks
   and/or a `skills/` dir).
2. Register it in the root `.claude-plugin/marketplace.json`.
3. If it's a **hook** plugin and should reach Codex/Copilot, add
   `codex/hooks.json` and `copilot/hooks.json` (see `ainb-hooks/` for the
   reference layout).
4. Add it to the setup catalog
   (`ainb-tui/crates/ainb-core/src/setup/catalog.rs`) so `ainb init` offers it.
