#!/usr/bin/env bash
#
# clone-external-repo.sh - Clone external repository for analysis
#
# Usage: clone_external_repo <repo_url> <target_dir> [shallow]
#

set -euo pipefail

# Clone external repository
# Usage: clone_external_repo <repo_url> <target_dir> [shallow]
clone_external_repo() {
    local repo_url="$1"
    local target_dir="$2"
    local shallow="${3:-true}"  # Default to shallow clone

    # Validate inputs
    if [ -z "$repo_url" ]; then
        echo "Error: Repository URL is required" >&2
        return 1
    fi

    if [ -z "$target_dir" ]; then
        echo "Error: Target directory is required" >&2
        return 1
    fi

    # Extract repo info for logging
    local repo_info
    repo_info=$(echo "$repo_url" | sed -E 's|https?://[^/]+/([^/]+)/([^/]+).*|\1/\2|')

    # Create target directory
    mkdir -p "$(dirname "$target_dir")"

    # Clone repository
    echo "📦 Cloning: $repo_info"

    if [ "$shallow" = "true" ]; then
        # Shallow clone (single commit, faster)
        if git clone --depth 1 --single-branch --quiet "$repo_url" "$target_dir" 2>/dev/null; then
            echo "✓ Cloned successfully (shallow)"
            return 0
        else
            echo "⚠ Shallow clone failed, trying full clone..." >&2
            # Fallback to full clone if shallow fails
            if git clone --quiet "$repo_url" "$target_dir" 2>/dev/null; then
                echo "✓ Cloned successfully (full)"
                return 0
            else
                echo "✗ Clone failed: $repo_url" >&2
                return 1
            fi
        fi
    else
        # Full clone
        if git clone --quiet "$repo_url" "$target_dir" 2>/dev/null; then
            echo "✓ Cloned successfully (full)"
            return 0
        else
            echo "✗ Clone failed: $repo_url" >&2
            return 1
        fi
    fi
}

# Check if repository already exists
# Usage: repo_exists <target_dir>
repo_exists() {
    local target_dir="$1"

    if [ -d "$target_dir/.git" ]; then
        return 0
    else
        return 1
    fi
}

# Get repository size (in KB)
# Usage: get_repo_size <target_dir>
get_repo_size() {
    local target_dir="$1"

    if [ -d "$target_dir" ]; then
        du -sk "$target_dir" | cut -f1
    else
        echo "0"
    fi
}

# Get repository commit hash
# Usage: get_repo_commit <target_dir>
get_repo_commit() {
    local target_dir="$1"

    if [ -d "$target_dir/.git" ]; then
        git -C "$target_dir" rev-parse HEAD 2>/dev/null || echo "unknown"
    else
        echo "unknown"
    fi
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 2 ]]; then
        echo "Usage: $0 <repo_url> <target_dir> [shallow]" >&2
        echo "  repo_url:    Repository URL (GitHub, GitLab, Bitbucket)" >&2
        echo "  target_dir:  Target directory for clone" >&2
        echo "  shallow:     'true' for shallow clone, 'false' for full (default: true)" >&2
        exit 1
    fi

    clone_external_repo "$@"
fi
