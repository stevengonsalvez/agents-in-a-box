#!/usr/bin/env bash
# Full live Claude proof: AskUserQuestion -> Fleet CLI -> hook broker -> Claude receipt.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
ainb_bin="${AINB_BIN:-$repo_root/ainb-tui/target/debug/ainb}"
daemon_bin="${HANGAR_DAEMON_BIN:-$repo_root/ainb-tui/target/debug/ainb-hangar-daemon}"
hook_script="${AINB_HOOK_SCRIPT:-$HOME/.agents-in-a-box/hooks/notify.sh}"
proof_root="${FLEET_PROOF_ROOT:-$(mktemp -d /tmp/fleet-live-ask.XXXXXX)}"
hangar_home="$proof_root/hangar-home"
agent_home="$proof_root/agent-home"
ainb_home="$agent_home/.agents-in-a-box"
tmux_session="fleet-live-ask-$(date +%s)-$$"
daemon_session="fleet-live-daemon-$(date +%s)-$$"
broker_session="fleet-live-broker-$(date +%s)-$$"
parent_key="fleet-live-proof-$(date +%s)-$$"
events_file="$ainb_home/events.jsonl"

for required in "$ainb_bin" "$daemon_bin" "$hook_script"; do
  if [ ! -x "$required" ]; then
    printf 'missing executable: %s\n' "$required" >&2
    exit 1
  fi
done

cleanup() {
  tmux has-session -t "$tmux_session" 2>/dev/null && tmux kill-session -t "$tmux_session"
  tmux has-session -t "$daemon_session" 2>/dev/null && tmux kill-session -t "$daemon_session"
  tmux has-session -t "$broker_session" 2>/dev/null && tmux kill-session -t "$broker_session"
  printf 'proof artifacts: %s\n' "$proof_root"
}
trap cleanup EXIT

wait_for_tmux_shell() {
  local session="$1"
  for _ in $(seq 1 30); do
    if tmux capture-pane -t "$session" -p | grep -q 'Terminal Ready'; then
      return 0
    fi
    sleep 1
  done
  tmux capture-pane -t "$session" -p
  printf 'tmux shell did not become ready: %s\n' "$session" >&2
  return 1
}

mkdir -p "$hangar_home/hangar" "$ainb_home"
if [ -f "$events_file" ]; then
  stat -f '%z' "$events_file" > "$hangar_home/hangar/attention_ingest.offset"
else
  printf '0' > "$hangar_home/hangar/attention_ingest.offset"
fi

tmux new-session -d -s "$daemon_session" -x 180 -y 50
wait_for_tmux_shell "$daemon_session"
tmux send-keys -t "$daemon_session" \
  "HOME='$agent_home' AINB_HANGAR_HOME='$hangar_home' AINB_CODEX_MANAGED=0 '$daemon_bin' 2>&1 | tee '$proof_root/daemon.log'" Enter

tmux new-session -d -s "$broker_session" -x 180 -y 50
wait_for_tmux_shell "$broker_session"
tmux send-keys -t "$broker_session" \
  "HOME='$agent_home' AINB_HOME='$ainb_home' '$ainb_bin' notifyd run 2>&1 | tee '$proof_root/notifyd.log'" Enter

for _ in $(seq 1 30); do
  if [ -S "$ainb_home/approve.sock" ]; then
    break
  fi
  sleep 1
done
if [ ! -S "$ainb_home/approve.sock" ]; then
  tmux capture-pane -t "$broker_session" -p
  printf 'Current notifyd did not bind its structured-answer broker\n' >&2
  exit 1
fi

for _ in $(seq 1 30); do
  if [ -S "$hangar_home/hangar/hangar.sock" ] || [ -S "$hangar_home/hangar.sock" ]; then
    break
  fi
  sleep 1
done
if [ ! -S "$hangar_home/hangar/hangar.sock" ] && [ ! -S "$hangar_home/hangar.sock" ]; then
  tmux capture-pane -t "$daemon_session" -p
  printf 'Hangar daemon did not bind its socket\n' >&2
  exit 1
fi

