---
title: "ainb TUI — overview"
---

`ainb` is the terminal UI and CLI for Agents-in-a-Box: a Rust + ratatui app that spawns and manages AI coding sessions (Claude Code, Codex, Gemini, Copilot) in isolated git worktrees, each driven inside its own tmux session.

Run `ainb` with no arguments to launch the TUI. Every operation the TUI performs is also exposed as a subcommand, so agents and automation can drive it headlessly. See the [CLI reference](cli.md) for the full subcommand list.

## Launching

```bash
ainb            # launch the TUI (default when no command is given)
ainb init       # first-time setup + prerequisite check
ainb auth       # set up authentication
```

First launch runs an onboarding flow that checks prerequisites (git, tmux, a provider CLI) and helps you authenticate. Use `ainb init --check` to verify prerequisites without changing anything, or `ainb init --status` to see onboarding completion.

## Screen tour

From the home screen, single keys jump to each screen (see [Keyboard shortcuts](keyboard-shortcuts.md)):

| Screen | Key | What it does |
|--------|-----|--------------|
| Home | — | Landing screen with the sidebar + tile navigation |
| Sessions | `s` | List, attach, restart, and delete agent sessions |
| Agents | `a` | Pick a provider/agent before spawning a session |
| Recovery | `R` | Find and resume orphaned sessions / broken worktrees |
| Stats | `i` | Usage analytics: Daily, Weekly, Project, Burndown, Optimize |
| Skills | `k` | Browse available skills |
| Inbox | `I` | ainb-hooks notification inbox |
| Config | `C` | View configuration |

Git operations, log streaming, and a tmux preview are available from within the session views. Press **`g`** on a session to open the Warp-style **[Code Review](code-review.md)** diff — a file sidebar plus per-file collapsible blocks with syntax highlighting and word-level emphasis (also available standalone as `ainb diff-review`). Press **`a`** to [attach](attach.md) full-screen, or **`A`** to turn the preview pane into a live embedded tmux client in-place (`Ctrl+Q` releases it).

## Multi-provider session model

A session pairs a repository checkout with an AI tool. `ainb run` accepts `--tool claude|codex|gemini|copilot` (default `claude`) and `--model` (default `sonnet`). Each session gets its own tmux session you can attach to and detach from without killing the agent.

## Worktree + tmux integration

With `--worktree`, each session is isolated in a fresh git worktree (default branch prefix `agents/`), so parallel sessions never collide on the same working tree. The agent process runs inside a dedicated tmux session; `ainb attach <name>` drops you into it, and detaching leaves it running. Claude Code emits very high-frequency screen updates, so a tuned tmux config is recommended — see [Architecture](architecture.md) and `ainb-tui/config/tmux.conf`.

## See also

- [Install the TUI](install.md)
- [Quickstart](quickstart.md)
- [CLI reference](cli.md)
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Architecture](architecture.md)
- [Docs hub](../README.md)
