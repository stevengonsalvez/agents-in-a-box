#!/usr/bin/env bash
# memory_discovery.sh — Discover and clean up orphaned worktree memory directories
#
# Usage:
#   memory_discovery.sh project-id          Print repo name from git remote
#   memory_discovery.sh discover            List orphaned memory dirs for current project
#   memory_discovery.sh discover --json     List as JSON array
#   memory_discovery.sh cleanup <file>      Delete dirs listed in <file> (one path per line)
#   memory_discovery.sh stats               Show count and total lines across orphaned dirs

set -euo pipefail

CLAUDE_PROJECTS_DIR="${HOME}/.claude/projects"

# --- Helpers ---

get_repo_name() {
  local remote_url
  remote_url=$(git remote get-url origin 2>/dev/null) || {
    echo "ERROR: Not in a git repo or no 'origin' remote configured" >&2
    return 1
  }
  # Extract repo name: handles both HTTPS and SSH URLs, strips .git suffix
  echo "$remote_url" | sed -E 's|.*[:/]([^/]+)/([^/.]+)(\.git)?$|\2|'
}

get_current_memory_dir() {
  # Derive the current session's memory dir key the same way Claude Code does:
  # PWD with / _ . replaced by -
  local key
  key=$(echo "$PWD" | tr '/_.' '---')
  echo "${CLAUDE_PROJECTS_DIR}/${key}/memory"
}

# --- Actions ---

action_project_id() {
  get_repo_name
}

action_discover() {
  local repo_name json_mode=false
  repo_name=$(get_repo_name) || return 1

  if [[ "${1:-}" == "--json" ]]; then
    json_mode=true
  fi

  local current_memory_dir
  current_memory_dir=$(get_current_memory_dir)

  local found=()

  # Scan all project dirs for ones containing the repo name (case-insensitive)
  for dir in "${CLAUDE_PROJECTS_DIR}"/*/; do
    [[ -d "$dir" ]] || continue
    local dirname
    dirname=$(basename "$dir")

    # Case-insensitive match on repo name within the path-derived key
    if echo "$dirname" | grep -qi "$repo_name"; then
      local memory_file="${dir}memory/MEMORY.md"
      if [[ -f "$memory_file" ]]; then
        # Skip current session's memory dir
        local memory_dir="${dir}memory"
        if [[ "$memory_dir" == "$current_memory_dir" ]]; then
          continue
        fi
        found+=("$memory_file")
      fi
    fi
  done

  if [[ ${#found[@]} -eq 0 ]]; then
    if $json_mode; then
      echo "[]"
    else
      echo "No orphaned memory directories found for repo: $repo_name"
    fi
    return 0
  fi

  if $json_mode; then
    printf '[\n'
    local first=true
    for f in "${found[@]}"; do
      if $first; then first=false; else printf ',\n'; fi
      printf '  "%s"' "$f"
    done
    printf '\n]\n'
  else
    echo "Found ${#found[@]} orphaned memory files for repo: $repo_name"
    echo ""
    for f in "${found[@]}"; do
      local lines
      lines=$(wc -l < "$f" | tr -d ' ')
      echo "  ${f} (${lines} lines)"
    done
  fi
}

action_stats() {
  local repo_name
  repo_name=$(get_repo_name) || return 1

  local current_memory_dir
  current_memory_dir=$(get_current_memory_dir)

  local count=0 total_lines=0

  for dir in "${CLAUDE_PROJECTS_DIR}"/*/; do
    [[ -d "$dir" ]] || continue
    local dirname
    dirname=$(basename "$dir")
    if echo "$dirname" | grep -qi "$repo_name"; then
      local memory_file="${dir}memory/MEMORY.md"
      if [[ -f "$memory_file" ]]; then
        local memory_dir="${dir}memory"
        [[ "$memory_dir" == "$current_memory_dir" ]] && continue
        count=$((count + 1))
        local lines
        lines=$(wc -l < "$memory_file" | tr -d ' ')
        total_lines=$((total_lines + lines))
      fi
    fi
  done

  echo "Repo: $repo_name"
  echo "Orphaned memory dirs: $count"
  echo "Total lines across all: $total_lines"
}

action_cleanup() {
  local list_file="${1:-}"
  if [[ -z "$list_file" || ! -f "$list_file" ]]; then
    echo "ERROR: Provide a file listing directories to delete (one per line)" >&2
    return 1
  fi

  local deleted=0 skipped=0
  while IFS= read -r dir_path; do
    [[ -z "$dir_path" ]] && continue
    # Derive the memory/ parent dir from the MEMORY.md path
    local memory_dir
    memory_dir=$(dirname "$dir_path")

    # Safety: only delete under $HOME/.claude/projects/
    if [[ "$memory_dir" != "${CLAUDE_PROJECTS_DIR}"/* ]]; then
      echo "SKIP (outside projects dir): $memory_dir" >&2
      skipped=$((skipped + 1))
      continue
    fi

    if [[ -d "$memory_dir" ]]; then
      rm -rf "$memory_dir"
      # Also remove the parent project dir if now empty
      local project_dir
      project_dir=$(dirname "$memory_dir")
      if [[ -d "$project_dir" ]] && [[ -z "$(ls -A "$project_dir")" ]]; then
        rmdir "$project_dir"
      fi
      deleted=$((deleted + 1))
    else
      echo "SKIP (not found): $memory_dir" >&2
      skipped=$((skipped + 1))
    fi
  done < "$list_file"

  echo "Deleted: $deleted directories"
  [[ $skipped -gt 0 ]] && echo "Skipped: $skipped directories"
}

# --- Main ---

case "${1:-help}" in
  project-id)
    action_project_id
    ;;
  discover)
    shift
    action_discover "${1:-}"
    ;;
  stats)
    action_stats
    ;;
  cleanup)
    shift
    action_cleanup "${1:-}"
    ;;
  help|--help|-h)
    echo "Usage: memory_discovery.sh <action> [args]"
    echo ""
    echo "Actions:"
    echo "  project-id          Print repo name from git remote"
    echo "  discover [--json]   List orphaned memory dirs for current project"
    echo "  stats               Show count and total lines"
    echo "  cleanup <file>      Delete dirs listed in file"
    echo ""
    echo "Examples:"
    echo "  memory_discovery.sh project-id"
    echo "  memory_discovery.sh discover"
    echo "  memory_discovery.sh discover --json"
    echo "  memory_discovery.sh stats"
    echo "  memory_discovery.sh cleanup /tmp/reflect-cleanup-dirs.txt"
    ;;
  *)
    echo "ERROR: Unknown action: $1" >&2
    echo "Run with --help for usage." >&2
    exit 1
    ;;
esac
