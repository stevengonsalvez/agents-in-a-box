---
name: coding-agent
description: 'Delegate coding tasks to Codex, Claude Code, OpenCode, Pi, or GitHub Copilot agents via tmux sessions. Use when: (1) building/creating new features or apps, (2) reviewing PRs (spawn in temp dir or worktree), (3) refactoring large codebases, (4) iterative coding that needs file exploration, (5) parallel issue fixing across worktrees. NOT for: simple one-liner fixes (just edit), reading code (use read tool), or cloud/long-running tasks (use /cloud-coding-agent instead). Requires `tmux` and the Bash tool.'
user-invocable: true
---

# Coding Agent (tmux-first)

Delegate coding tasks to local agents running inside tmux sessions. Every agent spawn goes through `tmux new-session -d` — tmux provides the PTY, handles session lifecycle, and survives disconnects. One mental model, one lifecycle, portable across any harness with a `Bash` tool.

## When to use this vs `/cloud-coding-agent`

| Use `/coding-agent` (this skill) | Use `/cloud-coding-agent` |
|----------------------------------|---------------------------|
| Fast, local machine | Long-running (>30 min) tasks |
| You need immediate shell access | You want session teleporting across machines |
| Interactive debugging / questions | Auto-PR creation from a cloud worker |
| Parallel worktrees on the same host | Cloud-hosted sandboxed runs |
| Rate-limited local CLIs (you control) | Cloud CLIs with independent quotas |

If the task needs to survive your laptop sleeping, or you want a PR auto-opened from a CI-like environment, stop reading and use `/cloud-coding-agent` instead.

---

## Prerequisites

- `tmux` installed (`brew install tmux` on macOS, `apt install tmux` on Linux)
- At least one agent CLI on `PATH`: `claude`, `codex`, `opencode`, `pi`, or `copilot`
- `git` (required for Codex, used for worktrees)
- `gh` (optional, for PR fetching)

Quick probe:

```bash
tmux -V && echo "tmux OK" || echo "tmux MISSING"
for cli in claude codex opencode pi copilot; do
  command -v "$cli" >/dev/null 2>&1 && echo "$cli OK" || echo "$cli MISSING"
done
```

---

## The Tmux Pattern (core cheatsheet)

Every agent operation reduces to these tmux commands. Learn them once.

### Spawn an agent

```bash
# Foreground is impossible — tmux new-session -d is always detached.
# PTY is allocated automatically by tmux.
tmux new-session -d -s cc-issue-174 -c ~/project 'claude "Fix the login bug"'
```

| Flag | Purpose |
|------|---------|
| `-d` | Detached (required — never omit) |
| `-s NAME` | Session name (see Session Naming Convention below) |
| `-c DIR` | Working directory — agent's view of the world |
| `'cmd'` | The agent invocation; quote carefully if prompt has special chars |

### Monitor progress

```bash
# Grab the last 500 lines of output
tmux capture-pane -t cc-issue-174 -p -S -500

# Check if the session is still alive
tmux has-session -t cc-issue-174 2>/dev/null && echo "running" || echo "done"

# List all running agent sessions
tmux list-sessions 2>/dev/null
```

### Interact with a running agent

```bash
# Send text and press Enter (answer "yes" to a prompt)
tmux send-keys -t cc-issue-174 'yes' Enter

# Send raw keys without Enter (type without submitting)
tmux send-keys -t cc-issue-174 'y'

# Send a control sequence (Ctrl+C to cancel)
tmux send-keys -t cc-issue-174 C-c

# Paste a multi-line prompt
tmux send-keys -t cc-issue-174 'fix this' Enter 'then run tests' Enter
```

### Kill a session

```bash
tmux kill-session -t cc-issue-174
```

**Never** use `tmux kill-server`, `pkill tmux`, or `killall tmux` — these nuke every tmux session on the machine, including your own dev environments and other users' agents. Always kill by session name.

---

## Quick Start: One-Shot Tasks

For quick tasks where you want the agent to run-and-exit, still use tmux:

```bash
# Quick scratch task (Codex needs a git repo!)
SCRATCH=$(mktemp -d) && (cd $SCRATCH && git init -q)
tmux new-session -d -s codex-scratch-$$ -c $SCRATCH 'codex exec "Write a fizzbuzz in Rust"'

# Wait for it to finish
while tmux has-session -t codex-scratch-$$ 2>/dev/null; do sleep 2; done

# Grab the result
tmux capture-pane -t codex-scratch-$$ -p -S -200
```

**Why `git init`?** Codex refuses to run outside a trusted git directory. Creating a temp repo is a one-liner that unblocks all scratch work.

---

## Multi-Agent Fallback (default pattern)

Don't waste time debugging rate limits — switch agents immediately.

```
Claude Code (primary)
    ↓ capped / rate-limited / erroring
Codex CLI (secondary)
    ↓ unavailable
GitHub Copilot (tertiary)
    ↓ unavailable
Manual intervention
```

### Availability probe

```bash
# Claude Code — rate-limit check
claude --print "OK" --dangerously-skip-permissions 2>&1 | \
  grep -qi "rate.limit\|capped\|exceeded\|429" && echo "CC_CAPPED" || echo "CC_OK"

# Codex — just needs to exist
codex --help &>/dev/null && echo "CODEX_OK" || echo "CODEX_MISSING"

# Copilot
copilot --help &>/dev/null && echo "COPILOT_OK" || echo "COPILOT_MISSING"
```

### Mid-task fallback

```bash
# Kill the failing session
tmux kill-session -t cc-issue-174

# Respawn with the next agent (same task, different tool)
tmux new-session -d -s codex-issue-174 -c ~/project 'codex --yolo exec "Fix the login bug"'
```

---

## Per-Agent Reference

### Claude Code

```bash
# Spawn in tmux
tmux new-session -d -s cc-build-snake -c ~/project \
  'claude "Build a snake game in vanilla JS"'

# With dangerous permissions (skip approvals)
tmux new-session -d -s cc-refactor-auth -c ~/project \
  'claude --dangerously-skip-permissions "Refactor the auth module"'
```

### Codex CLI

**Default model:** `gpt-5.2-codex` (configured in `~/.codex/config.toml`)

| Flag | Effect |
|------|--------|
| `exec "prompt"` | One-shot execution, exits when done |
| `--full-auto` | Sandboxed, auto-approves changes in workspace |
| `--yolo` | NO sandbox, NO approvals (fastest, most dangerous) |

```bash
# Quick build (auto-approves inside workspace)
tmux new-session -d -s codex-dark-mode -c ~/project \
  'codex exec --full-auto "Build a dark mode toggle"'

# Long-running refactor (yolo mode, no sandbox)
tmux new-session -d -s codex-auth-refactor -c ~/project \
  'codex --yolo exec "Refactor the auth module into feature modules"'
```

### OpenCode

```bash
tmux new-session -d -s oc-task-42 -c ~/project 'opencode run "Your task"'
```

### Pi (pi-coding-agent)

Install: `npm install -g @mariozechner/pi-coding-agent`

```bash
# Interactive
tmux new-session -d -s pi-task -c ~/project 'pi "Your task"'

# Non-interactive one-shot
tmux new-session -d -s pi-summary -c ~/project 'pi -p "Summarize src/"'

# Different provider/model
tmux new-session -d -s pi-cheap -c ~/project \
  'pi --provider openai --model gpt-4o-mini -p "Your task"'
```

