#!/bin/bash

# ABOUTME: tmux session monitoring script - discovers, categorizes, and reports status of all active tmux sessions

set -euo pipefail

# Output mode: compact (default), detailed, json
OUTPUT_MODE="${1:-compact}"

# Check if tmux is available
if ! command -v tmux &> /dev/null; then
    echo "❌ tmux is not installed"
    exit 1
fi

# Check if there are any sessions
if ! tmux list-sessions 2>/dev/null | grep -q .; then
    if [ "$OUTPUT_MODE" = "json" ]; then
        echo '{"sessions": [], "summary": {"total_sessions": 0, "total_windows": 0, "total_panes": 0}}'
    else
        echo "✅ No tmux sessions currently running"
    fi
    exit 0
fi

# Initialize counters
TOTAL_SESSIONS=0
TOTAL_WINDOWS=0
TOTAL_PANES=0

# Arrays to store sessions by category
declare -a DEV_SESSIONS
declare -a AGENT_SESSIONS
declare -a MONITOR_SESSIONS
declare -a CLAUDE_SESSIONS
declare -a OTHER_SESSIONS

# Get all sessions
SESSIONS=$(tmux list-sessions -F '#{session_name}|#{session_windows}|#{session_created}|#{session_attached}' 2>/dev/null)

# Parse and categorize sessions
while IFS='|' read -r SESSION_NAME WINDOW_COUNT CREATED ATTACHED; do
    TOTAL_SESSIONS=$((TOTAL_SESSIONS + 1))
    TOTAL_WINDOWS=$((TOTAL_WINDOWS + WINDOW_COUNT))

    # Get pane count for this session
    PANE_COUNT=$(tmux list-panes -t "$SESSION_NAME" 2>/dev/null | wc -l | tr -d ' ')
    TOTAL_PANES=$((TOTAL_PANES + PANE_COUNT))

    # Categorize by prefix
    if [[ "$SESSION_NAME" == dev-* ]]; then
        DEV_SESSIONS+=("$SESSION_NAME|$WINDOW_COUNT|$PANE_COUNT|$ATTACHED")
    elif [[ "$SESSION_NAME" == agent-* ]]; then
        AGENT_SESSIONS+=("$SESSION_NAME|$WINDOW_COUNT|$PANE_COUNT|$ATTACHED")
    elif [[ "$SESSION_NAME" == monitor-* ]]; then
        MONITOR_SESSIONS+=("$SESSION_NAME|$WINDOW_COUNT|$PANE_COUNT|$ATTACHED")
    elif [[ "$SESSION_NAME" == claude-* ]] || [[ "$SESSION_NAME" == *claude* ]]; then
        CLAUDE_SESSIONS+=("$SESSION_NAME|$WINDOW_COUNT|$PANE_COUNT|$ATTACHED")
    else
        OTHER_SESSIONS+=("$SESSION_NAME|$WINDOW_COUNT|$PANE_COUNT|$ATTACHED")
    fi
done <<< "$SESSIONS"

# Helper function to get session metadata
get_dev_metadata() {
    local SESSION_NAME=$1
    local METADATA_FILE=".tmux-dev-session.json"

    if [ -f "$METADATA_FILE" ]; then
        local SESSION_IN_FILE=$(jq -r '.session // empty' "$METADATA_FILE" 2>/dev/null)
        if [ "$SESSION_IN_FILE" = "$SESSION_NAME" ]; then
            echo "$METADATA_FILE"
        fi
    fi

    # Try iOS-specific metadata
    if [ -f ".tmux-ios-session.json" ]; then
        local SESSION_IN_FILE=$(jq -r '.session // empty' ".tmux-ios-session.json" 2>/dev/null)
        if [ "$SESSION_IN_FILE" = "$SESSION_NAME" ]; then
            echo ".tmux-ios-session.json"
        fi
    fi

    # Try Android-specific metadata
    if [ -f ".tmux-android-session.json" ]; then
        local SESSION_IN_FILE=$(jq -r '.session // empty' ".tmux-android-session.json" 2>/dev/null)
        if [ "$SESSION_IN_FILE" = "$SESSION_NAME" ]; then
            echo ".tmux-android-session.json"
        fi
    fi
}

