#!/usr/bin/env bash
# swarm-create v2 watchdog daemon
# Pure bash, no LLM. Runs in dedicated tmux session per swarm.
#
# Usage:
#   bash watchdog.sh <team-id>
#
# Reads team.json for: provider, tick_min, members, epic_id, commands.verify
# Writes:
#   <team-dir>/watchdog.log       - per-tick activity
#   <team-dir>/watchdog.state     - per-pane stuck counters + last hash
#   <team-dir>/watchdog.pid       - daemon pid
#   <team-dir>/shared/incidents.log - failed nudges, after-3rd-cycle alerts
#   <team-dir>/inbox/leader.jsonl - help messages on stuck after 2 cycles
#   <team-dir>/.finalized          - sentinel set after finalize.sh runs once

set -u

# ---------- args + paths ----------
TEAM_ID="${1:?Usage: watchdog.sh <team-id>}"
# Match swarm-lib.sh default: $PWD/.claude/swarm (so it resolves to the worktree's
# swarm dir, not the global {{HOME_TOOL_DIR}}/swarm). Override via env var SWARM_BASE_DIR.
SWARM_BASE_DIR="${SWARM_BASE_DIR:-${PWD}/.claude/swarm}"
TEAM_DIR="${SWARM_BASE_DIR}/${TEAM_ID}"
TEAM_JSON="${TEAM_DIR}/team.json"
LOG_FILE="${TEAM_DIR}/watchdog.log"
STATE_FILE="${TEAM_DIR}/watchdog.state"
PID_FILE="${TEAM_DIR}/watchdog.pid"
INCIDENTS_FILE="${TEAM_DIR}/shared/incidents.log"
LEADER_INBOX="${TEAM_DIR}/inbox/leader.jsonl"
FINALIZED_MARKER="${TEAM_DIR}/.finalized"
SKILL_DIR="$(cd "$(dirname "$0")" && pwd)"
FINALIZE_SH="${SKILL_DIR}/finalize.sh"

if [[ ! -f "$TEAM_JSON" ]]; then
  echo "FATAL: team.json not found at $TEAM_JSON" >&2
  exit 1
fi

mkdir -p "$(dirname "$INCIDENTS_FILE")" "$(dirname "$LEADER_INBOX")"

# Daemon pid
echo "$$" > "$PID_FILE"

# ---------- logging ----------
log() {
  local msg="$1"
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$msg" | tee -a "$LOG_FILE"
}

log_jsonl() {
  # Append a JSONL line to a file: log_jsonl <file> <key=val> <key=val> ...
  local file="$1"; shift
  local ts; ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  local payload="{\"ts\":\"${ts}\""
  while [[ $# -gt 0 ]]; do
    local pair="$1"; shift
    local key="${pair%%=*}"
    local val="${pair#*=}"
    val="${val//\\/\\\\}"
    val="${val//\"/\\\"}"
    val="${val//$'\n'/\\n}"
    payload+=",\"${key}\":\"${val}\""
  done
  payload+="}"
  echo "$payload" >> "$file"
}

# ---------- team.json reads ----------
read_team_field() {
  local jq_expr="$1"
  jq -r "$jq_expr" "$TEAM_JSON" 2>/dev/null
}

PROVIDER="$(read_team_field '.provider // "claude"')"
TICK_MIN="$(read_team_field '.watchdog.tick_min // 5')"
EPIC_ID="$(read_team_field '.epic_id // ""')"
VERIFY_CMD="$(read_team_field '.commands.verify // ":"')"
WATCHDOG_ENABLED="$(read_team_field '.watchdog.enabled // true')"

if [[ "$WATCHDOG_ENABLED" != "true" ]]; then
  log "watchdog disabled in team.json — exiting"
  rm -f "$PID_FILE"
  exit 0
fi

log "watchdog start team=${TEAM_ID} provider=${PROVIDER} tick=${TICK_MIN}m epic=${EPIC_ID}"

# ---------- provider spinner regex ----------
spinner_regex_for() {
  local provider="$1"
  case "$provider" in
    claude)
      # claude code shows: ✻/✳/⏺/✿/◐/◑/◓/◒ + an active verb
      echo '(✻|✳|⏺|✿|◐|◑|◓|◒) (Cooked|Sprouting|Metamorphosing|Proofing|Worked|Baked|Sautéed|Brewing|Simmering|Stewing|Steeping|Distilling|Marinating|Crystallizing|Synthesizing|Forging|Composing|Pondering|Wrestling|Computing|Concocting|Cooking|Crafting|Dispatching|Thinking|Forming|Reading|Reasoning)'
      ;;
    codex)
      # codex spinner — stubs; calibrate when first codex swarm runs
      echo '(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏) '
      ;;
    copilot)
      echo '(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏) '
      ;;
    generic|*)
      # Fall back to "pane-hash changed" heuristic — no spinner regex
      echo ''
      ;;
  esac
}

SPINNER_REGEX="$(spinner_regex_for "$PROVIDER")"

