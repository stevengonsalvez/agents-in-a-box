#!/usr/bin/env bash
# Seed an isolated ainb and open the sessions screen with agents waiting on it.
#
# The point of the attention-surface epic is that ONE screen answers "who needs
# me", so the demo has to show more than one waiting session and an answer
# actually going out. The fixture is deliberately the same shape as
# `tripwire_sessions_answer_outcome_per_question`, which drives this exact
# path in CI: what the recording shows is a path something tests.
#
# Everything lives under a throwaway $HOME. Nothing touches the operator's own
# ~/.agents-in-a-box, and the tmux sessions it makes are named with this
# script's pid and killed by EXACT name on the way out.
#
# Usage (from the repo root, after `cargo build --release` in ainb-tui/):
#   scripts/attention-surface-demo.sh
#
# `vhs docs/assets/screenshots/attention-surface.tape` drives it.

set -euo pipefail

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
PID=$$
# Two sessions share a worktree, which is what makes an answer there refuse as
# an ambiguous target — the failure path the `ask` pane has to report honestly.
SHARED_A="demo_attention_shared_a_$PID"
SHARED_B="demo_attention_shared_b_$PID"
SOLO="demo_attention_solo_$PID"

cleanup() {
  for s in "$SHARED_A" "$SHARED_B" "$SOLO"; do
    # EXACT name. A bare `-t name` is a PREFIX match in tmux and would take
    # sessions this script never created.
    tmux kill-session -t "=$s" 2>/dev/null || true
  done
  rm -rf "$DEMO_HOME"
}
trap cleanup EXIT

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
# NOT `exec`. Replacing this shell would discard the EXIT trap with it, and the
# three tmux sessions above would outlive every run — which is exactly what the
# first two recordings left behind, cluttering the third with their leftovers.
"$AINB_BIN" tui
