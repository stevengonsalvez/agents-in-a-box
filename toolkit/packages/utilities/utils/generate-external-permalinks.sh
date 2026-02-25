#!/usr/bin/env bash
#
# generate-external-permalinks.sh - Generate GitHub permalinks for external repository code
#
# Usage: generate_permalink <repo_path> <file_path> <start_line> [end_line]
#

set -euo pipefail

# Generate GitHub permalink for a file reference
# Usage: generate_permalink <repo_path> <file_path> <start_line> [end_line]
generate_permalink() {
    local repo_path="$1"
    local file_path="$2"
    local start_line="$3"
    local end_line="${4:-$start_line}"  # Default to same as start if not provided

    # Validate inputs
    if [ ! -d "$repo_path/.git" ]; then
        echo "Error: Not a git repository: $repo_path" >&2
        return 1
    fi

    # Get repository URL
    local repo_url
    repo_url=$(git -C "$repo_path" config --get remote.origin.url 2>/dev/null || echo "")

    if [ -z "$repo_url" ]; then
        echo "Error: No remote origin URL found" >&2
        return 1
    fi

    # Clean up URL (remove .git suffix, convert SSH to HTTPS)
    repo_url=$(echo "$repo_url" | sed 's/\.git$//')
    repo_url=$(echo "$repo_url" | sed 's|^git@github\.com:|https://github.com/|')
    repo_url=$(echo "$repo_url" | sed 's|^git@gitlab\.com:|https://gitlab.com/|')

    # Get current commit hash
    local commit_hash
    commit_hash=$(git -C "$repo_path" rev-parse HEAD 2>/dev/null || echo "")

    if [ -z "$commit_hash" ]; then
        echo "Error: Could not get commit hash" >&2
        return 1
    fi

    # Normalize file path (remove repo_path prefix if present)
    local rel_file_path
    rel_file_path=$(echo "$file_path" | sed "s|^${repo_path}/||")

    # Generate permalink
    if [ "$start_line" = "$end_line" ]; then
        # Single line
        echo "${repo_url}/blob/${commit_hash}/${rel_file_path}#L${start_line}"
    else
        # Line range
        echo "${repo_url}/blob/${commit_hash}/${rel_file_path}#L${start_line}-L${end_line}"
    fi
}

# Convert local file:line reference to permalink
# Usage: convert_reference <repo_path> <reference>
# Reference format: "path/to/file.ext:123" or "path/to/file.ext:123-145"
convert_reference() {
    local repo_path="$1"
    local reference="$2"

    # Extract file path and line numbers
    local file_part
    file_part=$(echo "$reference" | cut -d: -f1)

    local line_part
    line_part=$(echo "$reference" | cut -d: -f2)

    # Parse line numbers
    if [[ "$line_part" =~ ^([0-9]+)-([0-9]+)$ ]]; then
        # Range: 123-145
        local start_line="${BASH_REMATCH[1]}"
        local end_line="${BASH_REMATCH[2]}"
        generate_permalink "$repo_path" "$repo_path/$file_part" "$start_line" "$end_line"
    elif [[ "$line_part" =~ ^([0-9]+)$ ]]; then
        # Single line: 123
        local line_num="${BASH_REMATCH[1]}"
        generate_permalink "$repo_path" "$repo_path/$file_part" "$line_num"
    else
        echo "Error: Invalid line reference: $line_part" >&2
        return 1
    fi
}

# Generate markdown link with permalink
# Usage: generate_markdown_link <repo_path> <reference> <link_text>
generate_markdown_link() {
    local repo_path="$1"
    local reference="$2"
    local link_text="${3:-$reference}"  # Default to reference if no text provided

    local permalink
    permalink=$(convert_reference "$repo_path" "$reference")

    if [ $? -eq 0 ]; then
        echo "[\`$link_text\`]($permalink)"
    else
        return 1
    fi
}

# Batch convert references in a file
# Usage: batch_convert_file <repo_path> <input_file> <output_file>
batch_convert_file() {
    local repo_path="$1"
    local input_file="$2"
    local output_file="$3"

    if [ ! -f "$input_file" ]; then
        echo "Error: Input file not found: $input_file" >&2
        return 1
    fi

    # Process file line by line
    while IFS= read -r line; do
        # Look for file:line patterns (includes files without extensions like Makefile, Dockerfile)
        if [[ "$line" =~ ([a-zA-Z0-9/_.-]+):([0-9]+(-[0-9]+)?) ]]; then
            local reference="${BASH_REMATCH[0]}"
            local permalink
            permalink=$(convert_reference "$repo_path" "$reference" 2>/dev/null || echo "$reference")

            # Replace in line
            line=$(echo "$line" | sed "s|$reference|$permalink|g")
        fi

        echo "$line" >> "$output_file"
    done < "$input_file"

    echo "✓ Converted references in: $output_file"
}

# Get repository metadata for permalink generation
# Usage: get_repo_metadata <repo_path>
get_repo_metadata() {
    local repo_path="$1"

    if [ ! -d "$repo_path/.git" ]; then
        echo "Error: Not a git repository: $repo_path" >&2
        return 1
    fi

    local repo_url
    repo_url=$(git -C "$repo_path" config --get remote.origin.url 2>/dev/null || echo "unknown")
    repo_url=$(echo "$repo_url" | sed 's/\.git$//' | sed 's|^git@github\.com:|https://github.com/|')

    local commit_hash
    commit_hash=$(git -C "$repo_path" rev-parse HEAD 2>/dev/null || echo "unknown")

    local commit_date
    commit_date=$(git -C "$repo_path" log -1 --format="%ai" 2>/dev/null || echo "unknown")

    local repo_name
    repo_name=$(basename "$repo_path")

    cat <<METADATA
Repository: $repo_name
URL: $repo_url
Commit: $commit_hash
Date: $commit_date
Permalink Base: ${repo_url}/blob/${commit_hash}/
METADATA
}

# Main execution if run directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -lt 1 ]]; then
        cat <<USAGE >&2
Usage: $0 <command> <repo_path> <args...>

Commands:
  permalink <repo_path> <file_path> <start_line> [end_line]
      Generate permalink for file and line number(s)

  convert <repo_path> <reference>
      Convert "file.ext:123" or "file.ext:123-145" to permalink

  markdown <repo_path> <reference> [link_text]
      Generate markdown link with permalink

  batch <repo_path> <input_file> <output_file>
      Convert all file:line references in input_file to permalinks

  metadata <repo_path>
      Show repository metadata for permalink generation

Examples:
  $0 permalink /tmp/repo src/main.rs 45 67
  $0 convert /tmp/repo "src/lib.rs:123-145"
  $0 markdown /tmp/repo "README.md:10" "See README"
  $0 batch /tmp/repo analysis.md analysis-with-links.md
  $0 metadata /tmp/repo
USAGE
        exit 1
    fi

    COMMAND="$1"
    shift

    case "$COMMAND" in
        permalink)
            generate_permalink "$@"
            ;;
        convert)
            convert_reference "$@"
            ;;
        markdown)
            generate_markdown_link "$@"
            ;;
        batch)
            batch_convert_file "$@"
            ;;
        metadata)
            get_repo_metadata "$@"
            ;;
        *)
            echo "Error: Unknown command: $COMMAND" >&2
            exit 1
            ;;
    esac
fi
