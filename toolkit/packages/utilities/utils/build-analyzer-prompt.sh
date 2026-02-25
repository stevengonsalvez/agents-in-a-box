#!/usr/bin/env bash
#
# build-analyzer-prompt.sh - Build context-aware prompt for focused-repository-analyzer
#
# Usage: build_analyzer_prompt <repo_path> <repo_url> <research_query> <context>
#

set -euo pipefail

# Build analyzer prompt with full context
# Usage: build_analyzer_prompt <repo_path> <repo_url> <research_query> <context>
build_analyzer_prompt() {
    local repo_path="$1"
    local repo_url="$2"
    local research_query="$3"
    local context="$4"

    # Validate inputs
    if [ ! -d "$repo_path" ]; then
        echo "Error: Repository path not found: $repo_path" >&2
        return 1
    fi

    # Extract repository metadata
    local repo_name
    repo_name=$(basename "$repo_path")

    local commit_hash="unknown"
    if [ -d "$repo_path/.git" ]; then
        commit_hash=$(git -C "$repo_path" rev-parse HEAD 2>/dev/null || echo "unknown")
    fi

    local commit_date="unknown"
    if [ -d "$repo_path/.git" ]; then
        commit_date=$(git -C "$repo_path" log -1 --format="%ai" 2>/dev/null || echo "unknown")
    fi

    # Build the prompt
    cat <<EOF
Analyze the external repository at: $repo_path

**Repository Details:**
- Name: $repo_name
- URL: $repo_url
- Commit: $commit_hash
- Last Updated: $commit_date

**Research Query:**
$research_query

**Context (Why This Repository Was Selected):**
$context

**Your Task:**
1. Navigate to the repository at: $repo_path
2. Analyze ONLY the code relevant to the research query above
3. Extract key findings, patterns, and implementation approaches
4. Generate GitHub permalinks for all code references (using commit: $commit_hash)
5. Provide concrete recommendations for our implementation

**Output Requirements:**
- Focus on the research query - don't do comprehensive archaeology
- All code references MUST include GitHub permalinks
- Provide specific, actionable insights
- Limit analysis to 25-35 minutes
- Save analysis to: ~/.claude/research-cache/${repo_name}-${commit_hash:0:7}/analysis.md

**Permalink Format:**
$repo_url/blob/$commit_hash/path/to/file.ext#L123-L145

Begin your focused analysis now.
EOF
}

# Build batch prompt for multiple repositories
# Usage: build_batch_analyzer_prompt <repos_file> <research_query> <context>
build_batch_analyzer_prompt() {
    local repos_file="$1"
    local research_query="$2"
    local context="$3"

    if [ ! -f "$repos_file" ]; then
        echo "Error: Repositories file not found: $repos_file" >&2
        return 1
    fi

    # Count repositories
    local repo_count
    repo_count=$(grep -c "^" "$repos_file" || echo "0")

    cat <<EOF
Analyze $repo_count external repositories in parallel.

**Research Query:**
$research_query

**Context:**
$context

**Repositories to Analyze:**
EOF

    # Read repositories from file (format: path|url)
    while IFS='|' read -r repo_path repo_url || [ -n "$repo_path" ]; do
        [ -z "$repo_path" ] && continue

        local repo_name
        repo_name=$(basename "$repo_path")

        local commit_hash="unknown"
        if [ -d "$repo_path/.git" ]; then
            commit_hash=$(git -C "$repo_path" rev-parse HEAD 2>/dev/null || echo "unknown")
        fi

        cat <<REPO_BLOCK

Repository: $repo_name
- Path: $repo_path
- URL: $repo_url
- Commit: $commit_hash

REPO_BLOCK

    done < "$repos_file"

    cat <<EOF

**Task for Each Repository:**
1. Analyze code relevant to the research query
2. Extract key findings and patterns
3. Generate GitHub permalinks for code references
4. Save analysis to: ~/.claude/research-cache/<repo>-<commit>/analysis.md

**Important:**
- Analyze all repositories IN PARALLEL for efficiency
- Each analysis should complete within 25-35 minutes
- Focus ONLY on research query relevance
- Provide actionable recommendations

Begin parallel analysis now.
EOF
}

# Extract repository info for logging
# Usage: extract_repo_info <repo_path>
extract_repo_info() {
    local repo_path="$1"

    if [ ! -d "$repo_path" ]; then
        echo "Error: Repository path not found: $repo_path" >&2
        return 1
    fi

    local repo_name
    repo_name=$(basename "$repo_path")

    local repo_url="unknown"
    if [ -d "$repo_path/.git" ]; then
        repo_url=$(git -C "$repo_path" config --get remote.origin.url 2>/dev/null || echo "unknown")
    fi

    local commit_hash="unknown"
    if [ -d "$repo_path/.git" ]; then
        commit_hash=$(git -C "$repo_path" rev-parse HEAD 2>/dev/null || echo "unknown")
    fi

    local commit_date="unknown"
    if [ -d "$repo_path/.git" ]; then
        commit_date=$(git -C "$repo_path" log -1 --format="%ai" 2>/dev/null || echo "unknown")
    fi

    cat <<INFO
{
  "name": "$repo_name",
  "url": "$repo_url",
  "commit": "$commit_hash",
  "date": "$commit_date",
  "path": "$repo_path"
}
INFO
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 4 ]]; then
        echo "Usage: $0 <repo_path> <repo_url> <research_query> <context>" >&2
        echo "" >&2
        echo "Examples:" >&2
        echo "  Single repo:" >&2
        echo "    $0 /tmp/awesome-repo https://github.com/user/awesome-repo \\" >&2
        echo "       \"How to implement JWT auth?\" \"Found from web search about authentication\"" >&2
        echo "" >&2
        echo "  Batch mode:" >&2
        echo "    build_batch_analyzer_prompt repos.txt \"JWT auth patterns\" \"Web research results\"" >&2
        exit 1
    fi

    build_analyzer_prompt "$@"
fi
