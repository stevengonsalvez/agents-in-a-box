#!/usr/bin/env bash
#
# parallel-repo-clone.sh - Clone multiple repositories in parallel
#
# Usage: parallel_repo_clone <repo_urls_file> <base_target_dir> [max_parallel]
#

set -euo pipefail

# Source clone utility
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/clone-external-repo.sh"

# Clone multiple repositories in parallel
# Usage: parallel_repo_clone <repo_urls_file> <base_target_dir> [max_parallel]
parallel_repo_clone() {
    local repo_urls_file="$1"
    local base_target_dir="$2"
    local max_parallel="${3:-4}"  # Default to 4 parallel clones

    # Validate inputs
    if [ ! -f "$repo_urls_file" ]; then
        echo "Error: Repository URLs file not found: $repo_urls_file" >&2
        return 1
    fi

    if [ -z "$base_target_dir" ]; then
        echo "Error: Base target directory is required" >&2
        return 1
    fi

    # Create base target directory
    mkdir -p "$base_target_dir"

    # Count total repositories
    local total_repos
    total_repos=$(grep -c . "$repo_urls_file" || echo "0")

    if [ "$total_repos" -eq 0 ]; then
        echo "No repositories to clone" >&2
        return 0
    fi

    echo "📦 Cloning $total_repos repositories (max $max_parallel parallel)..."

    # Process repositories in parallel using background jobs
    local clone_pids=()
    local clone_results=()
    local current_jobs=0
    local completed=0
    local failed=0

    while IFS= read -r repo_url || [ -n "$repo_url" ]; do
        # Skip empty lines
        [ -z "$repo_url" ] && continue

        # Extract owner-repo for unique directory name
        local repo_name
        repo_name=$(echo "$repo_url" | sed -E 's|https?://[^/]+/([^/]+)/([^/]+).*|\1-\2|' | sed 's/\.git$//')

        # Target directory for this repository
        local target_dir="$base_target_dir/$repo_name"

        # Check if already exists
        if repo_exists "$target_dir"; then
            echo "⏭ Skipping (already exists): $repo_name"
            ((completed++))
            continue
        fi

        # Wait if at max parallel limit
        while [ "$current_jobs" -ge "$max_parallel" ]; do
            # Wait for any job to finish
            wait -n 2>/dev/null || true
            ((current_jobs--))
        done

        # Clone in background
        (
            if clone_external_repo "$repo_url" "$target_dir" "true"; then
                echo "success:$repo_url:$target_dir" > "$base_target_dir/.clone_result_$$_$RANDOM"
            else
                echo "failed:$repo_url:$target_dir" > "$base_target_dir/.clone_result_$$_$RANDOM"
            fi
        ) &

        clone_pids+=($!)
        ((current_jobs++))

    done < "$repo_urls_file"

    # Wait for all remaining jobs
    for pid in "${clone_pids[@]}"; do
        wait "$pid" 2>/dev/null || true
    done

    # Collect results
    for result_file in "$base_target_dir"/.clone_result_*; do
        if [ -f "$result_file" ]; then
            local result
            result=$(cat "$result_file")
            local status
            status=$(echo "$result" | cut -d: -f1)

            if [ "$status" = "success" ]; then
                ((completed++))
            else
                ((failed++))
            fi

            rm -f "$result_file"
        fi
    done

    # Summary
    echo ""
    echo "📊 Clone Summary:"
    echo "  Total:     $total_repos"
    echo "  Completed: $completed"
    echo "  Failed:    $failed"

    if [ "$failed" -gt 0 ]; then
        return 1
    else
        return 0
    fi
}

# Generate clone manifest (list of cloned repos with metadata)
# Usage: generate_clone_manifest <base_target_dir> <output_file>
generate_clone_manifest() {
    local base_target_dir="$1"
    local output_file="$2"

    echo "# Repository Clone Manifest" > "$output_file"
    echo "# Generated: $(date -Iseconds)" >> "$output_file"
    echo "" >> "$output_file"

    for repo_dir in "$base_target_dir"/*; do
        if [ -d "$repo_dir/.git" ]; then
            local repo_name
            repo_name=$(basename "$repo_dir")

            local commit_hash
            commit_hash=$(get_repo_commit "$repo_dir")

            local repo_size
            repo_size=$(get_repo_size "$repo_dir")

            local remote_url
            remote_url=$(git -C "$repo_dir" config --get remote.origin.url 2>/dev/null || echo "unknown")

            echo "Repository: $repo_name" >> "$output_file"
            echo "  URL: $remote_url" >> "$output_file"
            echo "  Path: $repo_dir" >> "$output_file"
            echo "  Commit: $commit_hash" >> "$output_file"
            echo "  Size: ${repo_size}KB" >> "$output_file"
            echo "" >> "$output_file"
        fi
    done

    echo "✓ Manifest saved: $output_file"
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 2 ]]; then
        echo "Usage: $0 <repo_urls_file> <base_target_dir> [max_parallel]" >&2
        echo "  repo_urls_file:   File containing repository URLs (one per line)" >&2
        echo "  base_target_dir:  Base directory for cloned repositories" >&2
        echo "  max_parallel:     Maximum parallel clones (default: 4)" >&2
        exit 1
    fi

    parallel_repo_clone "$@"

    # Generate manifest
    if [ -d "$2" ]; then
        generate_clone_manifest "$2" "$2/CLONE_MANIFEST.txt"
    fi
fi
