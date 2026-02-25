#!/bin/bash

# Spawn Agent Library
# Core agent spawning functions extracted from spawn-agent.md
# Used by orchestrator-runner.sh for multi-agent orchestration
# Enhanced with session persistence and recovery support

set -euo pipefail

# Constants
AGENTS_DIR="${HOME}/.claude/agents"
REGISTRY_FILE="${AGENTS_DIR}/registry.jsonl"
ARCHIVED_DIR="${AGENTS_DIR}/archived"

# Check if already running inside a Claude Code session
# This prevents nested session errors when spawning agents
is_inside_claude_session() {
    [ -n "${CLAUDECODE:-}" ]
}

# Ensure directories exist
ensure_agent_dirs() {
    mkdir -p "$AGENTS_DIR"
    mkdir -p "$ARCHIVED_DIR"
}

# Function: Find Claude transcript path for a worktree directory
# Usage: find_transcript_path <work_dir>
# Returns: Path to transcript file or empty string if not found
find_transcript_path() {
    local WORK_DIR="$1"
    local PROJECTS_DIR="${HOME}/.claude/projects"

    # Claude encodes paths by replacing / with - and removing leading slash
    # Example: /Users/foo/bar -> -Users-foo-bar
    local ENCODED_PATH
    ENCODED_PATH=$(echo "$WORK_DIR" | sed 's|^/||' | sed 's|/|-|g')

    # Look for the project directory
    local PROJECT_DIR="${PROJECTS_DIR}/-${ENCODED_PATH}"

    if [ -d "$PROJECT_DIR" ]; then
        # Find the most recent .jsonl file (transcript)
        local TRANSCRIPT
        TRANSCRIPT=$(find "$PROJECT_DIR" -maxdepth 1 -name "*.jsonl" -type f 2>/dev/null | \
                     xargs ls -t 2>/dev/null | head -1)
        if [ -n "$TRANSCRIPT" ]; then
            echo "$TRANSCRIPT"
            return 0
        fi
    fi

    # Fallback: search for project directories containing our path
    local FOUND_DIR
    FOUND_DIR=$(find "$PROJECTS_DIR" -maxdepth 1 -type d -name "*${ENCODED_PATH}*" 2>/dev/null | head -1)

    if [ -n "$FOUND_DIR" ] && [ -d "$FOUND_DIR" ]; then
        local TRANSCRIPT
        TRANSCRIPT=$(find "$FOUND_DIR" -maxdepth 1 -name "*.jsonl" -type f 2>/dev/null | \
                     xargs ls -t 2>/dev/null | head -1)
        if [ -n "$TRANSCRIPT" ]; then
            echo "$TRANSCRIPT"
            return 0
        fi
    fi

    echo ""
    return 1
}

# Function: Get tmux server PID for session
# Usage: get_tmux_pid <session_name>
get_tmux_pid() {
    local SESSION="$1"
    tmux list-sessions -F '#{session_name}:#{pid}' 2>/dev/null | \
        grep "^${SESSION}:" | cut -d: -f2 || echo ""
}

# Function: Append event to registry
# Usage: append_registry_event <event_type> <session> [additional_json_fields]
append_registry_event() {
    local EVENT="$1"
    local SESSION="$2"
    shift 2
    local EXTRA="$*"

    ensure_agent_dirs

    local TIMESTAMP
    TIMESTAMP=$(date -Iseconds)

    if [ -n "$EXTRA" ]; then
        echo "{\"event\":\"$EVENT\",\"session\":\"$SESSION\",\"timestamp\":\"$TIMESTAMP\",$EXTRA}" >> "$REGISTRY_FILE"
    else
        echo "{\"event\":\"$EVENT\",\"session\":\"$SESSION\",\"timestamp\":\"$TIMESTAMP\"}" >> "$REGISTRY_FILE"
    fi
}

# Function: Save agent metadata with enhanced fields
# Usage: save_agent_metadata <session> <task> <work_dir> <with_handover> <with_worktree> <worktree_branch> [orchestration]
save_agent_metadata() {
    local SESSION="$1"
    local TASK="$2"
    local WORK_DIR="$3"
    local WITH_HANDOVER="$4"
    local WITH_WORKTREE="$5"
    local WORKTREE_BRANCH="$6"
    local ORCHESTRATION="${7:-null}"

    ensure_agent_dirs

    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"
    local CREATED
    CREATED=$(date -Iseconds)

    # Get tmux PID
    local TMUX_PID
    TMUX_PID=$(get_tmux_pid "$SESSION")

    # Try to find transcript path (may not exist immediately after spawn)
    local TRANSCRIPT_PATH=""
    # Delay transcript lookup - it won't exist until Claude processes something
    # We'll update this later or let recovery command find it

    cat > "$METADATA_FILE" <<EOF
{
  "session": "$SESSION",
  "task": $(echo "$TASK" | jq -Rs .),
  "directory": "$WORK_DIR",
  "created": "$CREATED",
  "status": "running",
  "transcript_path": "$TRANSCRIPT_PATH",
  "tmux_pid": "$TMUX_PID",
  "with_handover": $WITH_HANDOVER,
  "with_worktree": $WITH_WORKTREE,
  "worktree_branch": "$WORKTREE_BRANCH",
  "orchestration": $ORCHESTRATION
}
EOF

    # Append spawn event to registry
    append_registry_event "spawn" "$SESSION" "\"task\":$(echo "$TASK" | jq -Rs .),\"worktree\":\"$WORK_DIR\""
}

