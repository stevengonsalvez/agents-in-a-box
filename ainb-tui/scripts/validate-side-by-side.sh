#!/usr/bin/env bash
# Phase 6 final validation: side-by-side tmux capture proving the
# plugin pipeline materially changes user-visible behaviour.
#
# Left pane:  AINB_PLUGIN_ROOT=dist/plugins ./ainb  (burndown + session-reader loaded)
# Right pane: AINB_DISABLE_PLUGINS=1     ./ainb     (host comes up empty)
#
# For each pane:
#   1. Land the home screen.
#   2. Press `i` (HomeScreen V2 -> GoToStats).
#   3. capture-pane and save under /tmp/ainb-side-by-side-<ts>/.
#
# Also runs the CLI variant as a deterministic byte-level proof:
#   ainb usage report --format=json with plugins  -> exit 0 + JSON
#   ainb usage report --format=json no plugins    -> exit 2 + actionable stderr
#
# Cleans up the tmux session by EXACT name. Never kill-server.

set -euo pipefail

WORKSPACE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$WORKSPACE"

BIN="$WORKSPACE/target/release/ainb"
DIST="$WORKSPACE/dist/plugins"
FIXTURE_SRC="$WORKSPACE/crates/ainb-core/tests/fixtures/sessions/claude_projects"

[[ -x "$BIN" ]] || { echo "missing $BIN — run cargo build --release -p ainb"; exit 1; }
[[ -d "$DIST/burndown" && -d "$DIST/session-reader" ]] || { echo "missing dist/plugins/{burndown,session-reader} — run scripts/build-plugins.sh"; exit 1; }
[[ -d "$FIXTURE_SRC" ]] || { echo "missing fixture sessions at $FIXTURE_SRC"; exit 1; }

TS=$(date +%s)
ARTIFACT_DIR="/tmp/ainb-side-by-side-$TS"
mkdir -p "$ARTIFACT_DIR"
echo "[validate] artifacts -> $ARTIFACT_DIR"

