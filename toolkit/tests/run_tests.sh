#!/usr/bin/env bash
# run_tests.sh - Test runner for the Unified Knowledge Loop
#
# Usage:
#   ./run_tests.sh           # E2E tests only (fast)
#   ./run_tests.sh --e2e     # E2E tests only
#   ./run_tests.sh --heavy   # All tests including heavy (needs sentence-transformers)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Colour helpers
GREEN='\033[0;32m'
BOLD='\033[1m'
RESET='\033[0m'

case "${1:-all}" in
    --e2e)
        echo -e "${BOLD}Running E2E tests${RESET}"
        python -m pytest e2e/ -v --tb=short
        ;;
    --heavy)
        echo -e "${BOLD}Running ALL tests including heavy${RESET}"
        python -m pytest e2e/ heavy/ -v --tb=short
        ;;
    all|"")
        echo -e "${BOLD}Running E2E tests (use --heavy to include GraphRAG)${RESET}"
        python -m pytest e2e/ -v --tb=short -m "not heavy"
        ;;
    *)
        echo "Usage: $0 [--e2e|--heavy]"
        exit 1
        ;;
esac

echo -e "\n${GREEN}${BOLD}Done.${RESET}"