# Function: Update agent metadata transcript path (call after Claude starts processing)
# Usage: update_agent_transcript <session> <work_dir>
update_agent_transcript() {
    local SESSION="$1"
    local WORK_DIR="$2"
    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"

    if [ ! -f "$METADATA_FILE" ]; then
        return 1
    fi

    local TRANSCRIPT_PATH
    TRANSCRIPT_PATH=$(find_transcript_path "$WORK_DIR")

    if [ -n "$TRANSCRIPT_PATH" ]; then
        # Update the metadata file with transcript path using jq
        local TEMP_FILE
        TEMP_FILE=$(mktemp)
        jq --arg tp "$TRANSCRIPT_PATH" '.transcript_path = $tp' "$METADATA_FILE" > "$TEMP_FILE" && \
            mv "$TEMP_FILE" "$METADATA_FILE"
    fi
}

# Function: Mark agent as completed
# Usage: mark_agent_complete <session> [status]
mark_agent_complete() {
    local SESSION="$1"
    local STATUS="${2:-success}"
    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"

    if [ -f "$METADATA_FILE" ]; then
        local TEMP_FILE
        TEMP_FILE=$(mktemp)
        jq --arg s "completed" --arg st "$STATUS" '.status = $s | .completion_status = $st | .completed_at = (now | todate)' \
            "$METADATA_FILE" > "$TEMP_FILE" && mv "$TEMP_FILE" "$METADATA_FILE"
    fi

    append_registry_event "complete" "$SESSION" "\"status\":\"$STATUS\""
}

# Function: Mark agent as orphaned
# Usage: mark_agent_orphaned <session> <reason>
mark_agent_orphaned() {
    local SESSION="$1"
    local REASON="$2"
    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"

    if [ -f "$METADATA_FILE" ]; then
        local TEMP_FILE
        TEMP_FILE=$(mktemp)
        jq --arg s "orphaned" --arg r "$REASON" '.status = $s | .orphan_reason = $r | .orphaned_at = (now | todate)' \
            "$METADATA_FILE" > "$TEMP_FILE" && mv "$TEMP_FILE" "$METADATA_FILE"
    fi

    append_registry_event "orphaned" "$SESSION" "\"reason\":\"$REASON\""
}

# Function: Wait for Claude Code to be ready for input
# Usage: wait_for_claude_ready <session_name>
# Returns: 0 on success, 1 on timeout
wait_for_claude_ready() {
    local SESSION=$1
    local TIMEOUT=${2:-30}  # Optional timeout parameter, default 30s
    local START=$(date +%s)

    while true; do
        # Capture pane output (suppress errors if session not ready)
        PANE_OUTPUT=$(tmux capture-pane -t "$SESSION" -p 2>/dev/null || echo "")

        # Check for Claude prompt/splash (any of these indicates readiness)
        if echo "$PANE_OUTPUT" | grep -qE "Claude Code|Welcome back|──────|Style:|bypass permissions"; then
            # Verify not in error state
            if ! echo "$PANE_OUTPUT" | grep -qiE "error|crash|failed|command not found"; then
                return 0
            fi
        fi

        # Timeout check
        local ELAPSED=$(($(date +%s) - START))
        if [ $ELAPSED -gt $TIMEOUT ]; then
            # Save debug output
            tmux capture-pane -t "$SESSION" -p > "/tmp/spawn-agent-${SESSION}-failure.log" 2>&1 || true
            return 1
        fi

        sleep 0.2
    done
}

