---
name: tmux-message
description: Reliable peer-to-peer message delivery to other Claude Code instances via tmux send-keys. Use as a fallback when claude-peers MCP send_message fails to surface in the receiver's inbox (delivered server-side but receiver never picks it up — observed behaviour). Also use when sending a directive to a known Claude Code TUI session by tmux session name or fuzzy hint, or when injecting a multi-line directive into a peer's prompt and submitting it. Trigger phrases — "claude-peers fallback", "tmux send-keys", "send to peer via tmux", "inject directive", "deliver to nanoclaw/hermes peer", "peer message". Tmux-only — won't reach peers running outside tmux.
---

# tmux-message

Deliver a message to another Claude Code TUI session running in tmux. Use the bundled `scripts/send.sh` — it handles session resolution, pre-flight safety checks, paste-buffer delivery, and post-flight verification.

## When to use

- claude-peers MCP `send_message` returned success but the receiver never acted on the message (silent failure mode observed in this fleet).
- Sending a directive/handoff to a peer Claude Code session whose tmux session name is known (full or fuzzy).
- Need to inject a multi-line message into another Claude Code TUI prompt and submit it as if the user typed it.

Do NOT use for — peers outside tmux, shell-only sessions (no Claude Code TUI), or one-shot commands you would just paste yourself.

## Usage

```bash
# Send file content
~/.claude/skills/tmux-message/scripts/send.sh <session-or-hint> <message-file>

# Send via stdin
echo "ping from hermes peer" | ~/.claude/skills/tmux-message/scripts/send.sh nanoclaw -
```

`<session-or-hint>` accepts either an exact tmux session name OR a unique substring. `tmux ls` is grepped — if zero or multiple matches, the script aborts and lists them.

## Workflow

1. **Compose the message in a file**. Always prefix with an origin marker so the receiver knows where it came from:

   ```
   [from <my-peer-id-or-cwd>]

   <directive body>
   ```

2. **Run send.sh**. It will:
   - Resolve the session (exact → fuzzy)
   - `capture-pane` and check for: idle `❯ ` prompt (good), activity spinner (warn), permission prompt (abort)
   - `load-buffer` → `paste-buffer` → `Enter`
   - Re-capture pane and report whether an activity indicator appeared (i.e. receiver submitted)

3. **Inspect** the receiver afterwards if needed:
   ```bash
   tmux capture-pane -t <session> -p | tail -20
   ```

## Safety gates

The script aborts in these cases (override carefully):

- **Permission prompt detected** (`Do you want to allow/approve/run`) — paste would be captured by the prompt handler instead of routed as input. Always abort. No override.
- **Mid-tool-call activity** (`✻`, `✢`, `⚒`, `Frosting…`, etc.) with no idle prompt visible — queueing into the current input buffer is risky. Override with `TMUX_MESSAGE_FORCE=1` only when you understand the receiver state.

## Edge cases

- **Long messages (>5KB)** — paste-buffer may exceed tmux buffer limits on some versions. Prefer writing the body to a file the receiver can read; the message you send is then just `see /tmp/handoff-X.md`.
- **Receiver at shell prompt (no Claude Code)** — paste still happens; Enter runs it as a shell command. Pre-flight catches absence of `❯ ` and warns.
- **`Remote Control active` line in status bar** — indicates `/remote-control` skill is running on the receiver. Delivery still works.
- **Multiple panes in the session** — script targets the active pane only. For multi-pane sessions, append `:0.0` (window:pane) to the session name in the first arg.

## Tradeoffs

- Tmux-only.
- Receiver sees the message as if the user typed it. Provenance only via the `[from X]` prefix you include — no read receipts.
- Post-flight check confirms an activity indicator appeared but not that the receiver actually understood the directive. Treat as "delivered" not "actioned".
- Message files at `/tmp/tmux-message-*.txt` are NOT auto-cleaned; that's intentional for replay/debugging. Sweep periodically.
