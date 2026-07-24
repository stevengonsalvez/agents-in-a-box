#!/usr/bin/env bash
# gap-3 §3.6 acceptance: sub-issue create (`s`) + keyboard mark-done (`d`) in the
# REAL Hangar TUI, asserting BOTH the rendered screen AND the sqlite side-effect.
#
# Flow (drives the live TUI in a tmux pane against an isolated hangar.db):
#   1. open Hangar (`g`), create a PARENT issue,
#   2. press `s`  -> the create wizard opens with a "Sub-issue of <HGR-n title>"
#      banner (SCREEN assertion #1); fill + create the CHILD,
#      -> child.parent_issue_id == parent.id in sqlite (STORE assertion #1),
#      -> the parent card shows the `⊟ 0/1` roll-up badge (SCREEN assertion #2),
#   3. select the child, press `d` -> the child moves to the Done column and the
#      parent badge flips to `⊟ 1/1` (SCREEN assertion #3),
#      -> child.state == 'done' AND a roll-up comment
#         ("… is done. 1/1 sub-issues complete.") lands on the parent in sqlite
#         (STORE assertions #2 + #3, i.e. the child-done -> parent cascade).
#
# NOTE ON KEYS: the feature is LOWERCASE `s` / `d`, NOT `S` / `D`. Uppercase
# `S` (Squads) and `D` (Daemon) are host-reserved GLOBAL tab-switches consumed by
# the plugin router (`routing_event`) before the issue-list reducer ever sees
# them — the same global-router-steals-keys class as the #450 squad-hotkey bug.
# Lowercase falls through to the reducer.
#
# Follows the `tmux-ui-tripwire` skill rules: skip gracefully without tmux/sqlite,
# poll (never bare-sleep past a fixed settle), kill the session by EXACT name.
#
# Usage:
#   scripts/verify-hangar-subissue.sh [path-to-ainb-binary]
# The binary defaults to target/release/ainb. The Hangar plugin MUST be staged
# (./scripts/build-plugins.sh --release) so the TUI can discover it from dist/.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/ainb}"

if ! command -v tmux >/dev/null 2>&1; then
    echo "SKIP: tmux unavailable"; exit 0
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "SKIP: sqlite3 unavailable"; exit 0
fi
if [[ ! -x "$BIN" ]]; then
    echo "FAIL: ainb binary not found/executable at $BIN" >&2; exit 1
fi
if [[ ! -d "$ROOT/dist/plugins/hangar-tui" ]]; then
    echo "FAIL: hangar-tui plugin not staged — run ./scripts/build-plugins.sh --release" >&2
    exit 1
fi

HOME_DIR="$(mktemp -d)"
DB="$HOME_DIR/hangar.db"
SESS="verify-subissue-$$"

cleanup() {
    tmux kill-session -t "$SESS" 2>/dev/null || true
    rm -rf "$HOME_DIR"
}
trap cleanup EXIT

fail() { echo "FAIL: $1" >&2; echo "---- final pane ----" >&2; tmux capture-pane -t "$SESS" -p >&2 2>/dev/null || true; exit 1; }

# Poll the pane until $2 (a grep -qE pattern) appears, up to $3 seconds.
wait_pane() {
    local sess="$1" pat="$2" secs="$3" i=0
    while (( i < secs * 2 )); do
        if tmux capture-pane -t "$sess" -p | grep -qE "$pat"; then return 0; fi
        sleep 0.5; (( i++ ))
    done
    return 1
}
# Poll sqlite until $1 (a query) returns $2.
wait_sql() {
    local query="$1" want="$2" secs="$3" i=0 got
    while (( i < secs * 2 )); do
        got="$(sqlite3 "$DB" "$query" 2>/dev/null || true)"
        if [[ "$got" == "$want" ]]; then return 0; fi
        sleep 0.5; (( i++ ))
    done
    echo "  (last sqlite value: '${got:-<none>}', wanted '$want')" >&2
    return 1
}

# Launch the TUI from the workspace root so it discovers dist/plugins/.
tmux new-session -d -s "$SESS" -x 200 -y 50 -c "$ROOT" \
    "AINB_HANGAR_HOME=$HOME_DIR HOME=$HOME_DIR $BIN"
sleep 5