# Build a deterministic fixture HOME for both panes (same fixture, two
# isolated tempdirs so plugin caches don't cross-contaminate).
make_fixture_home() {
    local out="$1"
    mkdir -p "$out/.claude/projects" "$out/.agents-in-a-box/config"
    cp -R "$FIXTURE_SRC/." "$out/.claude/projects/"
    local pkg_ver
    pkg_ver=$(grep -m1 '^version' "$WORKSPACE/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/' | tr -d '[:space:]')
    cat > "$out/.agents-in-a-box/config/onboarding.toml" <<EOF
completed = true
completed_at = "2026-05-09T00:00:00Z"
version = "${pkg_ver}"
EOF
}

LEFT_HOME="$ARTIFACT_DIR/home-with-plugins"
RIGHT_HOME="$ARTIFACT_DIR/home-without-plugins"
make_fixture_home "$LEFT_HOME"
make_fixture_home "$RIGHT_HOME"

# ----------------------------- CLI proof --------------------------------
echo "[validate] CLI side-by-side..."

CLI_LEFT_OUT="$ARTIFACT_DIR/cli-with-plugins.json"
CLI_RIGHT_STDERR="$ARTIFACT_DIR/cli-without-plugins.stderr"

set +e
HOME="$LEFT_HOME" AINB_HOME="$LEFT_HOME" AINB_PLUGIN_ROOT="$DIST" \
    "$BIN" usage report --format=json > "$CLI_LEFT_OUT" 2>/dev/null
LEFT_EXIT=$?

AINB_DISABLE_PLUGINS=1 "$BIN" usage report --format=json \
    >/dev/null 2> "$CLI_RIGHT_STDERR"
RIGHT_EXIT=$?
set -e

if [[ $LEFT_EXIT -ne 0 ]]; then
    echo "[FAIL] CLI with plugins exited $LEFT_EXIT (expected 0)"
    exit 1
fi
if [[ $RIGHT_EXIT -ne 2 ]]; then
    echo "[FAIL] CLI without plugins exited $RIGHT_EXIT (expected 2)"
    exit 1
fi
if ! grep -q "requires the burndown plugin" "$CLI_RIGHT_STDERR"; then
    echo "[FAIL] disabled-plugin stderr missing 'requires the burndown plugin'"
    cat "$CLI_RIGHT_STDERR"
    exit 1
fi
LEFT_BYTES=$(wc -c < "$CLI_LEFT_OUT" | tr -d '[:space:]')
echo "[ok] CLI with plugins:    exit 0, $LEFT_BYTES bytes JSON"
echo "[ok] CLI without plugins: exit 2, actionable stderr"

# ----------------------------- TUI tmux proof ---------------------------
echo "[validate] TUI tmux side-by-side..."

command -v tmux >/dev/null || { echo "tmux not on PATH — install via brew install tmux"; exit 1; }

SESSION="ainb-side-by-side-$TS"

# RAII cleanup. Never kill-server. Never wildcard.
cleanup() {
    if tmux has-session -t "$SESSION" 2>/dev/null; then
        echo "[validate] killing tmux session $SESSION"
        tmux kill-session -t "$SESSION" || true
    fi
}
trap cleanup EXIT

LEFT_CMD="HOME='$LEFT_HOME' AINB_HOME='$LEFT_HOME' AINB_PLUGIN_ROOT='$DIST' '$BIN'"
RIGHT_CMD="HOME='$RIGHT_HOME' AINB_HOME='$RIGHT_HOME' AINB_DISABLE_PLUGINS=1 '$BIN'"

# Two side-by-side panes, 240x40 total -> 120x40 each.
tmux new-session -d -s "$SESSION" -x 240 -y 40 "$LEFT_CMD"
tmux split-window -h -t "$SESSION" "$RIGHT_CMD"
tmux select-pane -t "${SESSION}:0.0"

wait_for_pane() {
    local pane=$1
    local pattern=$2
    local label=$3
    local deadline=$((SECONDS + 20))
    while (( SECONDS < deadline )); do
        if tmux capture-pane -t "${SESSION}:0.${pane}" -p | grep -q "$pattern"; then
            return 0
        fi
        sleep 0.3
    done
    echo "[FAIL] timed out waiting for '$label' in pane $pane"
    tmux capture-pane -t "${SESSION}:0.${pane}" -p > "$ARTIFACT_DIR/timeout-pane-$pane.txt"
    return 1
}

echo "[validate] waiting for both home screens..."
wait_for_pane 0 "Stats" "left home screen" || true
wait_for_pane 1 "Stats" "right home screen" || true

# Snapshot home-screen state before nav.
tmux capture-pane -t "${SESSION}:0.0" -p > "$ARTIFACT_DIR/01-left-home.txt"
tmux capture-pane -t "${SESSION}:0.1" -p > "$ARTIFACT_DIR/01-right-home.txt"

# Settle then navigate to Analytics in both panes.
sleep 0.5
tmux send-keys -t "${SESSION}:0.0" "i"
tmux send-keys -t "${SESSION}:0.1" "i"

echo "[validate] waiting for analytics screens..."
sleep 2

# Capture analytics-state snapshots.
tmux capture-pane -t "${SESSION}:0.0" -p > "$ARTIFACT_DIR/02-left-analytics.txt"
tmux capture-pane -t "${SESSION}:0.1" -p > "$ARTIFACT_DIR/02-right-analytics.txt"

# Combined side-by-side capture (both panes in one frame).
tmux capture-pane -t "${SESSION}:0.0" -p > "$ARTIFACT_DIR/03-combined-LEFT.txt"
tmux capture-pane -t "${SESSION}:0.1" -p > "$ARTIFACT_DIR/03-combined-RIGHT.txt"

# Quit cleanly.
tmux send-keys -t "${SESSION}:0.0" "q"
tmux send-keys -t "${SESSION}:0.1" "q"
sleep 0.5

# ---- assertions on captures ----
echo
echo "[validate] asserting visual differences..."

LEFT_ANALYTICS="$(cat "$ARTIFACT_DIR/02-left-analytics.txt")"
RIGHT_ANALYTICS="$(cat "$ARTIFACT_DIR/02-right-analytics.txt")"

# LEFT must show "Usage Analytics" header (burndown plugin is rendering).
if ! grep -q "Usage Analytics" <<< "$LEFT_ANALYTICS"; then
    echo "[FAIL] left pane (with plugins) didn't reach the analytics screen"
    exit 1
fi

# RIGHT must NOT show analytics chrome from the plugin — either the
# placeholder fallback (PluginScreen renders it when plugin missing) or
# the home screen still shown if `i` did nothing.
RIGHT_HAS_PLUGIN_ANALYTICS=0
if grep -q "Usage Analytics" <<< "$RIGHT_ANALYTICS" \
   && grep -q "Total Calls\|By Project" <<< "$RIGHT_ANALYTICS"; then
    RIGHT_HAS_PLUGIN_ANALYTICS=1
fi
if (( RIGHT_HAS_PLUGIN_ANALYTICS == 1 )); then
    echo "[FAIL] right pane (no plugins) is rendering plugin analytics — disable flag broken"
    exit 1
fi

echo "[ok] left pane:  'Usage Analytics' present (plugin rendered)"
echo "[ok] right pane: no plugin analytics body (plugin disabled)"
echo
echo "==================================================================="
echo "Side-by-side validation passed."
echo "Artifacts: $ARTIFACT_DIR"
echo "  CLI proof:"
echo "    cli-with-plugins.json     ($LEFT_BYTES bytes)"
echo "    cli-without-plugins.stderr"
echo "  TUI proof:"
echo "    01-{left,right}-home.txt"
echo "    02-{left,right}-analytics.txt"
echo "==================================================================="
