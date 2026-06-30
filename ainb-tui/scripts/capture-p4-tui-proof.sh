#!/usr/bin/env bash
# Capture an asciinema proof of the P4 Hangar TUI rendering live daemon data.
#
# Stands the exact P4.9 tripwire pipeline by hand (isolated $HOME, CLI-seeded
# issues on the `default` workspace, real ainb-hangar-daemon serving the unix
# socket), launches `ainb tui` in a tmux session, opens the Hangar screen with
# `g`, and walks the tabs (1 Issues -> 4 Skills -> , Settings -> 1 Issues) while
# asciinema records the attached pane.
#
# HARD RULES kept from tmux-ui-tripwire: exact-name kill only, single-char nav
# without Enter, poll-not-bare-sleep for the readiness gate, two distinct
# sessions never touched by wildcard.
set -euo pipefail

WS="$(cd "$(dirname "$0")/.." && pwd)"            # ainb-tui/
ROOT="$(cd "$WS/.." && pwd)"                        # worktree root
AINB="$WS/target/debug/ainb"
DAEMON="$WS/target/debug/ainb-hangar-daemon"
PLUGIN_ROOT="$WS/dist/plugins"
OUT="$ROOT/docs/hangar/proofs/p4-tui.cast"
SESSION="hangar-p4-proof-$$"

for b in "$AINB" "$DAEMON" "$PLUGIN_ROOT/hangar-tui/hangar-tui"; do
  [ -e "$b" ] || { echo "MISSING: $b (build + just stage-plugins first)"; exit 1; }
done
command -v asciinema >/dev/null || { echo "asciinema not installed"; exit 1; }
command -v tmux >/dev/null || { echo "tmux not installed"; exit 1; }

HOME_DIR="$(mktemp -d)"
cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$HOME_DIR"
}
trap cleanup EXIT

mkdir -p "$HOME_DIR/.agents-in-a-box" "$HOME_DIR/.agents-in-a-box/config"
# Skip onboarding wizard (only major version is gated; ainb is 1.x).
cat > "$HOME_DIR/.agents-in-a-box/config/onboarding.toml" <<'TOML'
completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "1.1.0"
skipped_dependencies = []
git_directories = []
TOML

# Seed real issues on the `default` workspace via the CLI (auto-bootstraps ws+db).
seed_issue() { HOME="$HOME_DIR" "$AINB" hangar issue create --title "$1" >/dev/null; }
seed_issue "Refactor API surface for v2"
seed_issue "Wire up payments webhook"
seed_issue "Tighten retry backoff on task FSM"

# Spawn the daemon under the same $HOME (binds $HOME/.agents-in-a-box/hangar.sock).
HOME="$HOME_DIR" HANGAR_DAEMON_DISABLE_CLAIM=1 "$DAEMON" >/dev/null 2>&1 &
DAEMON_PID=$!
SOCK="$HOME_DIR/.agents-in-a-box/hangar.sock"
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done
[ -S "$SOCK" ] || { echo "daemon socket never appeared"; exit 1; }

# Launch ainb tui in a detached tmux session.
tmux new-session -d -s "$SESSION" -x 160 -y 44
tmux send-keys -t "$SESSION" "HOME='$HOME_DIR' AINB_PLUGIN_ROOT='$PLUGIN_ROOT' exec '$AINB' tui" Enter

# Background key-driver: wait for the home screen, open Hangar, walk the tabs,
# then kill the session (which makes `tmux attach` — and asciinema — exit).
(
  # Poll for the interactive home footer before sending nav keys.
  for _ in $(seq 1 90); do
    tmux capture-pane -p -t "$SESSION" 2>/dev/null | grep -q "navigate" && break
    sleep 0.5
  done
  sleep 1
  # Open Hangar (re-send `g` until the issue list lands).
  for _ in $(seq 1 20); do
    tmux send-keys -t "$SESSION" g
    tmux capture-pane -p -t "$SESSION" 2>/dev/null | grep -q "Refactor API" && break
    sleep 1.2
  done
  sleep 3
  tmux send-keys -t "$SESSION" 1; sleep 3   # Issues
  tmux send-keys -t "$SESSION" 4; sleep 3   # Skills
  tmux send-keys -t "$SESSION" ,; sleep 3   # Settings
  tmux send-keys -t "$SESSION" 1; sleep 3   # back to Issues
  sleep 1
  tmux kill-session -t "$SESSION" 2>/dev/null || true
) &
DRIVER_PID=$!

mkdir -p "$(dirname "$OUT")"
asciinema rec --overwrite -y -c "tmux attach -t $SESSION" "$OUT" || true
wait "$DRIVER_PID" 2>/dev/null || true

echo "WROTE: $OUT"
ls -la "$OUT"
