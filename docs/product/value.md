# Value proposition

What you get when you adopt agents-in-a-box.

---

## The promise

**Run agents. Worktree per session. No cross-contamination.**

That's the whole pitch. The rest is what makes it real:

---

## What you get

### Multi-provider session management in one tool

Spawn Claude Code, Codex, Gemini, Copilot, Kiro, or a raw shell session from the same interface. Model picker per session (Sonnet · Opus · Haiku). Keyboard-driven. No web dashboard.

### Git worktree isolation by default

Every session gets its own branch and worktree directory. No stash dance. No cross-contamination between parallel sessions. Auto-cleanup when sessions are killed cleanly.

### tmux-backed persistence

Sessions survive terminal disconnects, SSH drops, laptop sleep. Reattach any time with `ainb attach <name>` or via the TUI.

### Built-in usage analytics

Token and session attribution per day, week, project, model, and provider. Burndown dashboard with optimisation hints. No external service needed — reads local JSONL logs.

### Scriptable end-to-end

Every TUI operation is also a CLI subcommand. `--format json` on every command. Pipe to `jq`, drive from CI, automate workflows.

### A portable toolkit that follows you across tools

86 skills (plan, implement, validate, reflect, swarm-create, …) and 37 specialised agents (backend-developer, code-reviewer, security-agent, …). Write them once; deploy to Claude Code, Codex, Copilot, Gemini, Amazon Q, Cursor, Cline, Roo, Hermes, nanoclaw, Clawdhub. One source, nine targets.

### A plugin system you can actually use

Add a TUI screen, a CLI subcommand, a statusline segment without forking the host. Native binaries over JSON-RPC. Capability-gated — your plugin can't reach the network unless you say it can. Two reference implementations ship in-tree; copy-paste a starting point.

### A knowledge system that learns across sessions

`/reflect` captures non-obvious learnings as you work. `/recall` retrieves them in future sessions. GraphRAG underneath for cross-project queries; QMD vector search for fast hits. 170+ learnings indexed across the maintainer's own dev work, growing.

---

## What it costs you

- **One install command.** `brew install ainb` (macOS, Linux). One-liner `install.sh` for everything else.
- **No subscription.** MIT-licensed. No accounts, no metering.
- **No vendor lock-in.** Toolkit deploys to nine AI tools; the TUI orchestrates four providers. You can swap out any layer.
- **No cloud dependency.** Everything runs locally. The only network traffic is what your AI provider (Claude, Codex, etc.) makes on your behalf.

---

## What it's worth

If you're running ≥2 parallel AI coding sessions a week and tracking your own token spend, agents-in-a-box pays for itself in the first week — purely on saved worktree-management toil. The burndown dashboard alone replaces a dashboard you'd otherwise build yourself (or live without).

If you're authoring AI skills or agents and supporting them across multiple tools (Claude Code + Codex + Copilot), the toolkit's deploy-everywhere bootstrap replaces nine custom packaging scripts.

If you're building a TUI extension or a custom analytics dashboard for your team's AI usage, the v2 plugin system lets you ship that as a 500-line binary instead of a fork.

---

## See also

- [What is agents-in-a-box?](what-is-ainb.md) — start-here
- [Whole-system architecture](architecture.md) — diagram + components
- [Install](../tui/install.md)
