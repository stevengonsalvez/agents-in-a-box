#!/usr/bin/env bash
#
# repo-analysis-cache.sh - Manage global repository analysis cache
#
# Cache Structure:
#   ~/.claude/research-cache/
#     <owner>-<repo>-<commit-short>/
#       analysis.md          - Main analysis document
#       metadata.json        - Cache metadata (timestamp, query hash, etc.)
#       cloned/              - Shallow clone of repository (optional, for reference)
#

set -euo pipefail

# Global cache directory
CACHE_DIR="${CLAUDE_RESEARCH_CACHE:-$HOME/.claude/research-cache}"

# Cache TTL in seconds (7 days)
CACHE_TTL=$((7 * 24 * 60 * 60))

# Max age before purge (30 days)
MAX_CACHE_AGE=$((30 * 24 * 60 * 60))

# Initialize cache directory
init_cache() {
    mkdir -p "$CACHE_DIR"
    echo "✓ Cache initialized: $CACHE_DIR"
}

# Generate cache key for repository
# Usage: get_cache_key <repo_url> <commit_hash>
get_cache_key() {
    local repo_url="$1"
    local commit_hash="$2"

    # Extract owner/repo from URL
    local owner_repo
    owner_repo=$(echo "$repo_url" | sed -E 's|https?://[^/]+/([^/]+)/([^/]+).*|\1-\2|')

    # Short commit hash
    local short_commit="${commit_hash:0:7}"

    echo "${owner_repo}-${short_commit}"
}

# Generate query hash for cache invalidation
# Usage: get_query_hash <query_text>
get_query_hash() {
    local query_text="$1"

    # Simple hash using md5 (first 8 chars) - portable between Linux and macOS
    echo -n "$query_text" | (md5sum 2>/dev/null || md5) | cut -c1-8
}

# Check if cache entry exists and is valid
# Usage: cache_exists <cache_key> [query_hash]
cache_exists() {
    local cache_key="$1"
    local query_hash="${2:-}"

    local cache_path="$CACHE_DIR/$cache_key"

    # Check if directory exists
    if [ ! -d "$cache_path" ]; then
        return 1
    fi

    # Check if analysis file exists
    if [ ! -f "$cache_path/analysis.md" ]; then
        return 1
    fi

    # Check metadata
    if [ ! -f "$cache_path/metadata.json" ]; then
        return 1
    fi

    # Check TTL
    local created_at
    created_at=$(jq -r '.created_at' "$cache_path/metadata.json" 2>/dev/null || echo "0")

    local current_time
    current_time=$(date +%s)

    local age=$((current_time - created_at))

    if [ "$age" -gt "$CACHE_TTL" ]; then
        echo "⚠ Cache expired (age: ${age}s, TTL: ${CACHE_TTL}s)" >&2
        return 1
    fi

    # If query hash provided, check if it matches
    if [ -n "$query_hash" ]; then
        local cached_query_hash
        cached_query_hash=$(jq -r '.query_hash' "$cache_path/metadata.json" 2>/dev/null || echo "")

        if [ "$cached_query_hash" != "$query_hash" ]; then
            echo "⚠ Cache query mismatch (cached: $cached_query_hash, requested: $query_hash)" >&2
            return 1
        fi
    fi

    echo "✓ Cache hit: $cache_key" >&2
    return 0
}

