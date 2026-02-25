#!/usr/bin/env bash
#
# detect-repo-urls.sh - Extract and validate repository URLs from text
#

set -euo pipefail

# Extract repository URLs from input text
# Usage: detect_repo_urls <file_or_stdin>
detect_repo_urls() {
    local input="${1:--}"  # Default to stdin if no file provided

    # Supported patterns (match owner/repo avoiding common terminators)
    local github_pattern='https?://github\.com/[^/]+/[^/[:space:])">]+'
    local gitlab_pattern='https?://gitlab\.com/[^/]+/[^/[:space:])">]+'
    local bitbucket_pattern='https?://bitbucket\.org/[^/]+/[^/[:space:])">]+'

    # Extract and deduplicate
    cat "$input" | \
        grep -oE "(${github_pattern}|${gitlab_pattern}|${bitbucket_pattern})" | \
        sed 's/\.git$//' | \
        sed 's/[,;:.]$//' | \
        sed 's/\/$//' | \
        sort -u
}

# Validate repository URL (check if accessible)
# Usage: validate_repo_url <url>
validate_repo_url() {
    local url="$1"

    # Basic URL format check
    if ! echo "$url" | grep -qE '^https?://(github|gitlab|bitbucket)\.(com|org)/[^/]+/[^/]+$'; then
        return 1
    fi

    # Check if URL is accessible (HTTP HEAD request)
    if command -v curl &> /dev/null; then
        if curl --head --silent --fail "$url" > /dev/null 2>&1; then
            return 0
        fi
    fi

    # If curl failed or not available, assume valid
    # (Avoid false negatives due to rate limiting)
    return 0
}

# Parse repository owner and name from URL
# Usage: parse_repo_info <url>
# Output: owner repo-name
parse_repo_info() {
    local url="$1"

    # Extract owner and repo from URL
    echo "$url" | sed -E 's|https?://[^/]+/([^/]+)/([^/]+).*|\1 \2|'
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -eq 0 ]]; then
        detect_repo_urls
    else
        detect_repo_urls "$1"
    fi
fi
