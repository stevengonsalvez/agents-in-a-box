#!/usr/bin/env bash
# Seed an isolated ainb and open the sessions screen with agents waiting on it.
#
# The point of the attention-surface epic is that ONE screen answers "who needs
# me", so the demo has to show more than one waiting session and an answer
# actually going out. The fixture is deliberately the same shape as
# `tripwire_sessions_answer_outcome_per_question`, which drives this exact
# path in CI: what the recording shows is a path something tests.
#
# Everything lives under a throwaway $HOME, and the demo runs against its own
# PRIVATE tmux server via $TMUX_TMPDIR. That second part is not tidiness: the
# sessions screen lists every tmux session on the host it cannot account for,
# under "Other tmux", so a recording made against the shared server publishes
# the operator's real session names. A private server can only ever show the
# three this script made.
#
# Nothing touches the operator's own ~/.agents-in-a-box, and the sessions are
# killed by EXACT name on the way out.
#
# Usage (from the repo root; a debug build is enough):
#   scripts/attention-surface-demo.sh              # no daemon
#   scripts/attention-surface-demo.sh --daemon     # with a hangar daemon
#
# WITHOUT `--daemon` is the interesting default: the chips come from the local
# notifyd producer and the answer rides the tmux send path, which is the half
# that has to hold the surface up when the daemon is down. Verbs that are
# daemon-OWNED (broadcast, the copilot tab) need `--daemon`, and say so
# honestly on screen when they do not have one.
#
# The daemon is brought up under its own $AINB_HANGAR_HOME inside the same
# throwaway directory, so it never touches ~/.agents-in-a-box, and it is
# stopped by that same home on the way out.
#
# The tapes in docs/assets/screenshots/attention-*.tape drive it.

set -euo pipefail

WITH_DAEMON=0
[[ ${1:-} == "--daemon" ]] && WITH_DAEMON=1

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# Release when it is there, debug otherwise. The tape's sleeps are sized for a
# cold debug start, so either produces the same recording.
if [[ -z ${AINB_BIN:-} ]]; then
  for candidate in "$ROOT/ainb-tui/target/release/ainb" "$ROOT/ainb-tui/target/debug/ainb"; do
    [[ -x $candidate ]] && AINB_BIN=$candidate && break
  done
fi
if [[ -z ${AINB_BIN:-} || ! -x $AINB_BIN ]]; then
  echo "no ainb binary — build one in ainb-tui/ first (debug is enough)" >&2
  exit 1
fi

DEMO_HOME=$(mktemp -d "${TMPDIR:-/tmp}/ainb-attention-demo.XXXXXX")
BASE="$DEMO_HOME/.agents-in-a-box"
HANGAR="$DEMO_HOME/hangar"
# The private tmux server. Exported BEFORE the first tmux call so the sessions
# this script creates and every tmux call the TUI makes land on the same
# private socket, and the host's own sessions are invisible to both.
# SHORT, and deliberately not under $DEMO_HOME: a unix socket path is capped
# around 104 bytes, and `$TMPDIR/ainb-attention-demo.XXXXXX/tmux/tmux-501/default`
# is already past it — tmux fails with "File name too long" and the demo dies
# before it draws anything.
TMUX_SOCKET_DIR=$(mktemp -d /tmp/ainbd.XXXX)
export TMUX_TMPDIR="$TMUX_SOCKET_DIR"
# A tmux CLIENT prefers the server named by $TMUX over $TMUX_TMPDIR, so running
# this from inside a tmux session would silently rejoin the host's server and
# put its real session names back on screen. Unset so both this script and the
# TUI resolve to the private socket.
unset TMUX TMUX_PANE
PID=$$
# Two sessions share a worktree, which is what makes an answer there refuse as
# an ambiguous target — the failure path the `ask` pane has to report honestly.
# The `tmux_` prefix is what marks a session as ainb-MANAGED: without it the
# rows are listed a second time under "Other tmux", which reads as six sessions
# when there are three.
SHARED_A="tmux_demo_payments_a_$PID"
SHARED_B="tmux_demo_payments_b_$PID"
SOLO="tmux_demo_billing_$PID"

cleanup() {
  if [[ $WITH_DAEMON == 1 ]]; then
    # Addressed by the SAME isolated home it was started under, so this can
    # never signal the operator's own daemon.
    HOME="$DEMO_HOME" AINB_HANGAR_HOME="$HANGAR" "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true
  fi
  for s in "$SHARED_A" "$SHARED_B" "$SOLO"; do
    # EXACT name. A bare `-t name` is a PREFIX match in tmux and would take
    # sessions this script never created. These live on the private server, so
    # $TMUX_TMPDIR must still be set for this to address them at all.
    tmux kill-session -t "=$s" 2>/dev/null || true
  done
  rm -rf "$DEMO_HOME" "${TMUX_SOCKET_DIR:-}"
}
# INT/TERM as well as EXIT: a recorder that kills the terminal rather than
# letting the TUI quit would otherwise skip cleanup and leave the daemon
# running against its temp home, which is exactly what a first smoke test of
# this script did.
trap cleanup EXIT INT TERM HUP

