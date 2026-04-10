# Specification: coding-agent Skill Optimization

**Generated from:** User interview (optimization request)
**Interview date:** 2026-04-10
**Version:** 1.0

## Executive Summary

Refactor the existing `coding-agent` skill from its clawdbot/OpenClaw-specific origin into a generic, portable skill for delegating coding tasks to local agents (Codex, Claude Code, OpenCode, Pi, GitHub Copilot) via tmux sessions. Establishes a clean boundary with `/cloud-coding-agent` for remote execution and removes all host-specific runtime assumptions.

## Objectives

### Primary Goals
- Remove all OpenClaw/clawdbot-specific references (custom tools, paths, scripts, personas)
- Standardize on **pure tmux** as the single execution model for spawning, monitoring, and controlling coding agents
- Add **GitHub Copilot** as a first-class supported agent (parity with `ainb-tui`'s agent list)
- Establish a clear boundary with `/cloud-coding-agent` (local vs cloud)
- Preserve all learnings and patterns (PTY requirements, parallel worktrees, PR review flow, fallback logic)

### Success Metrics
- Skill works unmodified in any environment with `tmux` + `Bash` tool access
- Zero references to `openclaw`, `clawdbot`, `skippy`, `~/clawd`, `sync-tmux.ts`, `process action:*`
- All agents spawned via `tmux new-session -d` (no `bash pty:true background:true` inline pattern)
- Line count stays close to 350 (current: 352) while covering 5 agents instead of 4

## Scope

### In Scope
- Rewrite of `SKILL.md` under `packages/skills/coding-agent/`
- Mapping of old `bash pty:true` + `process` tool references to pure tmux commands
- GitHub Copilot integration (using `copilot` CLI from GitHub)
- Pointer to `/cloud-coding-agent` for cloud use cases
- Updated frontmatter (drop `openclaw` metadata, drop `~/clawd` guardrail from description)
- Session naming convention preserved: `{tool}-{scope}-{id}`
- Reference to ACP/Discord runtimes for auto-notification (replaces `openclaw system event`)

### Out of Scope
- Creating helper bash scripts (skill stays as single `SKILL.md`)
- Adding new agent types beyond the 5 specified
- Modifying `/cloud-coding-agent` (only referenced, not changed)
- Modifying the `/reflect` or `/sync-learnings` integration
- Integration tests for the skill

### Future Considerations
- Helper library for tmux session management (deferred)
- Cost tracking per-agent per-session
- Auto-fallback chain (Claude Code → Codex → Copilot → manual) as a separate `/coding-agent-fallback` skill

## Technical Requirements

### Execution Model: Pure Tmux

**All agents spawn through `tmux new-session -d`.** No inline `bash pty:true background:true`. Tmux provides PTY inherently, handles session lifecycle, and survives client disconnects.

### Tmux Command Reference (replaces `process` tool)

| Old (OpenClaw) | New (pure tmux) |
|----------------|-----------------|
| `bash pty:true background:true command:"..."` | `tmux new-session -d -s NAME -c DIR '...'` |
| `process action:log sessionId:XXX` | `tmux capture-pane -t NAME -p -S -500` |
| `process action:poll sessionId:XXX` | `tmux has-session -t NAME 2>/dev/null && echo "running"` |
| `process action:write sessionId:XXX data:"y"` | `tmux send-keys -t NAME 'y'` |
| `process action:submit sessionId:XXX data:"yes"` | `tmux send-keys -t NAME 'yes' Enter` |
| `process action:kill sessionId:XXX` | `tmux kill-session -t NAME` |
| `process action:list` | `tmux list-sessions` |
| `process action:send-keys` | `tmux send-keys -t NAME 'C-c'` etc. |

### Supported Agents

| Agent | CLI | Spawn Command (in tmux) |
|-------|-----|-------------------------|
| Claude Code | `claude` | `tmux new-session -d -s cc-{id} -c {dir} 'claude "prompt"'` |
| Codex CLI | `codex` | `tmux new-session -d -s codex-{id} -c {dir} 'codex exec --full-auto "prompt"'` |
| OpenCode | `opencode` | `tmux new-session -d -s oc-{id} -c {dir} 'opencode run "prompt"'` |
| Pi | `pi` | `tmux new-session -d -s pi-{id} -c {dir} 'pi "prompt"'` |
| **GitHub Copilot** | `copilot` | `tmux new-session -d -s cop-{id} -c {dir} 'copilot suggest "prompt"'` |

### Frontmatter Changes

```yaml
# OLD
---
name: coding-agent
description: 'Delegate coding tasks to Codex, Claude Code, or Pi agents via background process...
              NOT for: ... any work in ~/clawd workspace ...
              Requires a bash tool that supports pty:true.'
metadata:
  { "openclaw": { "emoji": "🧩", "requires": { "anyBins": ["claude", "codex", "opencode", "pi"] } } }
---

# NEW
---
name: coding-agent
description: 'Delegate coding tasks to Codex, Claude Code, OpenCode, Pi, or GitHub Copilot
              agents via tmux sessions. Use when: (1) building/creating new features or apps,
              (2) reviewing PRs (spawn in temp dir or worktree), (3) refactoring large codebases,
              (4) iterative coding that needs file exploration, (5) parallel issue fixing across
              worktrees. NOT for: simple one-liner fixes (just edit), reading code (use read tool),
              or cloud/long-running tasks (use /cloud-coding-agent instead). Requires `tmux` and
              the bash tool.'
user-invocable: true
---
```

### Section Structure (target ~350 lines)

1. **Header + Intent** (10 lines)
2. **Cloud vs Local decision** (15 lines) — points to `/cloud-coding-agent`
3. **Prerequisites** (10 lines) — tmux, agent CLIs installed
4. **The Tmux Pattern** (40 lines) — spawn/monitor/interact/kill cheatsheet
5. **Quick Start: One-Shot Tasks** (30 lines) — scratch dir, git init for Codex
6. **Per-Agent Sections** (100 lines total, ~20 per agent):
   - Claude Code
   - Codex CLI (keep the detailed flags table)
   - OpenCode
   - Pi
   - GitHub Copilot
7. **Reviewing PRs** (30 lines) — temp dir / worktree patterns
8. **Parallel Issue Fixing with git worktrees** (40 lines)
9. **Batch PR Reviews (parallel army)** (25 lines)
10. **Session Naming Convention** (15 lines)
11. **Rules** (20 lines)
12. **Progress Updates** (15 lines)
13. **ACP/Discord Auto-Notify** (20 lines) — replaces OpenClaw event section
14. **Learnings** (15 lines)

### Fallback Pattern (updated)

Keep the multi-tool fallback, but strip OpenClaw references:

```
Claude Code (primary)
    ↓ capped / rate-limited / erroring
Codex CLI (secondary)
    ↓ erroring / unavailable
GitHub Copilot (tertiary)
    ↓ unavailable
Manual intervention
```

Drop `scripts/coding-tool.sh` (OpenClaw mission-control dependency). Replace with a generic availability check:

```bash
# Availability probe
claude --print "OK" --dangerously-skip-permissions 2>&1 | grep -qi "rate.limit\|capped\|exceeded\|429" && echo "CC_CAPPED" || echo "CC_OK"
codex --help &>/dev/null && echo "CODEX_OK" || echo "CODEX_MISSING"
copilot --help &>/dev/null && echo "COPILOT_OK" || echo "COPILOT_MISSING"
```

### ACP/Discord Auto-Notify Section (new, replaces OpenClaw event)

```markdown
## Auto-Notify on Completion

For long-running background agents, append a wake trigger to your prompt so the caller
harness gets notified immediately instead of polling. The mechanism depends on the runtime:

**ACP (Agent Control Protocol) runtime:**
When the harness supports ACP, the agent can emit a completion event. Append to your prompt:
`When completely finished, print "ACP:DONE <summary>" on its own line.`
The harness should watch for this marker via `tmux capture-pane`.

**Discord-threaded agents:**
If spawned from a Discord thread via the bot runtime, the bot will auto-post when the
tmux session exits. No extra prompt needed.

**Generic (no ACP/Discord):**
Poll manually with `tmux has-session -t NAME` or watch for a marker file the agent writes
on completion: `&& touch /tmp/coding-agent-<id>.done`.
```

### Workspace Safety Guardrail (generic)

Replace OpenClaw-specific rules:

```markdown
## Workspace Safety

**Never spawn a coding agent in the directory you're currently operating from.**
Agents will read files, crawl git history, and may make changes to the parent repo.
Always use one of:

1. **Temp directory**: `mktemp -d && cd $_ && git init`
2. **Git worktree**: `git worktree add /tmp/agent-work branch-name`
3. **Fresh clone**: `git clone URL /tmp/agent-review`

For PR reviews specifically, this is non-negotiable — agents will commit to whatever
branch is checked out.
```

## User Experience

### User Flows

1. **Quick one-shot task**
   - User: "Use codex to add error handling"
   - Skill: Creates tmux session, spawns codex, reports session name, lets user check progress

2. **PR review**
   - User: "Review PR #123 from owner/repo"
   - Skill: Creates temp dir, clones, checks out PR, spawns agent in tmux, returns session name

3. **Parallel issue fixing**
   - User: "Fix issues 78, 99, 112 in parallel"
   - Skill: Creates 3 worktrees, spawns 3 tmux sessions, monitors all

4. **Fallback chain on rate limit**
   - User: "Build snake game with claude"
   - Skill: Tries claude → hits rate limit → respawns in codex tmux session → continues

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| User asks for cloud/long-running task | Skill points to `/cloud-coding-agent` |
| tmux not installed | Skill reports prereq failure, does not attempt fallback |
| Agent CLI not found | Try next agent in fallback chain, report which failed |
| Session name collision | Append `-{timestamp}` suffix automatically |
| Agent hangs for >timeout | Document how to inspect with `tmux capture-pane`, don't auto-kill |
| Working dir is a protected location | Refuse to spawn, explain why (workspace safety rule) |
| Codex not in git repo | Auto-create scratch via `mktemp -d && git init` |

## Constraints & Dependencies

### Technical Constraints
- Must work with any harness that exposes `Bash` tool (not just Claude Code)
- No custom tools allowed (no `process`, no `bash pty:true` parameter)
- Must run on macOS and Linux (primary: macOS based on user's setup)

### External Dependencies
- `tmux` (any recent version with `new-session`, `send-keys`, `capture-pane`, `kill-session`)
- At least one of: `claude`, `codex`, `opencode`, `pi`, `copilot`
- `git` for worktree operations and Codex prerequisite
- `gh` for PR fetching (optional, only for PR review flow)

### Timeline Constraints
- None — this is a skill refactor, no urgency

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| User runs old skill cached in memory | Low | Med | Note at top: "If you see `bash pty:true` patterns, skill is stale — re-read" |
| GitHub Copilot CLI behavior differs from docs | Med | Med | Mark Copilot section as "experimental, verify with your CLI version" |
| tmux session name collision with other tools | Low | Low | Use prefix `cc-`/`codex-`/etc. consistently |
| Users lose OpenClaw mission-control fallback | Low | Low | Document that the generic fallback exists; if OpenClaw is detected, they can still layer their own script on top |
| Removing `~/clawd` guardrail could cause accidents in clawdbot | Low | Low | User is aware; clawdbot should manage its own skill copy |

## Decisions Made

### Key Trade-offs

**Decision 1: Pure tmux instead of hybrid `bash pty:true` + tmux**
- Alternatives considered: Keep hybrid; tmux-primary with inline for one-shots
- Rationale: User chose "Pure tmux only". One mental model, one lifecycle, works everywhere tmux is installed. PTY is automatic.

**Decision 2: Use pure tmux commands instead of wrapping in helper scripts**
- Alternatives considered: Provide `agent_log`/`agent_poll` helper wrappers
- Rationale: User chose "Pure tmux commands". Skill stays as single file, no script dependencies, users can read commands directly.

**Decision 3: Local vs cloud boundary via `/cloud-coding-agent`**
- Alternatives considered: Fallback chain (local → cloud); task-based routing
- Rationale: User chose the clean boundary. Local = fast, same machine. Cloud = long-running, teleporting, auto-PR. Skill points to `/cloud-coding-agent` at the top so users know where to go.

**Decision 4: Add GitHub Copilot as 5th agent**
- Alternatives considered: Keep at 4 agents
- Rationale: User explicitly requested parity with `ainb-tui`'s agent list.

**Decision 5: Keep length close to 350 lines**
- Alternatives considered: Trim to 200 or 150 lines
- Rationale: User chose to preserve depth of examples. Adding Copilot means we need the space.

**Decision 6: Keep `{tool}-{scope}-{id}` naming convention**
- Alternatives considered: `agent-{tool}-{id}-{slug}`; user-defined
- Rationale: Short, consistent with `ainb-tui`. Self-documenting enough.

### Deferred Decisions
- Whether to ship helper scripts (deferred — single SKILL.md for now)
- Auto-fallback as a separate skill (deferred — noted as future)
- Per-agent cost tracking (deferred — cloud-coding-agent territory)

## Implementation Notes

### Priority Order

1. **Frontmatter rewrite** — drop openclaw metadata, update description
2. **Replace `bash pty:true` → `tmux new-session -d`** throughout
3. **Replace `process action:*` → `tmux capture-pane`/`send-keys`/etc.**
4. **Remove OpenClaw-specific sections** (`scripts/coding-tool.sh`, `openclaw system event`, `~/clawd`, `Skippy`, `sync-tmux.ts`)
5. **Add GitHub Copilot section** modeled after the others
6. **Add cloud-coding-agent reference** near the top
7. **Add ACP/Discord auto-notify section** replacing OpenClaw event
8. **Generalize workspace safety guardrail**
9. **Update rules list** (drop rules 8 and 9 which are OpenClaw-specific)
10. **Update learnings section** (remove clawdbot-specific anecdotes)
11. **Sync to `~/.claude/skills/coding-agent/SKILL.md`** after edits to `packages/skills/coding-agent/SKILL.md`

### Technical Debt Accepted
- Tmux socket hygiene is not addressed (macOS `/tmp` cleanup issue we hit earlier) — users should know about `TMUX_TMPDIR` but skill won't enforce
- No automated tests for the skill (consistent with other bundled skills)
- GitHub Copilot section is experimental — exact flags may change

## Open Questions

- [ ] Does the user want `user-invocable: true` in frontmatter, or stay non-invocable? (Assumption: `user-invocable: true` since they run `/coding-agent` as a command)
- [ ] Should we add an `argument-hint` for common invocations (e.g., `[agent] [task]`)? (Assumption: no, since the skill is more of a guide than a command with args)
- [ ] Should the skill auto-detect which agents are installed and only document those? (Assumption: no — document all 5 and let the user pick)

---

*This specification was generated through systematic interview. Ready for implementation.*
