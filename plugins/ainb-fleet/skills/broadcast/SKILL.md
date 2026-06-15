---
name: ainb-fleet:broadcast
description: |
  Fan out a single prompt to selected claude sessions across the fleet.
  Use when you need to apply the same instruction (e.g. `/clear`,
  `git pull`, `remote-control disconnect`) to many sessions at once.
  Routing: tmux send-keys by default (the broker is an opt-in fallback,
  toggled via AINB_FLEET_TRANSPORT). Refuses to run without an explicit
  targeting flag (--all, --filter <regex>, or --cwd <substring>) — no
  implicit fan-out.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:broadcast
  - fleet broadcast
  - broadcast to fleet
  - send to all sessions
  - fan out prompt
allowed-tools:
  - Bash
---

# ainb fleet:broadcast

Send one prompt to selected sessions. Mandatory targeting flag.

## Run

```bash
ainb fleet broadcast "<prompt>" --all                       # every running session
ainb fleet broadcast "<prompt>" --filter "<regex>"          # match tmux/workspace name
ainb fleet broadcast "<prompt>" --cwd "<substring>"         # match cwd
```

## Targeting flags (one required)

| flag | matches against |
|---|---|
| `--all` | every session in `ainb fleet standup` |
| `--filter <regex>` | regex against `tmux_session` OR `workspace_name` |
| `--cwd <substring>` | substring against `cwd` |

If none provided, the command exits with `broadcast requires --all,
--filter <regex>, or --cwd <substring>` — by design, to prevent
accidental fan-out.

## Output

```
ainb fleet broadcast — sent to N target(s)
  ✓ <name> via tmux <tmux_session>
  ✓ <name> via broker peer <peer_id>
  ✗ <name>: <reason>
```

`✓ via tmux` = sent via `tmux send-keys -l` (literal mode, works for any
tmux pane regardless of peer state) — this is the default path.
`✓ via broker` = sent through claude-peers HTTP — only seen when a target
has no live tmux pane (or `AINB_FLEET_TRANSPORT=peers` is set).

## Routing rule

Controlled by `AINB_FLEET_TRANSPORT` (default `tmux-first`). For each target:

**Default (`tmux-first`):**

1. If session has `tmux_session` and tmux says it exists → `tmux send-keys -l`.
2. Else if session has `peer_id` and broker is healthy → POST `/send-message` to broker.
3. Else → `Failed { reason: "no live tmux session and no reachable broker peer" }`.

**`tmux-only`:** step 1 only; never touches the broker. Failure reason notes
`(transport=tmux-only)`.

**`peers` / `broker` (legacy):** the old order — broker first, tmux fallback.

## Common flows

**Cycle a specific app:**
```bash
ainb fleet broadcast "/clear" --filter "shotclubhouse"
```

**Pull latest in every active worktree:**
```bash
ainb fleet broadcast "git pull" --cwd "/agents-in-a-box/worktrees/"
```

**Universal reset:**
```bash
ainb fleet broadcast "/exit" --all   # careful — quits everything
```

## Safety notes

- `tmux send-keys -l` uses literal mode → shell metacharacters in the
  prompt are safe (no injection).
- Multi-line prompts: newlines in the broadcast text get sent as literal
  characters; the receiving claude reads them as a single message.
- The Enter key is sent *after* the literal text → claude sees the prompt
  as a submitted user message.

## Verify a broadcast landed

For each target, tail the JSONL transcript:

```bash
ls -t ~/.claude/projects/<cwd-slug>/*.jsonl | head -1 | xargs grep "<prompt-text>"
```

(`<cwd-slug>` = replace `/` with `-` in the target's cwd.)