hook_command="HOME=$agent_home AINB_HOME=$ainb_home PATH=$(dirname "$ainb_bin"):\$PATH AINB_HOOK_EVENT=PreToolUse AINB_HOOK_MATCHER=AskUserQuestion AINB_MANAGED=atc $hook_script"
settings="$(jq -cn --arg command "$hook_command" '{hooks:{PreToolUse:[{matcher:"AskUserQuestion",hooks:[{type:"command",command:$command,timeout:660}]}]}}')"

tmux new-session -d -s "$tmux_session" -x 180 -y 50
wait_for_tmux_shell "$tmux_session"
tmux send-keys -t "$tmux_session" \
  "cd '$repo_root' && AINB_PARENT_SESSION='$parent_key' claude --settings '$settings' --permission-mode bypassPermissions" Enter

prompt='Use AskUserQuestion exactly once now. Header: Fleet proof. Question: Does Fleet inject structured answer? Options: Yes, No. After receiving answer, respond exactly ANSWER_RECEIVED:<chosen option>. Do not call any other tool.'
for _ in $(seq 1 45); do
  pane="$(tmux capture-pane -t "$tmux_session" -p)"
  if printf '%s' "$pane" | grep -Eq 'Claude Code|Tips for getting started|What can I help'; then
    break
  fi
  sleep 1
done
tmux send-keys -t "$tmux_session" -l -- "$prompt"
tmux send-keys -t "$tmux_session" Enter
for _ in $(seq 1 12); do
  pane="$(tmux capture-pane -t "$tmux_session" -p)"
  if printf '%s' "$pane" | grep -Eq 'Choreographing|Cultivating|Thinking|Working'; then
    break
  fi
  tmux send-keys -t "$tmux_session" Enter
  sleep 1
done

ask_json="$proof_root/ask.json"
for _ in $(seq 1 90); do
  AINB_HANGAR_HOME="$hangar_home" "$ainb_bin" fleet ask --format json > "$ask_json" 2> "$proof_root/ask.err" || true
  question_count="$(jq -r '.questions | length' "$ask_json" 2>/dev/null || printf '0')"
  if [ "$question_count" -gt 0 ]; then
    break
  fi
  sleep 1
done
if [ ! -s "$ask_json" ] || [ "$(jq '.questions | length' "$ask_json")" -ne 1 ]; then
  printf 'Fleet did not receive live Claude AskUserQuestion\n' >&2
  tmux capture-pane -t "$tmux_session" -p
  tmux capture-pane -t "$daemon_session" -p
  exit 1
fi

printf '\n=== 1. Live Claude picker waits on Fleet ===\n'
tmux capture-pane -t "$tmux_session" -p
sleep 5
printf '\n=== 2. Fleet CLI exposes exact structured request ===\n'
jq '{head_revision, questions:[.questions[]|{session_key,version,request_fingerprint,questions}]}' "$ask_json"
sleep 5

session_key="$(jq -r '.questions[0].session_key' "$ask_json")"
version="$(jq -r '.questions[0].version' "$ask_json")"
fingerprint="$(jq -r '.questions[0].request_fingerprint' "$ask_json")"
question_id="$(jq -r '.questions[0].questions[0].id' "$ask_json")"
answers="$(jq -cn --arg id "$question_id" '[{question_id:$id,selected_options:["Yes"]}]')"

printf '\n=== 3. Fleet CLI injects structured Yes answer ===\n'
AINB_HANGAR_HOME="$hangar_home" "$ainb_bin" fleet answer "$session_key" \
  --version "$version" --fingerprint "$fingerprint" --answers "$answers" --format json
sleep 5

for _ in $(seq 1 60); do
  pane="$(tmux capture-pane -t "$tmux_session" -p)"
  if printf '%s' "$pane" | grep -Eq 'ANSWER_RECEIVED: ?Yes'; then
    break
  fi
  sleep 1
done
if ! printf '%s' "$pane" | grep -Eq 'ANSWER_RECEIVED: ?Yes'; then
  printf 'Claude did not visibly acknowledge injected answer\n' >&2
  printf '%s\n' "$pane"
  exit 1
fi

printf '\n=== 4. Same live Claude pane receives answer ===\n'
printf '%s\n' "$pane"
sleep 5
printf '\nPASS: real Claude picker -> Fleet CLI -> broker -> ANSWER_RECEIVED: Yes\n'