# Save analysis to cache
# Usage: cache_save <cache_key> <analysis_file> <repo_url> <commit_hash> <query> <context>
cache_save() {
    local cache_key="$1"
    local analysis_file="$2"
    local repo_url="$3"
    local commit_hash="$4"
    local query="$5"
    local context="$6"

    local cache_path="$CACHE_DIR/$cache_key"

    # Create cache directory
    mkdir -p "$cache_path"

    # Copy analysis file
    if [ -f "$analysis_file" ]; then
        cp "$analysis_file" "$cache_path/analysis.md"
    else
        echo "Error: Analysis file not found: $analysis_file" >&2
        return 1
    fi

    # Generate metadata
    local query_hash
    query_hash=$(get_query_hash "$query")

    local current_time
    current_time=$(date +%s)

    cat > "$cache_path/metadata.json" <<METADATA
{
  "cache_key": "$cache_key",
  "repo_url": "$repo_url",
  "commit_hash": "$commit_hash",
  "query": "$query",
  "query_hash": "$query_hash",
  "context": "$context",
  "created_at": $current_time,
  "created_date": "$(date -Iseconds)",
  "ttl_seconds": $CACHE_TTL,
  "expires_at": $((current_time + CACHE_TTL)),
  "expires_date": "$(date -d \"@$((current_time + CACHE_TTL))\" -Iseconds 2>/dev/null || date -r \"$((current_time + CACHE_TTL))\" -Iseconds 2>/dev/null || date -Iseconds)"
}
METADATA

    echo "✓ Cached analysis: $cache_key"
    echo "  Path: $cache_path/analysis.md"
    echo "  Query hash: $query_hash"
    echo "  Expires: $(date -Iseconds -r $((current_time + CACHE_TTL)) 2>/dev/null || echo 'N/A')"
}

# Get cached analysis path
# Usage: cache_get <cache_key>
cache_get() {
    local cache_key="$1"

    local cache_path="$CACHE_DIR/$cache_key"

    if cache_exists "$cache_key"; then
        echo "$cache_path/analysis.md"
        return 0
    else
        return 1
    fi
}