Note: Pi has Anthropic prompt caching enabled (PR #584, merged Jan 2026).

### GitHub Copilot

The `copilot` CLI provides suggestions and explanations. Experimental — verify flags with your installed version (`copilot --help`).

```bash
# Suggest a shell command or code snippet
tmux new-session -d -s cop-suggest-api -c ~/project \
  'copilot suggest "REST endpoint for creating a user in Express"'

# Explain existing code
tmux new-session -d -s cop-explain-main -c ~/project \
  'copilot explain "$(cat src/main.rs)"'
```

Copilot is best as a **tertiary fallback** for quick suggestions. For complex multi-file work, prefer Claude Code, Codex, or OpenCode.

---

## Reviewing PRs

**Never review PRs in the working directory you're operating from.** Always clone to temp or use a worktree — agents will read, crawl git history, and may commit to whatever branch is checked out.

### Temp clone (simplest)

```bash
REVIEW_DIR=$(mktemp -d)
git clone https://github.com/user/repo.git $REVIEW_DIR
cd $REVIEW_DIR && gh pr checkout 130

tmux new-session -d -s codex-pr-130 -c $REVIEW_DIR \
  'codex exec "Review this PR against origin/main. Report bugs, style issues, security concerns."'

# Wait for completion
while tmux has-session -t codex-pr-130 2>/dev/null; do sleep 5; done

# Grab the review
tmux capture-pane -t codex-pr-130 -p -S -1000 > /tmp/pr-130-review.md

# Clean up
rm -rf $REVIEW_DIR
```

### Git worktree (keeps main intact)

```bash
git worktree add /tmp/pr-130-review pr-130-branch

tmux new-session -d -s codex-pr-130 -c /tmp/pr-130-review \
  'codex exec "Review against main branch"'

# Cleanup after done
git worktree remove /tmp/pr-130-review
```

---

## Parallel Issue Fixing with git worktrees

For fixing multiple issues in parallel, spawn one worktree + one tmux session per issue.

```bash
# 1. Create worktrees
git worktree add -b fix/issue-78 /tmp/issue-78 main
git worktree add -b fix/issue-99 /tmp/issue-99 main
git worktree add -b fix/issue-112 /tmp/issue-112 main

# 2. Launch an agent in each tmux session
tmux new-session -d -s codex-fix-78 -c /tmp/issue-78 \
  'pnpm install && codex --yolo exec "Fix issue #78: <description>. Commit and push."'
tmux new-session -d -s codex-fix-99 -c /tmp/issue-99 \
  'pnpm install && codex --yolo exec "Fix issue #99: <description>. Commit and push."'
tmux new-session -d -s codex-fix-112 -c /tmp/issue-112 \
  'pnpm install && codex --yolo exec "Fix issue #112: <description>. Commit and push."'

# 3. Monitor all running agents
tmux list-sessions | grep codex-fix

# 4. Watch a specific one
tmux capture-pane -t codex-fix-78 -p -S -500

# 5. Create PRs after fixes complete
for issue in 78 99 112; do
  cd /tmp/issue-$issue
  git push -u origin fix/issue-$issue
  gh pr create --head fix/issue-$issue --title "fix: issue #$issue" --body "Fixed by agent"
done

# 6. Cleanup
git worktree remove /tmp/issue-78
git worktree remove /tmp/issue-99
git worktree remove /tmp/issue-112
```

---

## Batch PR Reviews (parallel army)

Deploy one agent per PR for bulk review.

```bash
# Fetch all PR refs
git fetch origin '+refs/pull/*/head:refs/remotes/origin/pr/*'

# Spawn reviewers (all use the same clean checkout)
for pr in 86 87 88; do
  tmux new-session -d -s codex-review-$pr -c ~/project \
    "codex exec 'Review PR #$pr. Check git diff origin/main...origin/pr/$pr'"
done

# Monitor
tmux list-sessions | grep codex-review

# Collect results and post
for pr in 86 87 88; do
  # Wait for it to finish
  while tmux has-session -t codex-review-$pr 2>/dev/null; do sleep 5; done
  REVIEW=$(tmux capture-pane -t codex-review-$pr -p -S -2000)
  gh pr comment $pr --body "$REVIEW"
done
```

---

## Session Naming Convention

Use consistent names so sessions are self-documenting and searchable with `tmux list-sessions | grep`.

```
{tool}-{scope}-{id}[-{desc}]
```

| Tool prefix | Agent |
|-------------|-------|
| `cc-` | Claude Code |
| `codex-` | Codex CLI |
| `oc-` | OpenCode |
| `pi-` | Pi |
| `cop-` | GitHub Copilot |

**Examples:**
- `cc-issue-174-auth-refactor`
- `codex-fix-1520`
- `codex-pr-86-security`
- `cop-suggest-api`
- `pi-summary-src`

Keep names under ~50 chars. Tmux allows longer but it gets ugly in `list-sessions`.

---

## Workspace Safety

**Never spawn a coding agent in the directory you're currently operating from.** Agents will read files, crawl git history, and may make changes to the parent repo. Always use one of:

1. **Temp directory**: `mktemp -d && cd $_ && git init`
2. **Git worktree**: `git worktree add /tmp/agent-work branch-name`
3. **Fresh clone**: `git clone URL /tmp/agent-review`

For PR reviews specifically, this is non-negotiable — agents will commit to whatever branch is checked out.

---

## Rules

1. **Always use tmux** — one execution model, one lifecycle. PTY is automatic.
2. **Respect tool choice** — if the user asks for Codex, use Codex. Don't silently swap agents unless falling back after a failure.
3. **Orchestrator mode: don't hand-code patches yourself** when an agent is running. If an agent fails or hangs, respawn it or escalate to the user.
4. **Be patient** — don't kill sessions because they're "slow". Check `tmux capture-pane` first.
5. **Monitor with `capture-pane`** — never `attach` to a background session (you'll block the harness).
6. **`--full-auto` for building, vanilla for reviewing** — reviews should be observational, not destructive.
7. **Parallel is OK** — run many tmux sessions concurrently for batch work.
8. **Workspace safety is non-negotiable** — temp dir, worktree, or fresh clone. Never spawn in your own working directory.
9. **Kill by session name only** — `tmux kill-session -t NAME`. Never `kill-server` or `pkill tmux`.

---

## Progress Updates

When you spawn coding agents in tmux, keep the user in the loop.

- Send **1 short message when you start**: what's running, which session name, where (workdir).
- Only update again when something changes:
  - A milestone completes (build finished, tests passed)
  - The agent asks a question / needs input
  - You hit an error or need user action
  - The agent finishes (include what changed + where)
- If you kill a session, immediately say you killed it and why.

This prevents the user from seeing only "Agent failed" with no idea what happened.

---

## Auto-Notify on Completion

For long-running background agents, append a completion marker to your prompt so the caller harness knows immediately instead of polling.

### ACP (Agent Control Protocol) runtime

When the harness supports ACP, the agent can emit a completion event. Append to your prompt:

```
When completely finished, print "ACP:DONE <brief summary>" on its own line.
```

The harness watches for this marker via `tmux capture-pane` and dispatches a wake event.

### Discord-threaded agents

If the agent was spawned from a Discord thread via a bot runtime, the bot will auto-post when the tmux session exits (session end = task done). No extra prompt needed.

### Generic (no ACP/Discord)

Poll manually or watch for a marker file the agent writes on completion:

```bash
tmux new-session -d -s codex-build-$$ -c ~/project \
  'codex --yolo exec "Build the API, then: touch /tmp/codex-build-done"'

# Block until marker appears
while [ ! -f /tmp/codex-build-done ]; do sleep 5; done
```

---

## Learnings (2026)

- **Tmux is enough** — you don't need a custom `process` tool or `bash pty:true` wrapper. `tmux new-session -d` + `capture-pane` + `send-keys` covers 100% of the lifecycle.
- **Git repo required for Codex** — `mktemp -d && git init -q` is the unblocker for any scratch work.
- **`exec` is your friend** — `codex exec "prompt"` runs and exits cleanly, perfect for one-shots (wrap in tmux anyway for consistency).
- **`send-keys` vs `send-keys Enter`** — without `Enter`, you're just typing. With `Enter`, you're submitting. Useful for pre-filling a prompt before confirming.
- **Don't attach** — from a harness context, attaching blocks everything. Always `capture-pane -p` instead.
- **Parallel fanout is cheap** — a dozen tmux sessions cost nothing. Use them liberally for batch work (PR reviews, worktree fixes).
- **Session names are search keys** — the `{tool}-{scope}-{id}` convention is what lets `tmux list-sessions | grep` replace every "which agent is running?" query.