mkdir -p "$BASE/config"
# The version has to be THIS binary's. A stale one re-runs the setup wizard,
# which is what the first recording of this caught: the tape drove a wizard
# instead of the sessions screen.
AINB_VERSION=$("$AINB_BIN" --version | awk '{print $2}')
cat >"$BASE/config/onboarding.toml" <<TOML
completed = true
completed_at = "2026-09-04T00:00:00+00:00"
version = "$AINB_VERSION"
skipped_dependencies = []
git_directories = []
TOML
printf '%s\n' \
  '{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}' \
  >"$BASE/install.json"

seed_repo() {
  local dir=$1
  mkdir -p "$dir"
  git -C "$dir" init --quiet --initial-branch=main
  printf 'demo\n' >"$dir/README.md"
  git -C "$dir" add README.md
  git -C "$dir" -c user.name=demo -c user.email=demo@example.invalid \
    -c commit.gpgsign=false commit --quiet -m seed
}

SHARED_TREE="$DEMO_HOME/payments-api"
SOLO_TREE="$DEMO_HOME/billing-worker"
seed_repo "$SHARED_TREE"
seed_repo "$SOLO_TREE"

# Every session must be LIVE: discovery filters on `is_running`, and a session
# it drops never reaches the screen to be answered.
for s in "$SHARED_A" "$SHARED_B" "$SOLO"; do
  tmux new-session -d -s "$s" -x 200 -y 50 "sh -c cat"
done

cat >"$BASE/sessions.json" <<JSON
{
  "sessions": {
    "$SHARED_A": {
      "session_id": "6f1f5f7e-0000-4000-8000-00000000ae01",
      "tmux_session_name": "$SHARED_A",
      "worktree_path": "$SHARED_TREE",
      "workspace_name": "payments-api",
      "created_at": "2026-09-04T00:00:00Z",
      "agent_type": "Claude",
      "skip_permissions": true
    },
    "$SHARED_B": {
      "session_id": "6f1f5f7e-0000-4000-8000-00000000ae02",
      "tmux_session_name": "$SHARED_B",
      "worktree_path": "$SHARED_TREE",
      "workspace_name": "payments-api",
      "created_at": "2026-09-04T00:00:00Z",
      "agent_type": "Claude",
      "skip_permissions": true
    },
    "$SOLO": {
      "session_id": "6f1f5f7e-0000-4000-8000-00000000ae03",
      "tmux_session_name": "$SOLO",
      "worktree_path": "$SOLO_TREE",
      "workspace_name": "billing-worker",
      "created_at": "2026-09-04T00:00:00Z",
      "agent_type": "Claude",
      "skip_permissions": true
    }
  }
}
JSON

# The waiting agents. This is the notifyd store the LOCAL producer reads, which
# is the half that keeps the surface working with no hangar daemon running at
# all — the case the whole epic is judged on.
NOW_MS=$(( $(date +%s) * 1000 - 90000 ))
sqlite3 "$BASE/notifications.db" <<SQL
CREATE TABLE IF NOT EXISTS notifications (
    id          TEXT PRIMARY KEY,
    ts          INTEGER NOT NULL,
    agent       TEXT NOT NULL,
    session_id  TEXT NOT NULL DEFAULT '',
    cwd         TEXT NOT NULL DEFAULT '',
    project     TEXT NOT NULL DEFAULT '',
    raw_event   TEXT NOT NULL,
    payload     TEXT NOT NULL DEFAULT '{}',
    read        INTEGER NOT NULL DEFAULT 0,
    dismissed   INTEGER NOT NULL DEFAULT 0
);
INSERT OR REPLACE INTO notifications
  (id, ts, agent, session_id, cwd, project, raw_event, payload)
VALUES
  ('demo-shared', $NOW_MS, 'claude', 'demo-shared', '$SHARED_TREE',
   'payments-api', 'Notification:idle_prompt',
   '{"message":"Which sqlite path should the ledger use?"}'),
  ('demo-solo', $((NOW_MS + 20000)), 'claude', 'demo-solo', '$SOLO_TREE',
   'billing-worker', 'Notification:idle_prompt',
   '{"message":"Rebase onto main, or merge?"}');
SQL

export HOME="$DEMO_HOME"
export AINB_DISABLE_PLUGINS=1

if [[ $WITH_DAEMON == 1 ]]; then
  mkdir -p "$HANGAR"
  # The daemon reads its own home, and an undismissed prompt would sit over the
  # screen being recorded.
  printf '%s\n' '{"agents":[],"hook_script":"","prompt_dismissed":true}' >"$HANGAR/install.json"
  export AINB_HANGAR_HOME="$HANGAR"
  # Store, socket-auth token and the background daemon, in one verb.
  "$AINB_BIN" hangar daemon setup >/dev/null 2>&1 || {
    echo "hangar daemon setup failed" >&2
    exit 1
  }
  # Let the socket accept before the TUI dials it, so a daemon-owned pane opens
  # live rather than painting UNAVAILABLE first.
  for _ in $(seq 1 40); do
    "$AINB_BIN" hangar daemon status >/dev/null 2>&1 && break
    sleep 0.25
  done
fi
# NOT `exec`. Replacing this shell would discard the EXIT trap with it, and the
# three tmux sessions above would outlive every run — which is exactly what the
# first two recordings left behind, cluttering the third with their leftovers.
"$AINB_BIN" tui
