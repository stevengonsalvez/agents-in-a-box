#!/bin/bash

# Session Registry Utility
# Manages the append-only registry of agent session events
# Provides orphan detection, session recovery, and cleanup utilities

set -euo pipefail

# Source spawn-agent-lib for core functions
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/spawn-agent-lib.sh"

# Constants (re-exported for standalone use)
AGENTS_DIR="${HOME}/.claude/agents"
REGISTRY_FILE="${AGENTS_DIR}/registry.jsonl"
ARCHIVED_DIR="${AGENTS_DIR}/archived"

# Function: Get registry statistics
# Usage: registry_stats
registry_stats() {
    ensure_agent_dirs

    local SPAWN_COUNT=0
    local COMPLETE_COUNT=0
    local ORPHAN_COUNT=0
    local ARCHIVED_COUNT=0
    local RESUMED_COUNT=0

    if [ -f "$REGISTRY_FILE" ]; then
        SPAWN_COUNT=$(grep -c '"event":"spawn"' "$REGISTRY_FILE" 2>/dev/null || echo 0)
        COMPLETE_COUNT=$(grep -c '"event":"complete"' "$REGISTRY_FILE" 2>/dev/null || echo 0)
        ORPHAN_COUNT=$(grep -c '"event":"orphaned"' "$REGISTRY_FILE" 2>/dev/null || echo 0)
        ARCHIVED_COUNT=$(grep -c '"event":"archived"' "$REGISTRY_FILE" 2>/dev/null || echo 0)
        RESUMED_COUNT=$(grep -c '"event":"resumed"' "$REGISTRY_FILE" 2>/dev/null || echo 0)
    fi

    local ACTIVE_METADATA
    ACTIVE_METADATA=$(find "$AGENTS_DIR" -maxdepth 1 -name "*.json" -type f 2>/dev/null | wc -l | tr -d ' ')

    local ARCHIVED_METADATA
    ARCHIVED_METADATA=$(find "$ARCHIVED_DIR" -maxdepth 1 -name "*.json" -type f 2>/dev/null | wc -l | tr -d ' ')

    local LIVE_TMUX=0
    for META_FILE in "$AGENTS_DIR"/*.json; do
        [ -f "$META_FILE" ] || continue
        local SESSION
        SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
        if [ -n "$SESSION" ] && tmux has-session -t "$SESSION" 2>/dev/null; then
            ((LIVE_TMUX++)) || true
        fi
    done

    echo "═══════════════════════════════════════════════════════════════"
    echo "  Agent Session Registry Statistics"
    echo "═══════════════════════════════════════════════════════════════"
    echo ""
    echo "Registry Events:"
    echo "  Spawned:   $SPAWN_COUNT"
    echo "  Completed: $COMPLETE_COUNT"
    echo "  Orphaned:  $ORPHAN_COUNT"
    echo "  Archived:  $ARCHIVED_COUNT"
    echo "  Resumed:   $RESUMED_COUNT"
    echo ""
    echo "Current State:"
    echo "  Active metadata files: $ACTIVE_METADATA"
    echo "  Archived metadata:     $ARCHIVED_METADATA"
    echo "  Live tmux sessions:    $LIVE_TMUX"
    echo ""
    echo "═══════════════════════════════════════════════════════════════"
}

# Function: List all sessions with status
# Usage: list_sessions [--all | --active | --orphaned]
list_sessions() {
    local FILTER="${1:---active}"

    ensure_agent_dirs

    echo "═══════════════════════════════════════════════════════════════"
    echo "  Agent Sessions ($FILTER)"
    echo "═══════════════════════════════════════════════════════════════"

    local COUNT=0

    for META_FILE in "$AGENTS_DIR"/*.json; do
        [ -f "$META_FILE" ] || continue
        [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

        local SESSION STATUS TASK CREATED TMUX_ALIVE
        SESSION=$(jq -r '.session // "unknown"' "$META_FILE" 2>/dev/null)
        STATUS=$(jq -r '.status // "unknown"' "$META_FILE" 2>/dev/null)
        TASK=$(jq -r '.task // "unknown"' "$META_FILE" 2>/dev/null | head -c 50 | tr '\n' ' ')
        CREATED=$(jq -r '.created // "unknown"' "$META_FILE" 2>/dev/null)

        # Check tmux status
        if tmux has-session -t "$SESSION" 2>/dev/null; then
            TMUX_ALIVE="LIVE"
        else
            TMUX_ALIVE="DEAD"
        fi

        # Apply filter
        case "$FILTER" in
            --active)
                [ "$TMUX_ALIVE" = "DEAD" ] && continue
                ;;
            --orphaned)
                [ "$TMUX_ALIVE" = "LIVE" ] && continue
                [ "$STATUS" = "completed" ] && continue
                [ "$STATUS" = "archived" ] && continue
                ;;
            --all)
                # Show everything
                ;;
        esac

        ((COUNT++)) || true

        echo ""
        echo "  [$TMUX_ALIVE] $SESSION"
        echo "  Status: $STATUS"
        echo "  Task:   ${TASK}..."
        echo "  Created: $CREATED"
    done

    if [ $COUNT -eq 0 ]; then
        echo ""
        echo "  No sessions found matching filter: $FILTER"
    fi

    echo ""
    echo "═══════════════════════════════════════════════════════════════"
}

# Function: Cleanup orphaned sessions (mark as orphaned + optionally archive)
# Usage: cleanup_orphaned [--archive]
cleanup_orphaned() {
    local DO_ARCHIVE="${1:-}"

    ensure_agent_dirs

    local ORPHAN_COUNT=0

    for META_FILE in "$AGENTS_DIR"/*.json; do
        [ -f "$META_FILE" ] || continue
        [ "$(basename "$META_FILE")" = "registry.jsonl" ] && continue

        local SESSION STATUS
        SESSION=$(jq -r '.session // empty' "$META_FILE" 2>/dev/null)
        STATUS=$(jq -r '.status // "unknown"' "$META_FILE" 2>/dev/null)

        [ -z "$SESSION" ] && continue
        [ "$STATUS" = "completed" ] && continue
        [ "$STATUS" = "archived" ] && continue
        [ "$STATUS" = "orphaned" ] && continue

        # Check if tmux session is dead
        if ! tmux has-session -t "$SESSION" 2>/dev/null; then
            echo "Marking orphaned: $SESSION"
            mark_agent_orphaned "$SESSION" "tmux_session_dead"
            ((ORPHAN_COUNT++)) || true

            if [ "$DO_ARCHIVE" = "--archive" ]; then
                echo "  -> Archiving: $SESSION"
                archive_session "$SESSION"
            fi
        fi
    done

    echo ""
    echo "Marked $ORPHAN_COUNT sessions as orphaned"
}

# Function: Search registry for session events
# Usage: search_registry <session_pattern>
search_registry() {
    local PATTERN="${1:-}"

    if [ -z "$PATTERN" ]; then
        echo "Usage: search_registry <session_pattern>"
        return 1
    fi

    if [ ! -f "$REGISTRY_FILE" ]; then
        echo "Registry file not found"
        return 1
    fi

    echo "Searching registry for: $PATTERN"
    echo "───────────────────────────────────────────────────────────────"
    grep "$PATTERN" "$REGISTRY_FILE" | jq -s '.' 2>/dev/null || grep "$PATTERN" "$REGISTRY_FILE"
}

# Function: Tail registry (watch for new events)
# Usage: tail_registry [count]
tail_registry() {
    local COUNT="${1:-20}"

    if [ ! -f "$REGISTRY_FILE" ]; then
        echo "Registry file not found"
        return 1
    fi

    echo "Last $COUNT registry events:"
    echo "───────────────────────────────────────────────────────────────"
    tail -n "$COUNT" "$REGISTRY_FILE" | while read -r line; do
        echo "$line" | jq -c '.'
    done
}

# Main: Run as command if executed directly
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    CMD="${1:-help}"
    shift || true

    case "$CMD" in
        stats)
            registry_stats
            ;;
        list)
            list_sessions "$@"
            ;;
        orphaned)
            list_sessions --orphaned
            ;;
        cleanup)
            cleanup_orphaned "$@"
            ;;
        search)
            search_registry "$@"
            ;;
        tail)
            tail_registry "$@"
            ;;
        help|*)
            echo "Session Registry Utility"
            echo ""
            echo "Usage: session-registry.sh <command> [options]"
            echo ""
            echo "Commands:"
            echo "  stats            Show registry statistics"
            echo "  list [filter]    List sessions (--all, --active, --orphaned)"
            echo "  orphaned         Shortcut for 'list --orphaned'"
            echo "  cleanup [--archive]  Mark dead sessions as orphaned"
            echo "  search <pattern> Search registry for session events"
            echo "  tail [count]     Show recent registry events"
            echo "  help             Show this help"
            ;;
    esac
fi