get_agent_metadata() {
    local SESSION_NAME=$1
    # Runtime-aware metadata path — TOOLKIT_HOME is set by bootstrap's agent-env.sh
    # Falls back to $HOME/{{TOOL_DIR}} for backward compatibility
    local TOOLKIT_HOME="${TOOLKIT_HOME:-$HOME/{{TOOL_DIR}}}"
    local METADATA_FILE="$TOOLKIT_HOME/agents/${SESSION_NAME}.json"

    if [ -f "$METADATA_FILE" ]; then
        echo "$METADATA_FILE"
    fi
}

# Get running ports
get_running_ports() {
    if command -v lsof &> /dev/null; then
        lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | grep -E "node|python|uv|npm|ruby|java" | awk '{print $9}' | cut -d':' -f2 | sort -u || true
    fi
}

RUNNING_PORTS=$(get_running_ports)

# Output functions

output_compact() {
    echo "${TOTAL_SESSIONS} active sessions:"

    # Dev environments
    if [[ -v DEV_SESSIONS[@] ]] && [ ${#DEV_SESSIONS[@]} -gt 0 ]; then
        for session_data in "${DEV_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"
            STATUS="detached"
            [ "$ATTACHED" = "1" ] && STATUS="active"

            # Try to get metadata
            METADATA_FILE=$(get_dev_metadata "$SESSION_NAME")
            if [ -n "$METADATA_FILE" ]; then
                PROJECT_TYPE=$(jq -r '.type // "dev"' "$METADATA_FILE" 2>/dev/null)
                echo "- $SESSION_NAME ($PROJECT_TYPE, $WINDOW_COUNT windows, $STATUS)"
            else
                echo "- $SESSION_NAME ($WINDOW_COUNT windows, $STATUS)"
            fi
        done
    fi

    # Agent sessions
    if [[ -v AGENT_SESSIONS[@] ]] && [ ${#AGENT_SESSIONS[@]} -gt 0 ]; then
        for session_data in "${AGENT_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"

            # Try to get agent metadata
            METADATA_FILE=$(get_agent_metadata "$SESSION_NAME")
            if [ -n "$METADATA_FILE" ]; then
                AGENT_TYPE=$(jq -r '.agent_type // "unknown"' "$METADATA_FILE" 2>/dev/null)
                STATUS_=$(jq -r '.status // "running"' "$METADATA_FILE" 2>/dev/null)
                echo "- $SESSION_NAME ($AGENT_TYPE, $STATUS_)"
            else
                echo "- $SESSION_NAME (agent)"
            fi
        done
    fi

    # Claude sessions
    if [[ -v CLAUDE_SESSIONS[@] ]] && [ ${#CLAUDE_SESSIONS[@]} -gt 0 ]; then
        for session_data in "${CLAUDE_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"
            STATUS="detached"
            [ "$ATTACHED" = "1" ] && STATUS="current"
            echo "- $SESSION_NAME (main session, $STATUS)"
        done
    fi

    # Other sessions
    if [[ -v OTHER_SESSIONS[@] ]] && [ ${#OTHER_SESSIONS[@]} -gt 0 ]; then
        for session_data in "${OTHER_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"
            echo "- $SESSION_NAME ($WINDOW_COUNT windows)"
        done
    fi

    # Port summary
    if [ -n "$RUNNING_PORTS" ]; then
        PORT_COUNT=$(echo "$RUNNING_PORTS" | wc -l | tr -d ' ')
        echo ""
        echo "$PORT_COUNT running servers on ports: $(echo $RUNNING_PORTS | tr '\n' ',' | sed 's/,$//')"
    fi

    echo ""
    echo "Use /tmux-status --detailed for full report"
}

output_detailed() {
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "📊 tmux Sessions Overview"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "**Total Active Sessions**: $TOTAL_SESSIONS"
    echo "**Total Windows**: $TOTAL_WINDOWS"
    echo "**Total Panes**: $TOTAL_PANES"
    echo ""

    # Dev environments
    if [[ -v DEV_SESSIONS[@] ]] && [ ${#DEV_SESSIONS[@]} -gt 0 ]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "## Development Environments (${#DEV_SESSIONS[@]})"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""

        local INDEX=1
        for session_data in "${DEV_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"
            STATUS="🔌 Detached"
            [ "$ATTACHED" = "1" ] && STATUS="⚡ Active (attached)"

            echo "### $INDEX. $SESSION_NAME"
            echo "- **Status**: $STATUS"
            echo "- **Windows**: $WINDOW_COUNT"
            echo "- **Panes**: $PANE_COUNT"

            # Get metadata if available
            METADATA_FILE=$(get_dev_metadata "$SESSION_NAME")
            if [ -n "$METADATA_FILE" ]; then
                PROJECT=$(jq -r '.project // "unknown"' "$METADATA_FILE" 2>/dev/null)
                PROJECT_TYPE=$(jq -r '.type // "unknown"' "$METADATA_FILE" 2>/dev/null)
                CREATED=$(jq -r '.created // "unknown"' "$METADATA_FILE" 2>/dev/null)

                echo "- **Project**: $PROJECT ($PROJECT_TYPE)"
                echo "- **Created**: $CREATED"

                # Check for ports
                if jq -e '.dev_port' "$METADATA_FILE" &>/dev/null; then
                    DEV_PORT=$(jq -r '.dev_port' "$METADATA_FILE" 2>/dev/null)
                    echo "- **Dev Server**: http://localhost:$DEV_PORT"
                fi

                if jq -e '.services' "$METADATA_FILE" &>/dev/null; then
                    echo "- **Services**: $(jq -r '.services | keys | join(", ")' "$METADATA_FILE" 2>/dev/null)"
                fi
            fi

            echo "- **Attach**: \`tmux attach -t $SESSION_NAME\`"
            echo ""

            INDEX=$((INDEX + 1))
        done
    fi

    # Agent sessions
    if [[ -v AGENT_SESSIONS[@] ]] && [ ${#AGENT_SESSIONS[@]} -gt 0 ]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "## Spawned Agents (${#AGENT_SESSIONS[@]})"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""

        local INDEX=1
        for session_data in "${AGENT_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"

            echo "### $INDEX. $SESSION_NAME"

            # Get agent metadata
            METADATA_FILE=$(get_agent_metadata "$SESSION_NAME")
            if [ -n "$METADATA_FILE" ]; then
                AGENT_TYPE=$(jq -r '.agent_type // "unknown"' "$METADATA_FILE" 2>/dev/null)
                TASK=$(jq -r '.task // "unknown"' "$METADATA_FILE" 2>/dev/null)
                STATUS_=$(jq -r '.status // "running"' "$METADATA_FILE" 2>/dev/null)
                DIRECTORY=$(jq -r '.directory // "unknown"' "$METADATA_FILE" 2>/dev/null)
                CREATED=$(jq -r '.created // "unknown"' "$METADATA_FILE" 2>/dev/null)

                echo "- **Agent Type**: $AGENT_TYPE"
                echo "- **Task**: $TASK"
                echo "- **Status**: $([ "$STATUS_" = "completed" ] && echo "✅ Completed" || echo "⚙️  Running")"
                echo "- **Working Directory**: $DIRECTORY"
                echo "- **Created**: $CREATED"

                # Check for worktree
                if jq -e '.worktree' "$METADATA_FILE" &>/dev/null; then
                    WORKTREE=$(jq -r '.worktree' "$METADATA_FILE" 2>/dev/null)
                    if [ "$WORKTREE" = "true" ]; then
                        AGENT_BRANCH=$(jq -r '.agent_branch // "unknown"' "$METADATA_FILE" 2>/dev/null)
                        echo "- **Git Worktree**: Yes (branch: $AGENT_BRANCH)"
                    fi
                fi
            fi

            echo "- **Attach**: \`tmux attach -t $SESSION_NAME\`"
            echo "- **Metadata**: \`cat $METADATA_FILE\`"
            echo ""

            INDEX=$((INDEX + 1))
        done
    fi

    # Claude sessions
    if [[ -v CLAUDE_SESSIONS[@] ]] && [ ${#CLAUDE_SESSIONS[@]} -gt 0 ]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "## Other Sessions (${#CLAUDE_SESSIONS[@]})"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""

        for session_data in "${CLAUDE_SESSIONS[@]}"; do
            IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"
            STATUS="Detached"
            [ "$ATTACHED" = "1" ] && STATUS="⚡ Active (current session)"
            echo "- $SESSION_NAME: $STATUS"
        done
        echo ""
    fi

    # Running processes summary
    if [ -n "$RUNNING_PORTS" ]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "## Running Processes Summary"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        echo "| Port | Service | Status |"
        echo "|------|---------|--------|"
        for PORT in $RUNNING_PORTS; do
            echo "| $PORT | Running | ✅ |"
        done
        echo ""
    fi

    # Quick actions
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "## Quick Actions"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "**List all sessions**:"
    echo "\`\`\`bash"
    echo "tmux ls"
    echo "\`\`\`"
    echo ""
    echo "**Attach to session**:"
    echo "\`\`\`bash"
    echo "tmux attach -t <session-name>"
    echo "\`\`\`"
    echo ""
    echo "**Kill session**:"
    echo "\`\`\`bash"
    echo "tmux kill-session -t <session-name>"
    echo "\`\`\`"
    echo ""
}

output_json() {
    echo "{"
    echo "  \"sessions\": ["

    local FIRST_SESSION=true

    # Dev sessions
    for session_data in "${DEV_SESSIONS[@]}"; do
        [ "$FIRST_SESSION" = false ] && echo ","
        FIRST_SESSION=false

        IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"

        echo "    {"
        echo "      \"name\": \"$SESSION_NAME\","
        echo "      \"type\": \"dev-environment\","
        echo "      \"windows\": $WINDOW_COUNT,"
        echo "      \"panes\": $PANE_COUNT,"
        echo "      \"attached\": $([ "$ATTACHED" = "1" ] && echo "true" || echo "false")"

        # Get metadata if available
        METADATA_FILE=$(get_dev_metadata "$SESSION_NAME")
        if [ -n "$METADATA_FILE" ]; then
            echo "      ,\"metadata_file\": \"$METADATA_FILE\""
        fi

        echo -n "    }"
    done

    # Agent sessions
    for session_data in "${AGENT_SESSIONS[@]}"; do
        [ "$FIRST_SESSION" = false ] && echo ","
        FIRST_SESSION=false

        IFS='|' read -r SESSION_NAME WINDOW_COUNT PANE_COUNT ATTACHED <<< "$session_data"

        echo "    {"
        echo "      \"name\": \"$SESSION_NAME\","
        echo "      \"type\": \"spawned-agent\","
        echo "      \"windows\": $WINDOW_COUNT,"
        echo "      \"panes\": $PANE_COUNT"

        # Get agent metadata
        METADATA_FILE=$(get_agent_metadata "$SESSION_NAME")
        if [ -n "$METADATA_FILE" ]; then
            AGENT_TYPE=$(jq -r '.agent_type // "unknown"' "$METADATA_FILE" 2>/dev/null)
            STATUS_=$(jq -r '.status // "running"' "$METADATA_FILE" 2>/dev/null)
            echo "      ,\"agent_type\": \"$AGENT_TYPE\","
            echo "      \"status\": \"$STATUS_\","
            echo "      \"metadata_file\": \"$METADATA_FILE\""
        fi

        echo -n "    }"
    done

    echo ""
    echo "  ],"
    echo "  \"summary\": {"
    echo "    \"total_sessions\": $TOTAL_SESSIONS,"
    echo "    \"total_windows\": $TOTAL_WINDOWS,"
    echo "    \"total_panes\": $TOTAL_PANES,"
    echo "    \"dev_sessions\": ${#DEV_SESSIONS[@]},"
    echo "    \"agent_sessions\": ${#AGENT_SESSIONS[@]}"
    echo "  }"
    echo "}"
}

# Main output
case "$OUTPUT_MODE" in
    compact)
        output_compact
        ;;
    detailed)
        output_detailed
        ;;
    json)
        output_json
        ;;
    *)
        echo "Unknown output mode: $OUTPUT_MODE"
        echo "Usage: monitor.sh [compact|detailed|json]"
        exit 1
        ;;
esac
