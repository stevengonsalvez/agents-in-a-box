#!/bin/bash
# ABOUTME: Run all tests for boss mode functionality

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "🧪 Running all boss mode tests..."
echo "=================================="
echo

# Run unit tests
echo "📋 Running unit tests..."
"$SCRIPT_DIR/test_boss_mode_unit.sh"
echo

# Run original prompt injection tests
echo "📋 Running prompt injection tests..."
"$SCRIPT_DIR/test_boss_mode_prompt_injection.sh"
echo

# Run Claude CLI syntax fix tests
echo "📋 Running Claude CLI syntax tests..."
"$SCRIPT_DIR/test_claude_cli_syntax_fix.sh"
echo

# Run simple boss mode tests
echo "📋 Running simple boss mode tests..."
"$SCRIPT_DIR/test_simple_boss_mode.sh"
echo

# Run git commit success UI tests
echo "📋 Running git commit success UI tests..."
"$SCRIPT_DIR/test_git_commit_success_ui.sh"
echo

echo "✅ All tests completed successfully!"
echo
echo "📝 Summary:"
echo "- Boss mode prompt injection function works correctly"
echo "- Environment variable CLAUDE_BOSS_MODE controls behavior"
echo "- Prompt text includes TDD and commit guidelines"
echo "- Claude CLI syntax is correct (prompt as positional argument)"
echo "- Git commit success UI feedback and screen transition implemented"
echo "- Ready for container rebuild and deployment"