# Function: Spawn a Claude agent in tmux
# Usage: spawn_agent_tmux <session_name> <work_dir> <task> [agent_type] [with_handover] [with_worktree] [worktree_branch] [orchestration]
# Returns: 0 on success, 1 on failure
spawn_agent_tmux() {
    local SESSION="$1"
    local WORK_DIR="$2"
    local TASK="$3"
    local AGENT_TYPE="${4:-general-purpose}"  # Optional agent type
    local WITH_HANDOVER="${5:-false}"
    local WITH_WORKTREE="${6:-false}"
    local WORKTREE_BRANCH="${7:-}"
    local ORCHESTRATION="${8:-null}"

    # Create tmux session
    tmux new-session -d -s "$SESSION" -c "$WORK_DIR" || return 1

    # Verify session creation
    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        return 1
    fi

    # Start Claude Code in the session (only if not already in a Claude session)
    if is_inside_claude_session; then
        echo "⚠️  Warning: Already inside Claude Code session, cannot spawn nested agent" >&2
        echo "   Unset CLAUDECODE env var to bypass this check" >&2
        tmux kill-session -t "$SESSION" 2>/dev/null || true
        return 1
    fi
    tmux send-keys -t "$SESSION" "claude --dangerously-skip-permissions" C-m

    # Wait for Claude to be ready
    if ! wait_for_claude_ready "$SESSION" 30; then
        # Cleanup on failure
        tmux kill-session -t "$SESSION" 2>/dev/null || true
        return 1
    fi

    # Additional small delay for UI stabilization
    sleep 0.5

    # Send the task (use literal mode for safety with special characters)
    tmux send-keys -t "$SESSION" -l "$TASK"
    tmux send-keys -t "$SESSION" C-m

    # Small delay for Claude to start processing
    sleep 1

    # Save enhanced metadata with transcript path support
    save_agent_metadata "$SESSION" "$TASK" "$WORK_DIR" "$WITH_HANDOVER" "$WITH_WORKTREE" "$WORKTREE_BRANCH" "$ORCHESTRATION"

    # Verify task was received
    local CURRENT_OUTPUT=$(tmux capture-pane -t "$SESSION" -p 2>/dev/null || echo "")
    if echo "$CURRENT_OUTPUT" | grep -qE "Thought for|Forming|Creating|Implement|⏳|✽|∴"; then
        # Task received and processing - try to update transcript path
        update_agent_transcript "$SESSION" "$WORK_DIR" || true
        return 0
    elif echo "$CURRENT_OUTPUT" | grep -qE "error|failed|crash"; then
        # Error detected
        return 1
    else
        # Unable to confirm but likely ok (agent may still be starting)
        return 0
    fi
}

# Function: Send additional message to running agent
# Usage: send_to_agent <session_name> <message>
send_to_agent() {
    local SESSION="$1"
    local MESSAGE="$2"

    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        return 1
    fi

    tmux send-keys -t "$SESSION" -l "$MESSAGE"
    tmux send-keys -t "$SESSION" C-m
    return 0
}

# Function: Get tmux pane output
# Usage: get_agent_output <session_name>
get_agent_output() {
    local SESSION="$1"

    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        return 1
    fi

    tmux capture-pane -t "$SESSION" -p 2>/dev/null || echo ""
}

# Function: Check if agent session is alive
# Usage: is_agent_alive <session_name>
is_agent_alive() {
    local SESSION="$1"
    tmux has-session -t "$SESSION" 2>/dev/null
}

# Function: Kill agent session
# Usage: kill_agent <session_name>
kill_agent() {
    local SESSION="$1"

    if tmux has-session -t "$SESSION" 2>/dev/null; then
        tmux kill-session -t "$SESSION" 2>/dev/null || true
    fi
}

