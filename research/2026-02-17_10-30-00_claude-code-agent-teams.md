# Research: Claude Code Agent Teams - Setup in Our Workspace

**Date**: 2026-02-17
**Repository**: ai-coder-rules
**Branch**: main
**Research Type**: Comprehensive (Web + Codebase)

## Research Question
How do Claude Code Agent Teams work, and how can we set them up in our workspace?

## Executive Summary

Claude Code Agent Teams is an **experimental built-in feature** (since v2.1.32, Feb 5 2026) that lets multiple Claude Code instances coordinate as a team via shared task lists and direct peer messaging. Our workspace already has a custom swarm system (`/swarm-create`, `/swarm-status`, etc.) built on tmux + JSONL messaging + Beads. The official Agent Teams feature is simpler to enable (one setting) and handles orchestration natively, but our custom system offers tighter Beads integration and worktree isolation.

## Key Findings

- Enable with one setting: `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
- Architecture: team lead + teammates with shared task list + mailbox messaging
- Two display modes: in-process (default, any terminal) or split-pane (tmux/iTerm2)
- Known bug: delegate mode breaks teammate permissions (Issue #25037)
- Our existing swarm system is complementary - use Agent Teams for ad-hoc parallel work, swarm for Beads-driven epic execution

---

## How Agent Teams Work

### Architecture

| Component | Role |
|-----------|------|
| **Team Lead** | Main Claude Code session - creates team, spawns teammates, coordinates |
| **Teammates** | Independent Claude Code instances with own context windows |
| **Shared Task List** | Work items with states (pending/in_progress/completed) + dependency support |
| **Mailbox** | Direct agent-to-agent messaging system |

### vs Subagents (Task tool)

| Aspect | Subagents (Task) | Agent Teams |
|--------|-------------------|-------------|
| Context | Own window; results return to caller | Own window; fully independent |
| Communication | Report back to main only | Teammates message each other directly |
| Coordination | Main agent manages all work | Shared task list with self-coordination |
| Best for | Focused tasks where only result matters | Complex work requiring discussion |
| Token cost | Lower | Higher (each teammate = separate instance) |

### vs Our Custom Swarm System

| Aspect | Agent Teams (built-in) | Our Swarm (`/swarm-create`) |
|--------|------------------------|---------------------------|
| Setup | One env var | `/swarm-create` with epic ID |
| Task source | Natural language prompts | Beads epic DAG |
| Messaging | Built-in mailbox at `~/.claude/teams/` | JSONL inbox at `.claude/swarm/{team}/inbox/` |
| Isolation | Shared directory only | Shared OR worktree mode |
| Task tracking | Built-in task list | Beads (`bd` CLI) |
| Progress | In-process UI or tmux split | `bd swarm status` |
| Spawning | Natural language ("create 3 teammates") | `swarm_spawn_agent()` |

---

## Setup Instructions

### Step 1: Enable the Feature

Add to `~/.claude/settings.json`:

```json
{
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  }
}
```

Or per-session:
```bash
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude
```

### Step 2: Configure Display Mode

Add to `~/.claude/settings.json`:

```json
{
  "teammateMode": "auto"
}
```

Options:
- `"auto"` (default) - split-pane if in tmux, otherwise in-process
- `"in-process"` - all teammates in main terminal (Shift+Up/Down to navigate)
- `"tmux"` - each teammate gets own tmux pane
- `"iterm2"` - iTerm2-native splits (macOS only)

Per-session override:
```bash
claude --teammate-mode in-process
```

### Step 3: Use It

Just describe the team you want in natural language:

```
Create an agent team to refactor the authentication module:
- One teammate on backend API changes
- One teammate on frontend integration
- One teammate writing tests
Use Sonnet for each teammate.
```

### Key Controls

| Action | Shortcut |
|--------|----------|
| Navigate teammates | `Shift+Up/Down` |
| View teammate session | `Enter` |
| Toggle delegate mode | `Shift+Tab` |
| Toggle task list | `Ctrl+T` |
| Interrupt teammate | `Escape` (while viewing) |

### Hooks for Quality Gates

In `.claude/settings.json`:

```json
{
  "hooks": {
    "TeammateIdle": [
      {
        "matcher": "",
        "hooks": [{
          "type": "command",
          "command": "echo 'Check if there are remaining tasks to pick up'"
        }]
      }
    ],
    "TaskCompleted": [
      {
        "matcher": "",
        "hooks": [{
          "type": "command",
          "command": "echo 'Verify tests pass before marking complete'"
        }]
      }
    ]
  }
}
```

Exit with code 2 from hooks to send feedback and prevent completion/idle.

---

## Best Use Cases

1. **Research & review** - multiple angles simultaneously
2. **New modules/features** - teammates own separate pieces
3. **Debugging with competing hypotheses** - parallel theory testing
4. **Cross-layer coordination** - frontend/backend/tests each owned by different teammate

## When NOT to Use

- Sequential tasks with heavy dependencies
- Multiple agents editing the same files
- Simple, routine single-session fixes
- When token cost matters (3-4x a single session)

## Known Limitations

- **No session resumption** - `/resume` and `/rewind` don't restore teammates
- **Delegate mode bug** - teammates inherit restricted permissions (Issue #25037), use Default mode instead
- **One team per session** - clean up before starting a new one
- **No nested teams** - teammates can't spawn their own teams
- **Split-pane requires tmux/iTerm2** - not VS Code terminal or Windows Terminal
- **Task status can lag** - teammates may not mark tasks as completed
- **Slow shutdown** - teammates finish current request before stopping

## Storage Locations

```
~/.claude/teams/{team-name}/config.json    # Team config + member registry
~/.claude/teams/{team-name}/messages/      # Inter-agent mailbox
~/.claude/tasks/{team-name}/               # Shared task list files
```

## Recommended Workspace Integration

### Option A: Use Agent Teams Alongside Custom Swarm (Recommended)

Keep both systems - use each for what it's best at:

- **Agent Teams** for ad-hoc parallel work (code review, research, debugging)
- **Custom Swarm** (`/swarm-create`) for structured Beads-driven epic execution

### Option B: Migrate to Agent Teams

Replace custom swarm with native Agent Teams:
- Simpler setup (one env var vs. custom scripts)
- Built-in UI and messaging
- But loses Beads integration and worktree isolation

### Suggested settings.json additions

```json
{
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  },
  "teammateMode": "auto"
}
```

## References

- [Official docs: Agent Teams](https://code.claude.com/docs/en/agent-teams)
- [Anthropic releases Opus 4.6 with Agent Teams - TechCrunch](https://techcrunch.com/2026/02/05/anthropic-releases-opus-4-6-with-new-agent-teams/)
- [From Tasks to Swarms - alexop.dev](https://alexop.dev/posts/from-tasks-to-swarms-agent-teams-in-claude-code/)
- [Claude Code's Hidden Multi-Agent System - paddo.dev](https://paddo.dev/blog/claude-code-hidden-swarm/)
- [Enable Team Mode - Scott Spence](https://scottspence.com/posts/enable-team-mode-in-claude-code)
- [Claude Code Swarms - Addy Osmani](https://addyosmani.com/blog/claude-code-agent-teams/)
- [Delegate mode bug - Issue #25037](https://github.com/anthropics/claude-code/issues/25037)
- [claude-code-hooks-multi-agent-observability](https://github.com/disler/claude-code-hooks-multi-agent-observability)
