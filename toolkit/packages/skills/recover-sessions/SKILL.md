---
name: recover-sessions
description: Recover crashed or orphaned agent sessions
user-invocable: true
---

# /recover-sessions - Recover Orphaned Agent Sessions

Recover agent sessions after tmux crash, system shutdown, or disconnect. Lists orphaned sessions (tmux dead, worktree exists), allows resuming with transcript, or cleanup.

## Usage

```bash
/recover-sessions                    # List orphaned sessions
/recover-sessions list               # List orphaned sessions (same as above)
/recover-sessions status             # Show summary statistics
/recover-sessions resume <session>   # Resume session in new tmux
/recover-sessions cleanup <session>  # Archive session and optionally remove worktree
/recover-sessions cleanup-all        # Archive all orphaned sessions
```

## Implementation

```bash
#!/bin/bash

set -euo pipefail

# Source the spawn-agent library
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../utils/spawn-agent-lib.sh"

AGENTS_DIR="${HOME}/.claude/agents"
ARCHIVED_DIR="${AGENTS_DIR}/archived"

# Ensure directories exist
ensure_agent_dirs

# Parse command
CMD="${1:-list}"
shift 2>/dev/null || true

case "$CMD" in
    list|"")
        echo "==============================================================="
        echo "  Orphaned Agent Sessions"
        echo "==============================================================="
        echo ""

        ORPHAN_COUNT=0
        RESUMABLE_COUNT=0

        for META_FILE in "$AGENTS_DIR"/*.json; do
            [ -f "$META_FILE" ] || continue
            [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

            SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
            [ -z "$SESSION" ] && continue

            STATUS=$(jq -r '.status // "unknown"' "$META_FILE" 2>/dev/null)
            [ "$STATUS" = "completed" ] && continue
            [ "$STATUS" = "archived" ] && continue

            # Check if tmux session exists
            if tmux has-session -t "$SESSION" 2>/dev/null; then
                continue  # Still alive, not orphaned
            fi

            WORKTREE=$(jq -r '.directory // empty' "$META_FILE" 2>/dev/null)
            [ -z "$WORKTREE" ] && continue
            [ ! -d "$WORKTREE" ] && continue

            ((ORPHAN_COUNT++)) || true

            TASK=$(jq -r '.task // "unknown"' "$META_FILE" | head -c 60 | tr '\n' ' ')
            CREATED=$(jq -r '.created // "unknown"' "$META_FILE")
            BRANCH=$(jq -r '.worktree_branch // ""' "$META_FILE")

            # Check for transcript
            TRANSCRIPT=$(jq -r '.transcript_path // empty' "$META_FILE")
            if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
                TRANSCRIPT=$(find_transcript_path "$WORKTREE")
            fi

            CAN_RESUME="No"
            if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
                CAN_RESUME="Yes"
                ((RESUMABLE_COUNT++)) || true
            fi

            echo "  Session: $SESSION"
            echo "  Task:    ${TASK}..."
            echo "  Dir:     $WORKTREE"
            [ -n "$BRANCH" ] && echo "  Branch:  $BRANCH"
            echo "  Created: $CREATED"
            echo "  Resume:  $CAN_RESUME"
            echo ""
        done

        if [ "$ORPHAN_COUNT" -eq 0 ]; then
            echo "  No orphaned sessions found."
            echo ""
        else
            echo "---------------------------------------------------------------"
            echo "  Total orphaned: $ORPHAN_COUNT"
            echo "  Resumable:      $RESUMABLE_COUNT"
            echo ""
            echo "  Commands:"
            echo "    /recover-sessions resume <session>   - Resume in new tmux"
            echo "    /recover-sessions cleanup <session>  - Archive session"
        fi
        echo "==============================================================="
        ;;

    status)
        echo "==============================================================="
        echo "  Session Recovery Status"
        echo "==============================================================="
        echo ""

        # Count active sessions
        ACTIVE_COUNT=0
        ORPHAN_COUNT=0
        ARCHIVED_COUNT=0

        for META_FILE in "$AGENTS_DIR"/*.json; do
            [ -f "$META_FILE" ] || continue
            [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

            SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
            [ -z "$SESSION" ] && continue

            STATUS=$(jq -r '.status // "unknown"' "$META_FILE")

            if tmux has-session -t "$SESSION" 2>/dev/null; then
                ((ACTIVE_COUNT++)) || true
            elif [ "$STATUS" != "completed" ] && [ "$STATUS" != "archived" ]; then
                ((ORPHAN_COUNT++)) || true
            fi
        done

        # Count archived
        if [ -d "$ARCHIVED_DIR" ]; then
            ARCHIVED_COUNT=$(find "$ARCHIVED_DIR" -maxdepth 1 -name "*.json" -type f 2>/dev/null | wc -l | tr -d ' ')
        fi

        echo "  Active (tmux live):  $ACTIVE_COUNT"
        echo "  Orphaned:            $ORPHAN_COUNT"
        echo "  Archived:            $ARCHIVED_COUNT"
        echo ""
        echo "==============================================================="
        ;;

    resume)
        SESSION="${1:-}"
        if [ -z "$SESSION" ]; then
            echo "Session name required"
            echo "Usage: /recover-sessions resume <session>"
            echo ""
            echo "Run '/recover-sessions list' to see orphaned sessions"
            exit 1
        fi

        # Allow partial match
        if [ ! -f "${AGENTS_DIR}/${SESSION}.json" ]; then
            # Try to find matching session
            MATCH=$(find "$AGENTS_DIR" -maxdepth 1 -name "*${SESSION}*.json" -type f 2>/dev/null | head -1)
            if [ -n "$MATCH" ]; then
                SESSION=$(basename "$MATCH" .json)
            fi
        fi

        resume_session "$SESSION"
        ;;

    cleanup)
        SESSION="${1:-}"
        if [ -z "$SESSION" ]; then
            echo "Session name required"
            echo "Usage: /recover-sessions cleanup <session>"
            exit 1
        fi

        META_FILE="${AGENTS_DIR}/${SESSION}.json"
        if [ ! -f "$META_FILE" ]; then
            # Try partial match
            MATCH=$(find "$AGENTS_DIR" -maxdepth 1 -name "*${SESSION}*.json" -type f 2>/dev/null | head -1)
            if [ -n "$MATCH" ]; then
                META_FILE="$MATCH"
                SESSION=$(basename "$MATCH" .json)
            fi
        fi

        if [ ! -f "$META_FILE" ]; then
            echo "Session not found: $SESSION"
            exit 1
        fi

        WORKTREE=$(jq -r '.directory // empty' "$META_FILE")
        WITH_WORKTREE=$(jq -r '.with_worktree // false' "$META_FILE")
        BRANCH=$(jq -r '.worktree_branch // empty' "$META_FILE")

        echo "Archiving session: $SESSION"
        archive_session "$SESSION"
        echo "Session archived to: ${ARCHIVED_DIR}/${SESSION}.json"

        # Offer worktree cleanup if applicable
        if [ "$WITH_WORKTREE" = "true" ] && [ -n "$WORKTREE" ] && [ -d "$WORKTREE" ]; then
            echo ""
            echo "Worktree still exists at: $WORKTREE"
            echo "   Branch: $BRANCH"
            echo ""
            echo "   To remove worktree and branch:"
            echo "   git worktree remove \"$WORKTREE\" && git branch -D \"$BRANCH\""
            echo ""
            echo "   Or use: /cleanup-agent-worktree ${SESSION#agent-}"
        fi
        ;;

    cleanup-all)
        echo "Archiving all orphaned sessions..."
        echo ""

        ARCHIVED=0

        for META_FILE in "$AGENTS_DIR"/*.json; do
            [ -f "$META_FILE" ] || continue
            [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

            SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
            [ -z "$SESSION" ] && continue

            STATUS=$(jq -r '.status // "unknown"' "$META_FILE")
            [ "$STATUS" = "completed" ] && continue
            [ "$STATUS" = "archived" ] && continue

            # Check if tmux session exists
            if ! tmux has-session -t "$SESSION" 2>/dev/null; then
                echo "  Archiving: $SESSION"
                archive_session "$SESSION"
                ((ARCHIVED++)) || true
            fi
        done

        echo ""
        echo "Archived $ARCHIVED sessions"
        echo ""
        echo "Note: Worktrees were NOT removed. Use /list-agent-worktrees to see them."
        ;;

    help|*)
        echo "==============================================================="
        echo "  /recover-sessions - Recover Orphaned Agent Sessions"
        echo "==============================================================="
        echo ""
        echo "  Commands:"
        echo ""
        echo "    list              List all orphaned sessions"
        echo "    status            Show summary: active, orphaned, archived"
        echo "    resume <session>  Resume session in new tmux with transcript"
        echo "    cleanup <session> Archive session metadata"
        echo "    cleanup-all       Archive all orphaned sessions"
        echo "    help              Show this help"
        echo ""
        echo "  What is an orphaned session?"
        echo "    A session where tmux died but the worktree/metadata still exists."
        echo "    This happens after system shutdown, network disconnect, or crash."
        echo ""
        echo "  Resuming sessions:"
        echo "    If a transcript exists, the session can be resumed with full"
        echo "    conversation history using 'claude --resume'."
        echo ""
        echo "==============================================================="
        ;;
esac

exit 0
```

## Notes

- Orphaned sessions are detected by checking if tmux session exists for each metadata file
- Resume requires a valid transcript file (stored by Claude at `~/.claude/projects/`)
- Archiving moves metadata to `~/.claude/agents/archived/` - worktrees are NOT auto-removed
- Use `/cleanup-agent-worktree` or manual git commands to remove worktrees after archiving

## Recovery Workflow

1. After system crash/restart, run `/recover-sessions list`
2. For sessions you want to continue: `/recover-sessions resume <session>`
3. For abandoned sessions: `/recover-sessions cleanup <session>` then `/cleanup-agent-worktree`
4. Quick cleanup: `/recover-sessions cleanup-all` to archive all orphaned metadata