# List all cache entries
# Usage: cache_list [--expired]
cache_list() {
    local show_expired="${1:-}"

    if [ ! -d "$CACHE_DIR" ]; then
        echo "No cache directory found"
        return 0
    fi

    local current_time
    current_time=$(date +%s)

    echo "Cache Directory: $CACHE_DIR"
    echo ""
    printf "%-30s %-15s %-20s %s\n" "CACHE KEY" "STATUS" "CREATED" "QUERY HASH"
    printf "%-30s %-15s %-20s %s\n" "----------" "------" "-------" "----------"

    for cache_entry in "$CACHE_DIR"/*; do
        if [ ! -d "$cache_entry" ]; then
            continue
        fi

        local cache_key
        cache_key=$(basename "$cache_entry")

        if [ ! -f "$cache_entry/metadata.json" ]; then
            printf "%-30s %-15s %-20s %s\n" "$cache_key" "INVALID" "-" "-"
            continue
        fi

        local created_at
        created_at=$(jq -r '.created_at' "$cache_entry/metadata.json" 2>/dev/null || echo "0")

        local query_hash
        query_hash=$(jq -r '.query_hash' "$cache_entry/metadata.json" 2>/dev/null || echo "unknown")

        local created_date
        created_date=$(jq -r '.created_date' "$cache_entry/metadata.json" 2>/dev/null || echo "unknown")

        local age=$((current_time - created_at))

        local status="VALID"
        if [ "$age" -gt "$CACHE_TTL" ]; then
            status="EXPIRED"
        fi

        if [ "$status" = "VALID" ] || [ "$show_expired" = "--expired" ]; then
            printf "%-30s %-15s %-20s %s\n" "${cache_key:0:30}" "$status" "${created_date:0:20}" "$query_hash"
        fi
    done
}

# Purge expired cache entries
# Usage: cache_purge [--force]
cache_purge() {
    local force="${1:-}"

    if [ ! -d "$CACHE_DIR" ]; then
        echo "No cache directory found"
        return 0
    fi

    local current_time
    current_time=$(date +%s)

    local purged=0
    local kept=0

    for cache_entry in "$CACHE_DIR"/*; do
        if [ ! -d "$cache_entry" ]; then
            continue
        fi

        local cache_key
        cache_key=$(basename "$cache_entry")

        if [ ! -f "$cache_entry/metadata.json" ]; then
            # Invalid entry, always purge
            echo "🧹 Purging invalid: $cache_key"
            rm -rf "$cache_entry"
            ((purged++))
            continue
        fi

        local created_at
        created_at=$(jq -r '.created_at' "$cache_entry/metadata.json" 2>/dev/null || echo "0")

        local age=$((current_time - created_at))

        # Purge if expired or force flag set
        if [ "$age" -gt "$MAX_CACHE_AGE" ] || [ "$force" = "--force" ]; then
            echo "🧹 Purging: $cache_key (age: ${age}s)"
            rm -rf "$cache_entry"
            ((purged++))
        else
            ((kept++))
        fi
    done

    echo ""
    echo "📊 Purge Summary:"
    echo "  Purged: $purged"
    echo "  Kept:   $kept"
}

# Get cache statistics
# Usage: cache_stats
cache_stats() {
    if [ ! -d "$CACHE_DIR" ]; then
        echo "No cache directory found"
        return 0
    fi

    local current_time
    current_time=$(date +%s)

    local total=0
    local valid=0
    local expired=0
    local invalid=0
    local total_size=0

    for cache_entry in "$CACHE_DIR"/*; do
        if [ ! -d "$cache_entry" ]; then
            continue
        fi

        ((total++))

        if [ ! -f "$cache_entry/metadata.json" ]; then
            ((invalid++))
            continue
        fi

        local created_at
        created_at=$(jq -r '.created_at' "$cache_entry/metadata.json" 2>/dev/null || echo "0")

        local age=$((current_time - created_at))

        if [ "$age" -gt "$CACHE_TTL" ]; then
            ((expired++))
        else
            ((valid++))
        fi

        # Add to total size
        local entry_size
        entry_size=$(du -sk "$cache_entry" 2>/dev/null | cut -f1 || echo "0")
        total_size=$((total_size + entry_size))
    done

    cat <<STATS
📊 Cache Statistics

Directory: $CACHE_DIR

Entries:
  Total:   $total
  Valid:   $valid
  Expired: $expired
  Invalid: $invalid

Storage:
  Total Size: ${total_size}KB

Configuration:
  TTL:          ${CACHE_TTL}s ($(( CACHE_TTL / 86400 )) days)
  Max Age:      ${MAX_CACHE_AGE}s ($(( MAX_CACHE_AGE / 86400 )) days)
STATS
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    COMMAND="${1:-help}"

    case "$COMMAND" in
        init)
            init_cache
            ;;
        list)
            cache_list "${2:-}"
            ;;
        stats)
            cache_stats
            ;;
        purge)
            cache_purge "${2:-}"
            ;;
        get)
            if [ $# -lt 2 ]; then
                echo "Usage: $0 get <cache_key>" >&2
                exit 1
            fi
            cache_get "$2"
            ;;
        exists)
            if [ $# -lt 2 ]; then
                echo "Usage: $0 exists <cache_key> [query_hash]" >&2
                exit 1
            fi
            cache_exists "$2" "${3:-}"
            ;;
        save)
            if [ $# -lt 6 ]; then
                echo "Usage: $0 save <cache_key> <analysis_file> <repo_url> <commit_hash> <query> <context>" >&2
                exit 1
            fi
            shift
            cache_save "$@"
            ;;
        key)
            if [ $# -lt 3 ]; then
                echo "Usage: $0 key <repo_url> <commit_hash>" >&2
                exit 1
            fi
            get_cache_key "$2" "$3"
            ;;
        *)
            cat <<HELP
Usage: $0 <command> [args...]

Commands:
  init                          Initialize cache directory
  list [--expired]              List cache entries
  stats                         Show cache statistics
  purge [--force]               Purge expired entries
  get <cache_key>               Get cached analysis path
  exists <cache_key> [query_hash]  Check if cache entry exists
  save <key> <file> <url> <commit> <query> <context>  Save analysis
  key <repo_url> <commit_hash>  Generate cache key

Examples:
  $0 init
  $0 list
  $0 stats
  $0 purge
  $0 get user-repo-abc1234
  $0 key https://github.com/user/repo abc1234567890
HELP
            ;;
    esac
fi
