#!/usr/bin/env bash
#
# cleanup-handler.sh - Robust cleanup handler for temporary directories
#
# Usage: Source this file and use register_cleanup_handler
#

set -euo pipefail

# Global array to track cleanup directories
declare -a CLEANUP_DIRS=()
declare -a CLEANUP_FILES=()

# Register directory for cleanup
# Usage: register_cleanup_dir <directory>
register_cleanup_dir() {
    local dir="$1"

    if [ -z "$dir" ]; then
        echo "Error: Directory path is required" >&2
        return 1
    fi

    # Add to cleanup array
    CLEANUP_DIRS+=("$dir")

    # Return the directory path (for convenience)
    echo "$dir"
}

# Register file for cleanup
# Usage: register_cleanup_file <file>
register_cleanup_file() {
    local file="$1"

    if [ -z "$file" ]; then
        echo "Error: File path is required" >&2
        return 1
    fi

    # Add to cleanup array
    CLEANUP_FILES+=("$file")

    # Return the file path (for convenience)
    echo "$file"
}

# Cleanup function (called by trap)
cleanup_registered_resources() {
    local exit_code=$?

    # Cleanup files
    if [ ${#CLEANUP_FILES[@]} -gt 0 ]; then
        for file in "${CLEANUP_FILES[@]}"; do
            if [ -f "$file" ]; then
                echo "🧹 Cleaning up file: $file" >&2
                rm -f "$file" 2>/dev/null || true
            fi
        done
    fi

    # Cleanup directories
    if [ ${#CLEANUP_DIRS[@]} -gt 0 ]; then
        for dir in "${CLEANUP_DIRS[@]}"; do
            if [ -d "$dir" ]; then
                echo "🧹 Cleaning up directory: $dir" >&2
                rm -rf "$dir" 2>/dev/null || true
            fi
        done
    fi

    # Exit with original exit code
    exit $exit_code
}

# Install trap handlers
install_cleanup_traps() {
    # Trap on EXIT, INT, TERM, ERR
    trap cleanup_registered_resources EXIT
    trap cleanup_registered_resources INT
    trap cleanup_registered_resources TERM
    trap cleanup_registered_resources ERR
}

# Create temporary directory with automatic cleanup
# Usage: create_temp_dir [prefix]
create_temp_dir() {
    local prefix="${1:-claude-temp}"

    # Create temp directory
    local temp_dir
    temp_dir=$(mktemp -d -t "${prefix}-XXXXXXXX")

    # Register for cleanup
    register_cleanup_dir "$temp_dir"

    # Return the path
    echo "$temp_dir"
}

# Create temporary file with automatic cleanup
# Usage: create_temp_file [prefix]
create_temp_file() {
    local prefix="${1:-claude-temp}"

    # Create temp file
    local temp_file
    temp_file=$(mktemp -t "${prefix}-XXXXXXXX")

    # Register for cleanup
    register_cleanup_file "$temp_file"

    # Return the path
    echo "$temp_file"
}

# Manual cleanup (call before normal exit if needed)
# Usage: cleanup_now
cleanup_now() {
    cleanup_registered_resources
    # Clear arrays to prevent double cleanup
    CLEANUP_DIRS=()
    CLEANUP_FILES=()
}

# Prevent cleanup for a specific directory (preserve it)
# Usage: preserve_dir <directory>
preserve_dir() {
    local dir="$1"

    # Remove from cleanup array
    local new_dirs=()
    for d in "${CLEANUP_DIRS[@]}"; do
        if [ "$d" != "$dir" ]; then
            new_dirs+=("$d")
        fi
    done
    CLEANUP_DIRS=("${new_dirs[@]}")

    echo "🔒 Preserved directory: $dir" >&2
}

# Prevent cleanup for a specific file (preserve it)
# Usage: preserve_file <file>
preserve_file() {
    local file="$1"

    # Remove from cleanup array
    local new_files=()
    for f in "${CLEANUP_FILES[@]}"; do
        if [ "$f" != "$file" ]; then
            new_files+=("$f")
        fi
    done
    CLEANUP_FILES=("${new_files[@]}")

    echo "🔒 Preserved file: $file" >&2
}

# Example usage demonstration
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "=== Cleanup Handler Demo ==="

    # Install trap handlers
    install_cleanup_traps

    # Create temp resources
    TEMP_DIR=$(create_temp_dir "demo")
    TEMP_FILE=$(create_temp_file "demo")

    echo "✓ Created temp dir: $TEMP_DIR"
    echo "✓ Created temp file: $TEMP_FILE"

    # Create some content
    echo "test content" > "$TEMP_FILE"
    mkdir -p "$TEMP_DIR/subdir"
    echo "nested content" > "$TEMP_DIR/subdir/test.txt"

    echo "✓ Added content to temp resources"
    echo ""
    echo "Temp resources will be cleaned up on exit..."
    echo "Press Ctrl+C to test interrupt cleanup, or wait 3 seconds for normal exit"

    sleep 3

    echo ""
    echo "✓ Normal exit - cleanup will happen automatically"
fi
