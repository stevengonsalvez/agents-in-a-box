---
title: "Quickstart — first session in 60 seconds"
---

From install to your first running agent session.

## 1. Pre-flight

Make sure you have `git`, `tmux`, and a provider CLI (e.g. Claude Code) installed, then:

```bash
ainb init --check    # verify prerequisites without changing anything
ainb auth            # set up authentication if needed
```

## 2. Launch

```bash
ainb
```

First launch runs onboarding. From the home screen, press `s` for Sessions or `n` to start a new session immediately.

## 3. Spawn a session (CLI)

You can also start a session straight from the shell. Isolate it in a worktree and attach in one step:

```bash
ainb run --repo . --worktree --attach
```

Useful variants:

```bash
ainb run --repo . --create-branch feat/new        # isolated worktree on a named branch
ainb run --repo . --worktree -p "fix the tests"   # spawn with an initial prompt
ainb run --repo . --worktree --tool codex         # use Codex instead of Claude
ainb run --remote-repo owner/repo --worktree      # clone a GitHub repo first, then isolate
```

Without `--worktree` (or `--create-branch`) the session runs directly in the
checkout you point at, sharing that branch, index and working tree with your
editor and with every other session started there. Reach for that only when you
deliberately want the shared checkout:

```bash
ainb run --repo .                                 # shared checkout, NO isolation
```

## 4. Attach, detach, reattach

```bash
ainb list                 # see running sessions
ainb attach my-project    # drop into the session's tmux
```

Detach with your tmux detach key (default `Ctrl-b d`) — the agent keeps running. Reattach any time with `ainb attach <name>`.

## 5. Kill a session safely

```bash
ainb kill my-project       # prompts for confirmation
ainb kill my-project -f     # force, no prompt
```

## Where to go next

- [Overview](overview.md) — every screen
- [CLI reference](cli.md) — every subcommand and flag
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Docs hub](../README.md)
