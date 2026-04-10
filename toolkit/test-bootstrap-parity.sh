#!/usr/bin/env bash
# test-bootstrap-parity.sh
#
# Runs `node bootstrap.js --tool=X --homeDir=/tmp/baseline-X` for each existing tool
# and diffs the output against a baseline snapshot. Used to verify backward compatibility
# when making changes to bootstrap.js or toolkit/packages.
#
# Workflow:
#   1. Before making changes: ./test-bootstrap-parity.sh baseline
#      → captures snapshots into /tmp/bootstrap-parity-baseline/
#   2. After making changes: ./test-bootstrap-parity.sh verify
#      → runs bootstrap again into /tmp/bootstrap-parity-after/ and diffs vs baseline
#   3. Clean: ./test-bootstrap-parity.sh clean
#      → removes /tmp/bootstrap-parity-*/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BOOTSTRAP="$SCRIPT_DIR/bootstrap.js"
BASELINE_DIR="/tmp/bootstrap-parity-baseline"
AFTER_DIR="/tmp/bootstrap-parity-after"

# Tools to verify backward compat for. Each of these must produce identical output
# before and after bootstrap.js changes (unless the change is an intentional addition).
TOOLS=(claude-code-4.5 codex copilot gemini)

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No color

usage() {
    cat <<EOF
Usage: $0 <command>

Commands:
    baseline    Capture the current bootstrap output as the baseline snapshot
    verify      Run bootstrap again and diff against baseline (fails on any diff)
    clean       Remove all parity test directories

Examples:
    # Before making changes:
    $0 baseline

    # After making changes:
    $0 verify

    # If baseline is stale:
    $0 clean && $0 baseline
EOF
    exit 1
}

run_bootstrap() {
    local tool="$1"
    local out_dir="$2"

    mkdir -p "$out_dir"
    echo -e "${YELLOW}→ Running bootstrap --tool=$tool --homeDir=$out_dir${NC}"

    # Run bootstrap silently, fail on error
    if ! (cd "$SCRIPT_DIR" && node "$BOOTSTRAP" --tool="$tool" --homeDir="$out_dir" >/dev/null 2>&1); then
        echo -e "${RED}✗ bootstrap failed for tool=$tool${NC}" >&2
        return 1
    fi

    # Normalize volatile files (timestamps, absolute paths inside generated scripts)
    # This prevents false positives on diff.
    normalize_output "$out_dir"
}

normalize_output() {
    local dir="$1"

    # Strip any file that contains a timestamp or the overridden homeDir path.
    # The setup-external.sh script is regenerated each run and contains the homeDir path.
    if [ -f "$dir/.claude/setup-external.sh" ]; then
        # Replace the tmp homeDir with a placeholder so diffs don't show path variance
        sed -i.bak "s|$dir|__HOMEDIR__|g" "$dir/.claude/setup-external.sh" 2>/dev/null || true
        rm -f "$dir/.claude/setup-external.sh.bak"
    fi

    for subdir in .hermes .codex .copilot .gemini .claude; do
        local script="$dir/$subdir/setup-external.sh"
        if [ -f "$script" ]; then
            sed -i.bak "s|$dir|__HOMEDIR__|g" "$script" 2>/dev/null || true
            rm -f "$script.bak"
        fi
    done
}

cmd_baseline() {
    echo -e "${GREEN}=== Capturing baseline ===${NC}"
    rm -rf "$BASELINE_DIR"
    mkdir -p "$BASELINE_DIR"

    for tool in "${TOOLS[@]}"; do
        run_bootstrap "$tool" "$BASELINE_DIR/$tool"
    done

    echo -e "${GREEN}✓ Baseline captured at $BASELINE_DIR${NC}"
    echo
    echo "Tools captured:"
    for tool in "${TOOLS[@]}"; do
        local count=$(find "$BASELINE_DIR/$tool" -type f 2>/dev/null | wc -l | tr -d ' ')
        echo "  - $tool: $count files"
    done
}

cmd_verify() {
    if [ ! -d "$BASELINE_DIR" ]; then
        echo -e "${RED}✗ No baseline found at $BASELINE_DIR${NC}"
        echo "  Run '$0 baseline' first (before making changes)."
        exit 1
    fi

    echo -e "${GREEN}=== Running verify against baseline ===${NC}"
    rm -rf "$AFTER_DIR"
    mkdir -p "$AFTER_DIR"

    local any_diff=0
    for tool in "${TOOLS[@]}"; do
        run_bootstrap "$tool" "$AFTER_DIR/$tool"
    done

    echo
    echo -e "${GREEN}=== Parity diff results ===${NC}"

    for tool in "${TOOLS[@]}"; do
        local before_dir="$BASELINE_DIR/$tool"
        local after_dir="$AFTER_DIR/$tool"

        local diff_output
        if diff_output=$(diff -rq "$before_dir" "$after_dir" 2>&1); then
            echo -e "${GREEN}✓ $tool: parity intact (no diff)${NC}"
        else
            any_diff=1
            echo -e "${RED}✗ $tool: DIFFERENCES DETECTED${NC}"
            echo "$diff_output" | head -40 | sed 's/^/    /'

            # Count diff types
            local only_new=$(echo "$diff_output" | grep -c "^Only in $after_dir" || echo 0)
            local only_removed=$(echo "$diff_output" | grep -c "^Only in $before_dir" || echo 0)
            local modified=$(echo "$diff_output" | grep -c "^Files.*differ" || echo 0)

            echo "    Summary: $only_new new, $only_removed removed, $modified modified"

            if [ "$only_removed" -gt 0 ] || [ "$modified" -gt 0 ]; then
                echo -e "    ${RED}BREAKING: files removed or modified${NC}"
            elif [ "$only_new" -gt 0 ]; then
                echo -e "    ${YELLOW}ADDITIVE: only new files${NC}"
            fi
        fi
    done

    echo
    if [ "$any_diff" -eq 0 ]; then
        echo -e "${GREEN}✓ ALL TOOLS PASS PARITY CHECK${NC}"
        return 0
    else
        echo -e "${RED}✗ PARITY CHECK FAILED — review diffs above${NC}"
        echo
        echo "To see full diff for a specific tool:"
        echo "  diff -r $BASELINE_DIR/<tool> $AFTER_DIR/<tool>"
        return 1
    fi
}

cmd_clean() {
    rm -rf "$BASELINE_DIR" "$AFTER_DIR"
    echo -e "${GREEN}✓ Cleaned parity test directories${NC}"
}

# Dispatch
case "${1:-}" in
    baseline) cmd_baseline ;;
    verify)   cmd_verify ;;
    clean)    cmd_clean ;;
    *)        usage ;;
esac
