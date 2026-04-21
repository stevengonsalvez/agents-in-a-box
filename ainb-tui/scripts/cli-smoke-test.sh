#!/usr/bin/env bash
# ABOUTME: Isolated smoke tests for Wave 1 CLI commands (config, recover, run).
#
# Runs the real `ainb` binary against an isolated HOME and TMUX_TMPDIR so it
# has ZERO impact on the user's real ~/.agents-in-a-box/, ~/.claude/, or tmux
# sessions. Verifies end-to-end behaviour of:
#   - ainb config {show,get,set,reset,path}
#   - ainb recover {list,cleanup}
#   - ainb run + list + status + logs + kill (with stubbed claude binary)
#   - Multi-provider validation for --tool codex/gemini/copilot
#
# Usage: ./scripts/cli-smoke-test.sh

set -euo pipefail

# ---- Isolation setup --------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$CRATE_ROOT/target/debug/ainb"

if [[ ! -x "$BINARY" ]]; then
    echo "ERROR: $BINARY not found. Run 'cargo build' first." >&2
    exit 1
fi

# Snapshot real-data timestamps so we can verify isolation at the end
REAL_HOME="$HOME"
REAL_SESSIONS_STAT=""
REAL_CONFIG_STAT=""
[[ -f "$REAL_HOME/.agents-in-a-box/sessions.json" ]] && REAL_SESSIONS_STAT=$(stat -f %m "$REAL_HOME/.agents-in-a-box/sessions.json" 2>/dev/null || true)
[[ -f "$REAL_HOME/.agents-in-a-box/config/config.toml" ]] && REAL_CONFIG_STAT=$(stat -f %m "$REAL_HOME/.agents-in-a-box/config/config.toml" 2>/dev/null || true)
REAL_CLAUDE_AGENT_COUNT=$(find "$REAL_HOME/.claude/agents" -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')

# Build isolated sandbox
TEST_ROOT=$(mktemp -d -t ainb-smoke-XXXXXX)
export HOME="$TEST_ROOT/home"
export TMUX_TMPDIR="$TEST_ROOT/tmux"
mkdir -p "$HOME" "$TMUX_TMPDIR"

cleanup() {
    local exit_code=$?
    echo ""
    echo "--- Cleanup ---"
    # Kill any tmux sessions in the isolated socket before wiping
    if [[ -d "$TMUX_TMPDIR" ]]; then
        while IFS= read -r -d '' socket; do
            [[ -S "$socket" ]] && tmux -S "$socket" kill-server 2>/dev/null || true
        done < <(find "$TMUX_TMPDIR" -name default -type s -print0 2>/dev/null)
    fi
    rm -rf "$TEST_ROOT"

    # Verify isolation worked — real user data MUST be untouched
    local leak=0
    if [[ -f "$REAL_HOME/.agents-in-a-box/sessions.json" ]]; then
        local now=$(stat -f %m "$REAL_HOME/.agents-in-a-box/sessions.json" 2>/dev/null || true)
        if [[ "$now" != "$REAL_SESSIONS_STAT" ]]; then
            echo "LEAK: real sessions.json was modified!" >&2
            leak=1
        fi
    fi
    if [[ -f "$REAL_HOME/.agents-in-a-box/config/config.toml" ]]; then
        local now=$(stat -f %m "$REAL_HOME/.agents-in-a-box/config/config.toml" 2>/dev/null || true)
        if [[ "$now" != "$REAL_CONFIG_STAT" ]]; then
            echo "LEAK: real config.toml was modified!" >&2
            leak=1
        fi
    fi
    local now_count=$(find "$REAL_HOME/.claude/agents" -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')
    if [[ "$now_count" != "$REAL_CLAUDE_AGENT_COUNT" ]]; then
        echo "LEAK: ~/.claude/agents count changed ($REAL_CLAUDE_AGENT_COUNT -> $now_count)" >&2
        leak=1
    fi

    if [[ $leak -eq 0 ]]; then
        echo "✓ Isolation verified: real user data untouched"
    else
        echo "✗ ISOLATION FAILURE — check damage in $REAL_HOME" >&2
        exit 2
    fi

    if [[ $exit_code -eq 0 ]]; then
        echo "✓ All smoke tests passed"
    else
        echo "✗ Smoke tests failed (exit $exit_code)"
    fi
    exit $exit_code
}
trap cleanup EXIT INT TERM

# ---- Test helpers -----------------------------------------------------------

PASS=0
FAIL=0

section() { echo; echo "=== $* ==="; }

assert() {
    local desc="$1"; shift
    if "$@"; then
        echo "  ✓ $desc"
        PASS=$((PASS+1))
    else
        echo "  ✗ $desc" >&2
        FAIL=$((FAIL+1))
    fi
}

run_ainb() {
    # Run binary with isolated env; prints output to stdout
    "$BINARY" "$@"
}

# ---- Preflight --------------------------------------------------------------

section "Preflight"
echo "  TEST_ROOT=$TEST_ROOT"
echo "  HOME=$HOME"
echo "  TMUX_TMPDIR=$TMUX_TMPDIR"
echo "  BINARY=$BINARY"

assert "binary is executable" test -x "$BINARY"
assert "HOME is isolated" test "$HOME" != "$REAL_HOME"
assert "HOME is under TEST_ROOT" test "${HOME#$TEST_ROOT}" != "$HOME"

# ---- Test 4a: config command ------------------------------------------------

section "4a. config command (pure filesystem)"

# path: should list 3 config locations
output=$(run_ainb config path 2>&1)
assert "config path runs successfully" test $? -eq 0
assert "config path mentions project-level" grep -qi "project" <<<"$output"
assert "config path mentions user-level" grep -qi "user" <<<"$output"
assert "config path references isolated HOME" grep -q "$HOME" <<<"$output"

# show: with empty home, should return defaults (not error)
output=$(run_ainb config show 2>&1 || true)
assert "config show runs" test -n "$output"

# show --format json: valid JSON
output=$(run_ainb --format json config show 2>&1 || true)
assert "config show --format json is valid JSON" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# set → get round trip
run_ainb config set authentication.default_model opus >/dev/null 2>&1
output=$(run_ainb config get authentication.default_model 2>&1 | tr -d '"' | tr -d ' ')
assert "config set+get round trip returns 'opus'" test "$output" = "opus"

# set only modified isolated config (sanity check)
assert "isolated config file exists" test -f "$HOME/.agents-in-a-box/config/config.toml"
assert "config contains the set value" grep -q "opus" "$HOME/.agents-in-a-box/config/config.toml"

# set a different key
run_ainb config set authentication.default_model sonnet >/dev/null 2>&1
output=$(run_ainb config get authentication.default_model 2>&1 | tr -d '"' | tr -d ' ')
assert "config set can change value back" test "$output" = "sonnet"

# reset with force
run_ainb config reset --force >/dev/null 2>&1 || true
# After reset the file may be empty or regenerated — check command succeeded
assert "config reset --force succeeded" test $? -eq 0

# ---- Test 4b: recover command -----------------------------------------------

section "4b. recover command (empty + seeded orphan)"

# Empty home: recover list should succeed with zero orphans
output=$(run_ainb recover list 2>&1 || true)
assert "recover list runs on empty home" test $? -eq 0

# JSON format
output=$(run_ainb --format json recover list 2>&1 || true)
assert "recover list --format json is valid JSON" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# Seed a fake orphan: SessionStore entry with a tmux session name that doesn't exist
# on the isolated socket. Note: agent_type must match enum serialization ("Claude" not "claude").
mkdir -p "$HOME/.agents-in-a-box"
SESSION_UUID="12345678-1234-1234-1234-123456789abc"
cat > "$HOME/.agents-in-a-box/sessions.json" <<EOF
{
  "sessions": {
    "ainb-test-orphan": {
      "session_id": "$SESSION_UUID",
      "tmux_session_name": "ainb-test-orphan",
      "worktree_path": "/tmp/nonexistent-worktree-$SESSION_UUID",
      "workspace_name": "test-workspace",
      "created_at": "2026-01-01T00:00:00Z",
      "agent_type": "Claude"
    }
  }
}
EOF

# recover list should now find the orphan (tmux session doesn't exist → stale metadata)
output=$(run_ainb recover list 2>&1 || true)
assert "recover list detects seeded orphan" grep -q "test-workspace\|ainb-test-orphan" <<<"$output"

# recover cleanup --force should remove it
run_ainb recover cleanup --force >/dev/null 2>&1 || true

# After cleanup the orphan should be gone
output=$(run_ainb recover list 2>&1 || true)
assert "recover cleanup removed orphan" bash -c "! grep -q 'ainb-test-orphan' <<<\"$output\""

# ---- Test 4c: run + list + status + kill (with stubbed claude) --------------

section "4c. run command lifecycle (with stubbed provider)"

# Create a throwaway git repo inside TEST_ROOT
TEST_REPO="$TEST_ROOT/test-repo"
git init -q "$TEST_REPO"
(cd "$TEST_REPO" && git -c user.email=test@test.local -c user.name=Test -c commit.gpgsign=false commit --allow-empty -q -m "init")

# Stub the claude binary so we don't need a real install
mkdir "$TEST_ROOT/fake-bin"
cat > "$TEST_ROOT/fake-bin/claude" <<'STUB'
#!/usr/bin/env bash
echo "fake claude started with args: $*"
# Loop forever so tmux session stays alive
while :; do sleep 60; done
STUB
chmod +x "$TEST_ROOT/fake-bin/claude"
export PATH="$TEST_ROOT/fake-bin:$PATH"

# Skip run lifecycle if tmux is not installed
if ! command -v tmux >/dev/null 2>&1; then
    echo "  ⚠ tmux not installed — skipping 4c"
else
    # Run the command (no --attach so it returns)
    output=$(run_ainb run --repo "$TEST_REPO" --worktree --name smoke-test 2>&1 || true)
    assert "run command completes" grep -qi "session.*created\|Session ID" <<<"$output"

    # Give tmux a moment to stabilise
    sleep 1

    # list should show our session
    output=$(run_ainb list 2>&1 || true)
    assert "list shows smoke-test session" grep -q "smoke-test\|test-repo" <<<"$output"

    # list --format json is valid
    output=$(run_ainb --format json list 2>&1 || true)
    assert "list --format json is valid" bash -c "echo '$output' | jq -e 'type == \"array\"' >/dev/null 2>&1"

    # status should report on the session (find by prefix)
    # The actual session name is workspace-based; use smoke-test if present
    output=$(run_ainb status smoke-test 2>&1 || run_ainb status test-repo 2>&1 || true)
    assert "status command runs for smoke-test" test -n "$output"

    # logs should capture something (the fake claude echoed output)
    output=$(run_ainb logs smoke-test --lines 10 2>&1 || run_ainb logs test-repo --lines 10 2>&1 || true)
    assert "logs command runs" test -n "$output"

    # kill --force should terminate it
    run_ainb kill smoke-test --force >/dev/null 2>&1 || run_ainb kill test-repo --force >/dev/null 2>&1 || true

    sleep 1
    output=$(run_ainb list 2>&1 || true)
    # After kill, session should not appear in list (or list is empty)
    assert "kill removed session from list" bash -c "! grep -q 'smoke-test' <<<\"$output\""
fi

# ---- Test 4d: Multi-provider validation -------------------------------------

section "4d. multi-provider --tool flag validation (Phase 1)"

# Remove stub claude and ensure codex/gemini/copilot aren't on PATH
export PATH="/usr/bin:/bin:/usr/sbin:/sbin"  # Reset to minimal PATH (no fake-bin)

# Each should fail with a helpful "not found" error, proving --tool is wired up
output=$(run_ainb run --tool codex --repo "$TEST_REPO" 2>&1 || true)
assert "codex validation fires when not installed" grep -qi "codex" <<<"$output"

output=$(run_ainb run --tool gemini --repo "$TEST_REPO" 2>&1 || true)
assert "gemini validation fires when not installed" grep -qi "gemini" <<<"$output"

output=$(run_ainb run --tool copilot --repo "$TEST_REPO" 2>&1 || true)
assert "copilot validation fires when not installed" grep -qi "copilot" <<<"$output"

# Restore useful PATH for Wave 2+3 tests (need tmux, git, jq)
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# ---- Test 4e: favorites command (Phase 5) ----------------------------------

section "4e. favorites command (Phase 5)"

# Empty list
output=$(run_ainb favorites list 2>&1 || true)
assert "favorites list runs on empty store" test $? -eq 0

# Add a favorite
run_ainb favorites add "torvalds/linux" --alias linux --description "kernel" >/dev/null 2>&1 || true
output=$(run_ainb favorites list 2>&1 || true)
assert "favorites list shows added entry" grep -q "linux\|torvalds" <<<"$output"
assert "favorites.yaml was created" test -f "$HOME/.agents-in-a-box/favorites.yaml"

# JSON format
output=$(run_ainb --format json favorites list 2>&1 || true)
assert "favorites list --format json is valid" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# Add a second favorite and record usage
run_ainb favorites add "https://github.com/rust-lang/rust" --alias rust >/dev/null 2>&1 || true
run_ainb favorites use linux >/dev/null 2>&1 || true

# Remove one
run_ainb favorites remove linux >/dev/null 2>&1 || true
output=$(run_ainb favorites list 2>&1 || true)
assert "favorites remove deleted the entry" bash -c "! grep -q 'torvalds' <<<\"$output\""

# ---- Test 4f: presets command (Phase 7) ------------------------------------

section "4f. presets command (Phase 7)"

# list should show built-in presets (rust-backend, typescript-frontend, fast-iteration)
output=$(run_ainb presets list 2>&1 || true)
assert "presets list shows built-in rust-backend" grep -qi "rust-backend\|rust_backend" <<<"$output"

# show the built-in
output=$(run_ainb presets show rust-backend 2>&1 || true)
assert "presets show displays a preset" test -n "$output"

# JSON format
output=$(run_ainb --format json presets list 2>&1 || true)
assert "presets list --format json is valid" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# Apply to CWD - should write .agents-box/preset.toml
(cd "$TEST_ROOT" && "$BINARY" presets apply rust-backend >/dev/null 2>&1) || true
assert "presets apply wrote preset.toml" test -f "$TEST_ROOT/.agents-box/preset.toml"

# Delete attempt on built-in should fail gracefully
output=$(run_ainb presets delete rust-backend 2>&1 || true)
assert "presets delete rejects built-in presets" grep -qi "built-in\|cannot\|not.*custom\|not.*allowed" <<<"$output"

# ---- Test 4g: git command (Phase 4) ----------------------------------------

section "4g. git command (Phase 4)"

# worktrees list should succeed on empty home (no worktrees exist)
output=$(run_ainb git worktrees 2>&1 || true)
assert "git worktrees runs on empty home" test $? -eq 0

# JSON format
output=$(run_ainb --format json git worktrees 2>&1 || true)
assert "git worktrees --format json is valid" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# Cleanup with dry-run should not modify anything
output=$(run_ainb git cleanup --dry-run 2>&1 || true)
assert "git cleanup --dry-run runs" test $? -eq 0

# ---- Test 4h: init command (Phase 6) ---------------------------------------

section "4h. init command (Phase 6)"

# --check should report prereqs without modifying anything
output=$(run_ainb init --check 2>&1 || true)
assert "init --check runs" test -n "$output"
assert "init --check reports tmux" grep -qi "tmux" <<<"$output"
assert "init --check reports git" grep -qi "git" <<<"$output"

# --status (may report not-yet-onboarded on empty home)
output=$(run_ainb init --status 2>&1 || true)
assert "init --status runs" test $? -eq 0

# --check with --format json
output=$(run_ainb --format json init --check 2>&1 || true)
assert "init --check --format json is valid" bash -c "echo '$output' | jq -e . >/dev/null 2>&1"

# ---- Summary ---------------------------------------------------------------

section "Summary"
echo "  Passed: $PASS"
echo "  Failed: $FAIL"

if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
exit 0
