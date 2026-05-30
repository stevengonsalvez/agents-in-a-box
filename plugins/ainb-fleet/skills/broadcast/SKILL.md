---
name: ainb-fleet:broadcast
description: |
  Fan out a single prompt to selected claude sessions across the fleet.
  Use when you need to apply the same instruction (e.g. `/clear`,
  `git pull`, `remote-control disconnect`) to many sessions at once.
  Routing: peers-first (broker HTTP) when peer registered, tmux send-keys
  fallback otherwise. Refuses to run without an explicit targeting flag
  (--all, --filter <regex>, or --cwd <substring>) — no implicit fan-out.
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
  ✓ <name> via broker peer <peer_id>
  ✓ <name> via tmux <tmux_session>
  ✗ <name>: <reason>
```

`✓ via broker` = sent through claude-peers HTTP (clean, structured).
`✓ via tmux` = sent via `tmux send-keys -l` (literal mode, works for any
tmux pane regardless of peer state).

## Routing rule

For each target:

1. If session has `peer_id` and broker is healthy → POST `/send-message` to broker.
2. Else if session has `tmux_session` and tmux says it exists → `tmux send-keys -l`.
3. Else → `Failed { reason: "no peer registered and no tmux session found" }`.

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