# ---------- state load ----------
# State JSON: { "<pane>": { "stuck_cycles": N, "last_hash": "...", "last_action": "..." } }
load_state() {
  if [[ -f "$STATE_FILE" ]]; then
    cat "$STATE_FILE"
  else
    echo "{}"
  fi
}

save_state() {
  echo "$1" > "$STATE_FILE"
}

state_get() {
  local state="$1" pane="$2" key="$3"
  echo "$state" | jq -r --arg p "$pane" --arg k "$key" '.[$p][$k] // ""'
}

state_set() {
  local state="$1" pane="$2" key="$3" val="$4"
  echo "$state" | jq --arg p "$pane" --arg k "$key" --arg v "$val" \
    '.[$p] = (.[$p] // {}) | .[$p][$k] = $v'
}

# ---------- pane inspection ----------
pane_capture() {
  local target="$1"
  tmux capture-pane -t "$target" -p -S -25 2>/dev/null || echo ""
}

pane_hash() {
  echo -n "$1" | shasum -a 256 | awk '{print $1}'
}

detect_state() {
  local pane_content="$1"
  # CAPPED
  if echo "$pane_content" | grep -q "5h \[██████\] 100%" || \
     echo "$pane_content" | grep -q "You've hit your limit"; then
    echo "CAPPED"; return
  fi
  # ACTIVE (provider-specific spinner)
  if [[ -n "$SPINNER_REGEX" ]] && echo "$pane_content" | grep -qE "$SPINNER_REGEX"; then
    echo "ACTIVE"; return
  fi
  # STUCK = non-empty text in prompt line `❯ XXX`
  if echo "$pane_content" | grep -qE '^❯ [^ ]'; then
    echo "STUCK"; return
  fi
  echo "IDLE"
}

# ---------- comprehensive nudge sequence ----------
nudge_pane() {
  local target="$1"
  local pane_name="$2"
  local stuck_cycles="$3"

  log "${pane_name}: STUCK (cycle ${stuck_cycles}) — beginning comprehensive nudge"

  # Step 1: plain Enter
  tmux send-keys -t "$target" C-m 2>/dev/null
  sleep 1
  tmux send-keys -t "$target" Enter 2>/dev/null
  sleep 5
  local post1; post1="$(pane_capture "$target")"
  local state1; state1="$(detect_state "$post1")"
  if [[ "$state1" == "ACTIVE" ]] || ! echo "$post1" | grep -qE '^❯ [^ ]'; then
    log "  ${pane_name}: nudge step1 (Enter) → ${state1}"
    return 0
  fi

  # Step 2: type "continue" + Enter
  tmux send-keys -t "$target" "continue" 2>/dev/null
  sleep 1
  tmux send-keys -t "$target" Enter 2>/dev/null
  sleep 5
  local post2; post2="$(pane_capture "$target")"
  local state2; state2="$(detect_state "$post2")"
  if [[ "$state2" == "ACTIVE" ]] || ! echo "$post2" | grep -qE '^❯ [^ ]'; then
    log "  ${pane_name}: nudge step2 (continue+Enter) → ${state2}"
    return 0
  fi

  log "  ${pane_name}: nudge step1+2 FAILED — still stuck after Enter and 'continue'"

  # Step 3: escalate to leader inbox if >=2 stuck cycles
  if [[ "$stuck_cycles" -ge 2 ]]; then
    local stuck_snippet
    stuck_snippet="$(echo "$post2" | grep -E '^❯ [^ ]' | head -1 | cut -c1-120)"
    log_jsonl "$LEADER_INBOX" \
      "type=help" \
      "from=watchdog" \
      "subject=agent stuck" \
      "agent=${pane_name}" \
      "stuck_cycles=${stuck_cycles}" \
      "stuck_snippet=${stuck_snippet}"
    log "  ${pane_name}: escalated to leader.jsonl (help message, cycle=${stuck_cycles})"
  fi

  # Step 4: after 4th cycle, write to incidents.log
  if [[ "$stuck_cycles" -ge 4 ]]; then
    log_jsonl "$INCIDENTS_FILE" \
      "type=nudge_failed" \
      "agent=${pane_name}" \
      "stuck_cycles=${stuck_cycles}"
    log "  ${pane_name}: NUDGE_FAILED — logged to incidents.log"
  fi
  return 1
}