# Function: List orphaned sessions (tmux dead, worktree/metadata exists)
# Usage: list_orphaned_sessions
# Returns: List of metadata file paths for orphaned sessions
list_orphaned_sessions() {
    ensure_agent_dirs

    for META_FILE in "$AGENTS_DIR"/*.json; do
        [ -f "$META_FILE" ] || continue
        [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

        local SESSION
        SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
        [ -z "$SESSION" ] && continue

        local STATUS
        STATUS=$(jq -r '.status // "unknown"' "$META_FILE" 2>/dev/null)

        # Skip already completed/archived sessions
        [ "$STATUS" = "completed" ] && continue
        [ "$STATUS" = "archived" ] && continue

        # Check if tmux session exists
        if ! tmux has-session -t "$SESSION" 2>/dev/null; then
            # Tmux dead - check if worktree exists OR we have a transcript
            local WORKTREE
            WORKTREE=$(jq -r '.directory // empty' "$META_FILE" 2>/dev/null)

            local TRANSCRIPT
            TRANSCRIPT=$(jq -r '.transcript_path // empty' "$META_FILE" 2>/dev/null)

            if [ -n "$WORKTREE" ] && [ -d "$WORKTREE" ]; then
                echo "$META_FILE"
            elif [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
                echo "$META_FILE"
            fi
        fi
    done
}

# Function: Get resumable sessions (have valid transcript)
# Usage: get_resumable_sessions
get_resumable_sessions() {
    for META_FILE in $(list_orphaned_sessions); do
        local WORK_DIR
        WORK_DIR=$(jq -r '.directory // empty' "$META_FILE" 2>/dev/null)

        # Try to find/update transcript path
        local TRANSCRIPT
        TRANSCRIPT=$(jq -r '.transcript_path // empty' "$META_FILE" 2>/dev/null)

        # If no transcript stored, try to find it
        if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
            TRANSCRIPT=$(find_transcript_path "$WORK_DIR")
        fi

        if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
            echo "$META_FILE"
        fi
    done
}

# Function: Archive completed/abandoned session
# Usage: archive_session <session_name>
archive_session() {
    local SESSION="$1"
    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"

    ensure_agent_dirs

    if [ -f "$METADATA_FILE" ]; then
        local ARCHIVED_FILE="${ARCHIVED_DIR}/${SESSION}.json"

        # Update status and move
        local TEMP_FILE
        TEMP_FILE=$(mktemp)
        jq '.status = "archived" | .archived_at = (now | todate)' "$METADATA_FILE" > "$TEMP_FILE" && \
            mv "$TEMP_FILE" "$ARCHIVED_FILE"
        rm -f "$METADATA_FILE"

        append_registry_event "archived" "$SESSION"
        return 0
    fi
    return 1
}

# Function: Get session info formatted for display
# Usage: get_session_info <metadata_file>
get_session_info() {
    local META_FILE="$1"

    if [ ! -f "$META_FILE" ]; then
        echo "Metadata file not found"
        return 1
    fi

    local SESSION TASK WORKTREE BRANCH CREATED STATUS
    SESSION=$(jq -r '.session // "unknown"' "$META_FILE")
    TASK=$(jq -r '.task // "unknown"' "$META_FILE" | head -c 60)
    WORKTREE=$(jq -r '.directory // "unknown"' "$META_FILE")
    BRANCH=$(jq -r '.worktree_branch // ""' "$META_FILE")
    CREATED=$(jq -r '.created // "unknown"' "$META_FILE")
    STATUS=$(jq -r '.status // "unknown"' "$META_FILE")

    echo "Session: $SESSION"
    echo "Status:  $STATUS"
    echo "Task:    $TASK..."
    echo "Dir:     $WORKTREE"
    [ -n "$BRANCH" ] && echo "Branch:  $BRANCH"
    echo "Created: $CREATED"
}

# Function: Resume orphaned session
# Usage: resume_session <session_name>
resume_session() {
    local SESSION="$1"
    local METADATA_FILE="${AGENTS_DIR}/${SESSION}.json"

    if [ ! -f "$METADATA_FILE" ]; then
        echo "❌ Metadata not found for session: $SESSION"
        return 1
    fi

    local WORK_DIR
    WORK_DIR=$(jq -r '.directory // empty' "$METADATA_FILE")

    if [ -z "$WORK_DIR" ] || [ ! -d "$WORK_DIR" ]; then
        echo "❌ Working directory not found: $WORK_DIR"
        return 1
    fi

    # Find transcript
    local TRANSCRIPT
    TRANSCRIPT=$(jq -r '.transcript_path // empty' "$METADATA_FILE")

    if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
        TRANSCRIPT=$(find_transcript_path "$WORK_DIR")
    fi

    if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
        echo "❌ No transcript found for session. Cannot resume."
        echo "   Worktree exists at: $WORK_DIR"
        echo "   You can manually start a new Claude session there."
        return 1
    fi

    # Generate new session name for resumed session
    local NEW_SESSION="${SESSION}-resumed-$(date +%s)"

    echo "🔄 Resuming session..."
    echo "   Original: $SESSION"
    echo "   New:      $NEW_SESSION"
    echo "   Directory: $WORK_DIR"
    echo "   Transcript: $TRANSCRIPT"

    # Create new tmux session
    tmux new-session -d -s "$NEW_SESSION" -c "$WORK_DIR" || {
        echo "❌ Failed to create tmux session"
        return 1
    }

    # Start Claude with --resume
    tmux send-keys -t "$NEW_SESSION" "claude --dangerously-skip-permissions --resume \"$TRANSCRIPT\"" C-m

    # Update metadata
    local TEMP_FILE
    TEMP_FILE=$(mktemp)
    jq --arg ns "$NEW_SESSION" '.status = "resumed" | .resumed_as = $ns | .resumed_at = (now | todate)' \
        "$METADATA_FILE" > "$TEMP_FILE" && mv "$TEMP_FILE" "$METADATA_FILE"

    append_registry_event "resumed" "$SESSION" "\"resumed_as\":\"$NEW_SESSION\""

    echo ""
    echo "✅ Session resumed!"
    echo "   Attach: tmux attach -t $NEW_SESSION"
}
