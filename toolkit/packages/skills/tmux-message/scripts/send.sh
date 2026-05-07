#!/usr/bin/env bash
# tmux-message: deliver a multi-line message to another Claude Code session
# running in tmux. Use as a fallback when claude-peers MCP send_message
# fails to surface in the receiver's inbox.
#
# Usage:
#   send.sh <session-or-hint> <message-file>
#   send.sh <session-or-hint> -      # read message from stdin
#
# Examples:
#   send.sh nanoclaw /tmp/directive.md
#   echo "ping" | send.sh hermes-agent -
#
# Flow:
#   1. Resolve session: exact match, else fuzzy via grep on `tmux ls`
#   2. Pre-flight: capture-pane and check for idle prompt
#   3. load-buffer + paste-buffer + Enter
#   4. Post-flight: capture-pane and check for activity indicator

set -u

if [ $# -lt 2 ]; then
    cat >&2 <<EOF
usage: $0 <session-or-hint> <message-file|->
   <session-or-hint>: tmux session name (exact) or unique fuzzy substring
   <message-file>:    path to file containing message body, or - for stdin
EOF
    exit 2
fi

HINT="$1"
SRC="$2"

# --- 1. Resolve session ---
if tmux has-session -t "$HINT" 2>/dev/null; then
    SESSION="$HINT"
else
    matches=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep -F "$HINT" || true)
    count=$(printf '%s\n' "$matches" | grep -c . || true)
    if [ "$count" -eq 0 ]; then
        echo "ERROR: no tmux session matching '$HINT'" >&2
        echo "Available sessions:" >&2
        tmux list-sessions -F '  #{session_name}' >&2 || true
        exit 3
    elif [ "$count" -gt 1 ]; then
        echo "ERROR: hint '$HINT' is ambiguous, $count matches:" >&2
        printf '  %s\n' $matches >&2
        exit 4
    fi
    SESSION="$matches"
fi

# --- 2. Stage message file ---
if [ "$SRC" = "-" ]; then
    MSG_FILE="/tmp/tmux-message-$(date +%s)-$$.txt"
    cat > "$MSG_FILE"
else
    if [ ! -f "$SRC" ]; then
        echo "ERROR: message file not found: $SRC" >&2
        exit 5
    fi
    MSG_FILE="$SRC"
fi

# --- 3. Pre-flight: inspect receiver state ---
PANE=$(tmux capture-pane -t "$SESSION" -p)
last_lines=$(printf '%s\n' "$PANE" | tail -30)

# Detect Claude Code prompt: a line starting with "❯ " (idle) means safe to inject
has_prompt=$(printf '%s' "$last_lines" | grep -c '^❯ ' || true)

# Detect mid-tool-call activity indicators
has_activity=$(printf '%s' "$last_lines" | grep -cE '✻|✢|⚒|✶|⏺|Thinking…|Crunched|Frosting|Working' || true)

# Detect permission prompts (would steal our paste)
has_permission=$(printf '%s' "$last_lines" | grep -cE 'Do you want to (allow|approve|run)' || true)

if [ "$has_permission" -gt 0 ]; then
    echo "ERROR: receiver is at a permission prompt — refusing to inject" >&2
    echo "Last lines of pane:" >&2
    printf '%s\n' "$last_lines" | tail -8 >&2
    exit 6
fi

if [ "$has_activity" -gt 0 ] && [ "$has_prompt" -eq 0 ]; then
    echo "WARNING: receiver appears mid-tool-call (activity indicator present, no idle prompt)" >&2
    echo "Last 6 lines:" >&2
    printf '%s\n' "$last_lines" | tail -6 >&2
    echo "Set TMUX_MESSAGE_FORCE=1 to inject anyway (will queue at current input cursor)" >&2
    if [ "${TMUX_MESSAGE_FORCE:-0}" != "1" ]; then
        exit 7
    fi
fi

# --- 4. Deliver: load-buffer + paste-buffer + Enter ---
tmux load-buffer "$MSG_FILE"
tmux paste-buffer -t "$SESSION"
sleep 1
tmux send-keys -t "$SESSION" Enter

# --- 5. Post-flight verify ---
sleep 4
POST=$(tmux capture-pane -t "$SESSION" -p | tail -10)
post_activity=$(printf '%s' "$POST" | grep -cE '✻|✢|⚒|✶|⏺|Thinking…|Crunched|Frosting|Working' || true)

bytes=$(wc -c < "$MSG_FILE" | tr -d ' ')
echo "✓ delivered to session: $SESSION"
echo "  message file: $MSG_FILE ($bytes bytes)"
if [ "$post_activity" -gt 0 ]; then
    echo "  status: receiver picked up (activity indicator present)"
else
    echo "  status: ⚠ no activity indicator — receiver may not have submitted, check pane"
fi
echo
echo "verify with:  tmux capture-pane -t '$SESSION' -p | tail -20"