# ---------- epic-done check ----------
check_epic_done() {
  if [[ -z "$EPIC_ID" ]]; then
    return 1
  fi
  if [[ -f "$FINALIZED_MARKER" ]]; then
    return 1  # already finalized
  fi

  # Look up children of the epic. bd may not support `--parent` directly across versions;
  # use a generic predicate via beads. Fallback: all bd issues with this label/dep.
  local all_closed
  all_closed="$(BEADS_DIR="$(detect_beads_dir)" bd list --json 2>/dev/null | \
    jq --arg epic "$EPIC_ID" '
      [.[] | select(
        (.parent_epic // "") == $epic
        or (.depends_on // [] | contains([$epic]))
        or (.id == $epic and .status == "closed")
      )] | length as $n
      | if $n > 0 then (
          [.[] | select(
            (.parent_epic // "") == $epic
            or (.depends_on // [] | contains([$epic]))
          )] | all(.status == "closed")
        ) else false end
    ' 2>/dev/null || echo "false")"

  if [[ "$all_closed" == "true" ]]; then
    return 0
  fi
  return 1
}

detect_beads_dir() {
  # Prefer team.json beads_dir, fallback to <worktree>/.beads, fallback to cwd/.beads
  local bd_path
  bd_path="$(read_team_field '.beads_dir // ""')"
  if [[ -n "$bd_path" && -d "$bd_path" ]]; then echo "$bd_path"; return; fi
  if [[ -d "$(pwd)/.beads" ]]; then echo "$(pwd)/.beads"; return; fi
  echo "${HOME}/.beads"
}

# ---------- members enumeration ----------
list_members() {
  # Return list of {pane_name, session_name} pairs
  jq -r '
    (.members // [] | map(.name // .id // .)) as $agents |
    ["leader"] + ($agents | map(. // empty)) |
    .[] | select(. != null and . != "")
  ' "$TEAM_JSON" 2>/dev/null
}

# ---------- main tick loop ----------
heartbeat() {
  local ts; ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  # Update team.json watchdog.last_tick atomically via temp file
  local tmp; tmp="$(mktemp)"
  jq --arg ts "$ts" '.watchdog.last_tick = $ts | .watchdog.pid = (env.WATCHDOG_PID // "")' \
    "$TEAM_JSON" > "$tmp" 2>/dev/null && mv "$tmp" "$TEAM_JSON" || rm -f "$tmp"
}

run_tick() {
  log "--- tick start ---"
  local state; state="$(load_state)"
  local nudge_count=0
  local capped_count=0
  local active_count=0
  local idle_count=0

  for member in $(list_members); do
    local target="${TEAM_ID}-${member}"
    if ! tmux has-session -t "$target" 2>/dev/null; then
      log "${member}: no tmux session — skip"
      continue
    fi

    local content; content="$(pane_capture "$target")"
    local hash; hash="$(pane_hash "$content")"
    local last_hash; last_hash="$(state_get "$state" "$member" "last_hash")"
    local stuck_cycles; stuck_cycles="$(state_get "$state" "$member" "stuck_cycles")"
    [[ -z "$stuck_cycles" ]] && stuck_cycles=0

    local pane_state; pane_state="$(detect_state "$content")"

    # If pane-hash unchanged AND not ACTIVE AND not STUCK → mark stuck (silent stall)
    if [[ "$hash" == "$last_hash" && "$pane_state" == "IDLE" ]]; then
      pane_state="STUCK"
    fi

    case "$pane_state" in
      CAPPED)
        log "${member}: 🔴 CAPPED"
        capped_count=$((capped_count + 1))
        state="$(state_set "$state" "$member" "stuck_cycles" "0")"
        ;;
      ACTIVE)
        log "${member}: 🟢 ACTIVE"
        active_count=$((active_count + 1))
        state="$(state_set "$state" "$member" "stuck_cycles" "0")"
        ;;
      IDLE)
        log "${member}: 🔵 IDLE (hash changed — between actions)"
        idle_count=$((idle_count + 1))
        state="$(state_set "$state" "$member" "stuck_cycles" "0")"
        ;;
      STUCK)
        stuck_cycles=$((stuck_cycles + 1))
        state="$(state_set "$state" "$member" "stuck_cycles" "$stuck_cycles")"
        nudge_pane "$target" "$member" "$stuck_cycles" && nudge_count=$((nudge_count + 1))
        ;;
    esac

    state="$(state_set "$state" "$member" "last_hash" "$hash")"
    state="$(state_set "$state" "$member" "last_state" "$pane_state")"
  done

  save_state "$state"

  log "tick summary: active=${active_count} idle=${idle_count} capped=${capped_count} nudged=${nudge_count}"

  # Epic-done check
  if check_epic_done; then
    if [[ -x "$FINALIZE_SH" ]]; then
      log "EPIC DONE detected — running finalize.sh"
      bash "$FINALIZE_SH" "$TEAM_ID" >> "$LOG_FILE" 2>&1
      touch "$FINALIZED_MARKER"
      log "finalize complete — marker set"
    else
      log "EPIC DONE but $FINALIZE_SH not executable — skipping finalize"
    fi
  fi

  heartbeat
}

# ---------- signal handlers ----------
cleanup() {
  log "watchdog stopping (signal received)"
  rm -f "$PID_FILE"
  exit 0
}
trap cleanup INT TERM

# ---------- main loop ----------
export WATCHDOG_PID="$$"

while true; do
  run_tick
  log "sleeping ${TICK_MIN}m until next tick"
  sleep $((TICK_MIN * 60))
done
