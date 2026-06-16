---
title: "Starting a new session"
description: "Walk through the new-session wizard in ainb-tui — pick a repo, choose a preset, and tune the Agent, Model, Mode, Yolo, and Branch options. What each option means and when to change it."
---

Every session in ainb runs in its own **git worktree + tmux + agent**, fully isolated. The new-session wizard is where you choose *what* runs and *where*. This page walks through opening it and explains every option.

## Open the wizard

From the home or Sessions screen, press **`n`**. ainb opens the **repo picker** — type to filter, or paste an `owner/repo`, an `https://` / `ssh://` URL, or a local path — then press `Enter` to pick one.

You land on the **Configure** screen. Move between rows with **`Tab`** (or `↑` / `↓`), change the focused row's value with **`←` / `→`**, and launch from the **`[ Launch ]`** row:

![The new-session Configure wizard cycling through every option — Preset, Agent (Claude · Codex · Gemini [soon] · Copilot · Shell · SSH), Model, Mode, Yolo, Branch, and Launch](../assets/screenshots/new-session-options.gif)

## The options

### Preset

A named bundle of all the settings below — the fast path. ainb ships a few (e.g. `claude-interactive-yolo`, `codex-interactive-yolo`, `opusplan`, `shell`), with **`Custom`** at the end of the ring.

- Pick a **named preset** and the rows below are *locked* to its values.
- Pick **`Custom`** to *unlock* every row and tune it by hand.
- After tweaking Custom, press **`Ctrl+S`** to save it as a new named preset.

### Agent

Which tool drives the session:

| Agent | What it is |
|-------|-----------|
| **Claude** | Anthropic's Claude Code |
| **Codex** | OpenAI's Codex CLI |
| **Copilot** | GitHub Copilot CLI |
| **Gemini** `[soon]` | greyed out — shown for visibility, not yet selectable |
| **Shell** | a plain terminal, no agent |
| **SSH** | connect to a remote host |

Cycle with `←` / `→`. The greyed **`Gemini [soon]`** pill is skipped by the cursor — it can't be selected yet.

### Model

The model the agent uses. Only **Claude** and **Codex** expose a model ring (Claude Opus / Sonnet / Haiku; Codex GPT variants) — other agents use their own default, so the row reads `system default` or is hidden. Leave it on the default to let the agent's CLI decide.

### Mode

- **Interactive** — you drive; the agent waits for your input. The normal mode.
- **Boss** `[alpha]` — autonomous; the agent works on its own. Marked `[alpha]` because it isn't production-ready yet — don't rely on it.

### Yolo

How the agent handles permissions:

- **ON** — auto-approve everything (skip permission prompts). Fast, but the agent can run commands without asking.
- **OFF** — prompt before sensitive actions.

### Branch

Each session gets its own git worktree. This row shows **`<source> → <worktree>`** — the branch you base off and the new worktree branch ainb creates (default `agents/<hash>`). Press `Enter` to rename it or pick a different base.

### Launch

`Tab` down to **`[ Launch ]`** and press `Enter` to start the session — or press **`Ctrl+Enter`** from any row to quick-launch with the current settings.

## Keys

| Key | Action |
|-----|--------|
| `Tab` / `↑` `↓` | move between rows |
| `←` / `→` | change the focused value |
| `Enter` | launch (on the Launch row); on the Branch row, pick a base (source segment) or edit the name (worktree segment) |
| `Ctrl+Enter` | quick-launch from any row |
| `Ctrl+S` | save Custom as a named preset |
| `Esc` | back to the repo picker |

## Where to go next

- [Quickstart](quickstart.md) — install to first session, plus the `ainb run` CLI equivalent
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [CLI reference](cli.md)
- [Overview](overview.md) — every screen
