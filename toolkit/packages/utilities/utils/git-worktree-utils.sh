#!/bin/bash

# ABOUTME: Git worktree utilities for agent workspace isolation

set -euo pipefail

# Create agent worktree with isolated branch
create_agent_worktree() {
    local AGENT_ID=$1
    local BASE_BRANCH=${2:-$(git branch --show-current)}
    local TASK_SLUG=${3:-""}

    # Build directory name with optional task slug
    if [ -n "$TASK_SLUG" ]; then
        local WORKTREE_DIR="worktrees/agent-${AGENT_ID}-${TASK_SLUG}"
    else
        local WORKTREE_DIR="worktrees/agent-${AGENT_ID}"
    fi

    local BRANCH_NAME="agent/agent-${AGENT_ID}"

    # Create worktrees directory if needed
    mkdir -p worktrees

    # Create worktree with new branch (redirect git output to stderr)
    git worktree add -b "$BRANCH_NAME" "$WORKTREE_DIR" "$BASE_BRANCH" >&2

    # Echo only the directory path to stdout
    echo "$WORKTREE_DIR"
}

# Remove agent worktree
cleanup_agent_worktree() {
    local AGENT_ID=$1
    local FORCE=${2:-false}

    # Find worktree directory (may have task slug suffix)
    local WORKTREE_DIR=$(find worktrees -type d -name "agent-${AGENT_ID}*" 2>/dev/null | head -1)
    local BRANCH_NAME="agent/agent-${AGENT_ID}"

    if [ -z "$WORKTREE_DIR" ] || [ ! -d "$WORKTREE_DIR" ]; then
        echo "❌ Worktree not found for agent: $AGENT_ID"
        return 1
    fi

    # Check for uncommitted changes
    if ! git -C "$WORKTREE_DIR" diff --quiet 2>/dev/null; then
        if [ "$FORCE" = false ]; then
            echo "⚠️  Worktree has uncommitted changes. Use --force to remove anyway."
            return 1
        fi
    fi

    # Remove worktree
    git worktree remove "$WORKTREE_DIR" $( [ "$FORCE" = true ] && echo "--force" )

    # Delete branch (only if merged or forced)
    git branch -d "$BRANCH_NAME" 2>/dev/null || \
        ( [ "$FORCE" = true ] && git branch -D "$BRANCH_NAME" )
}

# List all agent worktrees
list_agent_worktrees() {
    git worktree list | grep "worktrees/agent-" || echo "No agent worktrees found"
}

# Merge agent work into current branch
merge_agent_work() {
    local AGENT_ID=$1
    local BRANCH_NAME="agent/agent-${AGENT_ID}"

    if ! git show-ref --verify --quiet "refs/heads/$BRANCH_NAME"; then
        echo "❌ Branch not found: $BRANCH_NAME"
        return 1
    fi

    git merge "$BRANCH_NAME"
}

# Check if worktree exists
worktree_exists() {
    local AGENT_ID=$1
    local WORKTREE_DIR=$(find worktrees -type d -name "agent-${AGENT_ID}*" 2>/dev/null | head -1)

    [ -n "$WORKTREE_DIR" ] && [ -d "$WORKTREE_DIR" ]
}

# Main CLI (only run if executed directly, not sourced)
if [ "${BASH_SOURCE[0]:-}" = "${0:-}" ]; then
    case "${1:-help}" in
        create)
            create_agent_worktree "$2" "${3:-}" "${4:-}"
            ;;
        cleanup)
            cleanup_agent_worktree "$2" "${3:-false}"
            ;;
        list)
            list_agent_worktrees
            ;;
        merge)
            merge_agent_work "$2"
            ;;
        exists)
            worktree_exists "$2"
            ;;
        *)
            echo "Usage: git-worktree-utils.sh {create|cleanup|list|merge|exists} [args]"
            exit 1
            ;;
    esac
fi