# Skip the first-run setup wizard, open Hangar (`g`), ack the danger modal.
tmux send-keys -t "$SESS" Escape; sleep 1
tmux send-keys -t "$SESS" Escape; sleep 1
tmux send-keys -t "$SESS" g; sleep 1
wait_pane "$SESS" "Backlog|hangar-tui unavailable" 15 || fail "Hangar screen never rendered"
if tmux capture-pane -t "$SESS" -p | grep -q "hangar-tui unavailable"; then
    fail "hangar-tui plugin not loaded (staging/discovery problem)"
fi
tmux send-keys -t "$SESS" y; sleep 2   # dismiss danger-full-access modal (idempotent)

# --- 1. create the PARENT --------------------------------------------------
tmux send-keys -t "$SESS" c; sleep 2
tmux send-keys -t "$SESS" "Parent epic"; sleep 1
tmux send-keys -t "$SESS" Down Down Down; sleep 1   # Title -> Repo
tmux send-keys -t "$SESS" "@"; sleep 1              # open repo dropdown
tmux send-keys -t "$SESS" Enter; sleep 1           # pick scratch
tmux send-keys -t "$SESS" Enter; sleep 2           # create
wait_sql "SELECT title FROM issue WHERE title='Parent epic';" "Parent epic" 15 \
    || fail "parent issue was not created"
echo "PASS: parent issue created"

# --- 2. press `s` -> sub-issue wizard with banner --------------------------
tmux send-keys -t "$SESS" s; sleep 2
wait_pane "$SESS" "Sub-issue of .* Parent epic" 8 \
    || fail "'s' did not open a sub-issue wizard with the 'Sub-issue of …' banner"
echo "PASS: SCREEN — 's' opens wizard with 'Sub-issue of … Parent epic' banner"

# fill + create the CHILD
tmux send-keys -t "$SESS" "Child one"; sleep 1
tmux send-keys -t "$SESS" Down Down Down; sleep 1
tmux send-keys -t "$SESS" "@"; sleep 1
tmux send-keys -t "$SESS" Enter; sleep 1
tmux send-keys -t "$SESS" Enter; sleep 2
wait_sql "SELECT title FROM issue WHERE title='Child one';" "Child one" 15 \
    || fail "child issue was not created"

# STORE #1: child.parent_issue_id == parent.id
wait_sql "SELECT CASE WHEN (SELECT parent_issue_id FROM issue WHERE title='Child one')=(SELECT id FROM issue WHERE title='Parent epic') THEN 'ok' ELSE 'no' END;" "ok" 10 \
    || fail "child was not linked to the parent (parent_issue_id mismatch)"
echo "PASS: STORE — child.parent_issue_id == parent.id"

# SCREEN #2: parent card shows the 0/1 roll-up badge
wait_pane "$SESS" "0/1" 8 || fail "parent card did not show the '⊟ 0/1' sub-issue badge"
echo "PASS: SCREEN — parent card shows ⊟ 0/1 badge"

# --- 3. select the child, press `d` -> done + cascade ----------------------
tmux send-keys -t "$SESS" j; sleep 1   # move selection to the child card
tmux send-keys -t "$SESS" d; sleep 3   # mark done (fires issue_update + cascade)

# STORE #2: child.state == done
wait_sql "SELECT state FROM issue WHERE title='Child one';" "done" 15 \
    || fail "'d' did not move the child to done in the store"
echo "PASS: STORE — child.state == done after 'd'"

# STORE #3: the child-done -> parent cascade posted a roll-up comment on the parent
wait_sql "SELECT CASE WHEN EXISTS(SELECT 1 FROM comment WHERE issue_id=(SELECT id FROM issue WHERE title='Parent epic') AND body LIKE '%sub-issues complete%') THEN 'ok' ELSE 'no' END;" "ok" 15 \
    || fail "no child-done roll-up comment cascaded onto the parent"
echo "PASS: STORE — parent has the '1/1 sub-issues complete' cascade comment"

# SCREEN #3: the parent card badge flipped to 1/1 and the child sits in Done
wait_pane "$SESS" "1/1" 8 || fail "parent card badge did not flip to '⊟ 1/1'"
echo "PASS: SCREEN — parent badge flipped to ⊟ 1/1"

echo
echo "ALL PASS: gap-3 §3.6 sub-issue (s) + mark-done (d) verified end-to-end (screen + sqlite)."
