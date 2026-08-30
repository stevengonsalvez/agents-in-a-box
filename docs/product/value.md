---
title: "Value proposition"
---

What changes about your day when you adopt agents-in-a-box.

---

## The promise

**Run a fleet. Lose nothing.**

Every agent gets its own worktree, its own tmux session, and a line item on your
bill. One agent is a chat window. Five agents is an operations problem, and that
is the part nothing else solves.

---

## What changes

### Your agents stop destroying each other's work

Every session gets its own branch and its own worktree directory. No stash
dance, no index lock races, no half-committed file from one agent landing in
another's diff. Worktrees are cleaned up when a session is killed cleanly.

### Closing your laptop stops costing you a session

Sessions are tmux-backed, so they survive terminal disconnects, SSH drops and
sleep. Reattach with `ainb attach <name>` or from the TUI and keep typing. When
something does die badly, `ainb recover` finds the orphans and resumes them
rather than leaving you to read `tmux ls` and guess.

### You stop polling your own agents

Press `f` and the Fleet panel tells you which agents are blocked on you right
now, and what they asked. Answer from that one screen without attaching to
anything. Every approval, completion and error lands in a single Inbox instead
of scattering across panes you have to remember to check.

### Work happens while you are not watching

Hangar holds a board of tasks that agents pull from, so you stop being the
scheduler. Squads fan one job across several agents instead of running them one
at a time. Autopilots fire on a schedule and report back. ATC is an always-on
watcher that works the queue while you are away.

### You see what an agent changed before you trust it

Hunk-by-hunk review with syntax highlighting and word-level emphasis, on the
session's real diff. `ainb diff-review` does the same thing headless, so a
review can gate a merge without opening the TUI at all.

### The bill stops surprising you

Token and cost attribution per day, week, project, model and provider, read from
local JSONL logs with no external service involved. Per-project attribution
tells you which repo actually burned the budget, which no provider console will.
Headroom compresses context on opted-in sessions so a long run stops walking
into the context wall.

### You stop re-explaining your codebase

`/reflect` captures the non-obvious things you work out during a session.
`/recall` brings them back in the next one. GraphRAG underneath for cross-project
questions, vector search for fast hits.

### Your rules follow you between tools

94 skills and 16 specialised agents, written once and deployed to Claude Code,
Codex, Copilot, Gemini, Amazon Q, Cursor, Cline, Roo and Claude Desktop. One
source, nine targets, so a rule you fix stays fixed everywhere instead of drifting
between nine copies.

### It reaches past your terminal

Bridge the fleet to Telegram, Slack or Discord and answer an agent from your
phone. `ainb web` serves a read-only dashboard for a glance from a browser. The
Claude Code statusline can carry your live rate-limit and spend without you
opening anything.

### It scripts

Every TUI action has a CLI equivalent across 40 commands, and session state,
config, git, usage, fleet and most daemons speak `--format json`. Pipe it to
`jq`, drive it from CI.

### You can extend it without forking

Add a TUI screen, a CLI subcommand or a statusline segment as a native binary
speaking JSON-RPC. Capabilities are default-deny, so a plugin cannot reach the
network or your secrets unless its manifest says so and you agree. Six plugins
ship in-tree as worked examples.

---

## What it costs you

- **One install command.** `brew install ainb` on macOS and Linux, an
  `install.sh` one-liner elsewhere.
- **No subscription.** MIT-licensed, no accounts, no metering.
- **No cloud dependency.** Everything runs locally. The only network traffic is
  what your AI provider makes on your behalf.
- **No vendor lock-in.** The TUI talks to providers through a PTY, not an SDK,
  so swapping a layer is a config change rather than a migration.

---

## What it will not do

Worth knowing before you install rather than twenty minutes in:

- **Worktrees isolate git state, not your environment.** Two agents still share
  port 3000, your database and `node_modules`. If they need different services
  running, that is still your problem to arrange.
- **tmux and git are required**, not optional.
- **No native Windows.** It uses PTY and POSIX file modes. WSL2 works.
- **`ainb otel` sets up Grafana Alloy** to carry Claude Code's telemetry. ainb
  does not emit telemetry of its own, and nothing here phones home.
- **Memory starts empty.** The learnings browser is worth exactly what `reflect`
  has written into it so far.
- **ATC and headroom are opt-in**, needing a hook plugin and an external binary
  respectively.

---

## What it is worth

Running two or more parallel sessions a week, the worktree and tmux handling
alone pays for the install in the first week, and the burndown dashboard
replaces one you would otherwise build or live without.

Authoring skills or agents across several tools, the toolkit's deploy-everywhere
bootstrap replaces the per-tool packaging scripts you would otherwise maintain
by hand.

Building a TUI extension or a team usage dashboard, the v2 plugin system lets
you ship it as a small binary against a versioned contract instead of a fork you
have to keep rebasing.

---

## See also

- [What is agents-in-a-box?](what-is-ainb.md) · start here
- [Whole-system architecture](architecture.md) · diagram + components
- [Install](../tui/install.md)
