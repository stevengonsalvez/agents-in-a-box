#!/usr/bin/env bash
# ABOUTME: Hand-verify runbook for `ainb fleet` DoD #2 / #3 / #4.
#
# Spawns throwaway claude session(s) via `ainb run`, exercises the fleet
# subcommand under test, asserts the expected behaviour, cleans up by killing
# only the exact ainb-managed sessions it created.
#
# Strict tmux safety: never `tmux kill-server`, never `pkill tmux`, never a
# wildcard kill. Cleanup uses `ainb kill <name>` which calls
# `tmux kill-session -t <exact-name>` internally.
#
# Usage:
#   scripts/fleet-test.sh dod2           # broadcast lands in JSONL
#   scripts/fleet-test.sh dod3           # daemon auto-continues on injected error
#   scripts/fleet-test.sh dod4           # sequence runs 3 steps ack-gated
#   scripts/fleet-test.sh all            # all three, sequentially
#   scripts/fleet-test.sh --dry-run dod2 # print commands without running
#
# Cost note: each DoD spawns one real claude session using `--model haiku`
# (cheap). Expect ~$0.001 / DoD in API spend; ~30-60s wallclock each.
#
# Evidence: stdout + stderr is teed to .agents/test-evidence/fleet-test-<ts>.log

set -euo pipefail

# Point at the locally-built ainb (with `fleet` subcommand) by default.
# Override via env: AINB=/path/to/other/ainb scripts/fleet-test.sh dod2
AINB="${AINB:-$(git rev-parse --show-toplevel)/ainb-tui/target/debug/ainb}"
if [ ! -x "$AINB" ]; then
  echo "ERROR: ainb binary not executable at $AINB" >&2
  echo "Build first: (cd ainb-tui && cargo build --bin ainb)" >&2
  exit 2
fi

TS=$(date -u '+%Y%m%dT%H%M%SZ')
PID=$$
PREFIX="fleet-test-${PID}-${TS}"
EVIDENCE_DIR="$(git rev-parse --show-toplevel)/.agents/test-evidence"
mkdir -p "$EVIDENCE_DIR"
LOG="$EVIDENCE_DIR/${PREFIX}.log"

# ─────────────────────────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────────────────────────

DRY_RUN=0
SPAWNED_NAMES=()

log() {
  # Always to stderr + log file. Never to stdout — keeps function returns clean.
  local line
  line="[$(date -u '+%H:%M:%S')] $*"
  printf '%s\n' "$line" >&2
  printf '%s\n' "$line" >> "$LOG"
}

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf 'DRY: %q ' "$@"; printf '\n'
    return 0
  fi
  "$@"
}

# Spawn one claude session via ainb run.
# Sets CURRENT_NAME and appends to SPAWNED_NAMES in the CALLER's shell
# (this function must NOT be invoked via $(...) — that runs it in a subshell
# and the SPAWNED_NAMES += would be lost, leaving the session leaked on exit).
# Usage:
#   spawn_session 1
#   name="$CURRENT_NAME"
spawn_session() {
  local n="$1"
  CURRENT_NAME="${PREFIX}-${n}"
  local repo="/tmp/${CURRENT_NAME}"
  log "spawn_session $CURRENT_NAME (repo=$repo)"
  if [ "$DRY_RUN" -eq 0 ]; then
    mkdir -p "$repo"
    ( cd "$repo" && git init -q . && git commit -q --allow-empty -m "test seed" )
    "$AINB" run \
      --repo "$repo" \
      --name "$CURRENT_NAME" \
      --tool claude \
      --model haiku \
      --dangerously-skip-permissions \
      --format json > "$EVIDENCE_DIR/${CURRENT_NAME}-run.json" 2>&1
    SPAWNED_NAMES+=( "$CURRENT_NAME" )
  fi
}

# Wait until `ainb list` shows the session with claude_active=true. Times out
# after 90s.
wait_for_claude_active() {
  local name="$1"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "DRY: would wait for $name to become claude_active"
    return 0
  fi
  local elapsed=0
  while [ "$elapsed" -lt 90 ]; do
    if "$AINB" list --format json 2>/dev/null \
        | jq -e --arg n "$name" 'any(.[]; .workspace_name == $n and .is_running == true)' \
        >/dev/null 2>&1; then
      log "session $name is running"
      return 0
    fi
    sleep 3
    elapsed=$((elapsed + 3))
  done
  log "ERROR: session $name did not become active within 90s"
  return 1
}

# Resolve the JSONL transcript path for a workspace. macOS /tmp ↔ /private/tmp
# is the gotcha — ainb stores the resolved /private/tmp path, so we try both.
# Returns empty string + non-zero on failure, but never panics under `set -e`
# because callers use `path=$(transcript_path_for ...)`.
transcript_path_for() {
  local workspace="$1"
  local worktree
  worktree=$("$AINB" list --format json 2>/dev/null \
    | jq -r --arg n "$workspace" '.[] | select(.workspace_name == $n) | .worktree_path' \
    || true)
  if [ -z "$worktree" ]; then
    log "WARN: no worktree_path for workspace=$workspace"
    return 0
  fi
  local slug
  slug=$(echo "$worktree" | sed 's|/|-|g')
  local candidate
  candidate=$(ls -t "${HOME}/.claude/projects/${slug}"/*.jsonl 2>/dev/null | head -1 || true)
  if [ -z "$candidate" ]; then
    # /tmp ↔ /private/tmp fallback (macOS).
    local alt_slug="${slug/-tmp-/-private-tmp-}"
    if [ "$alt_slug" != "$slug" ]; then
      candidate=$(ls -t "${HOME}/.claude/projects/${alt_slug}"/*.jsonl 2>/dev/null | head -1 || true)
    fi
  fi
  printf '%s' "$candidate"
}

cleanup() {
  local name
  for name in "${SPAWNED_NAMES[@]:-}"; do
    [ -z "$name" ] && continue
    log "cleanup: ainb kill $name"
    if [ "$DRY_RUN" -eq 0 ]; then
      "$AINB" kill --force "$name" 2>&1 | tee -a "$LOG" || true
      # Also remove the throwaway worktree dir under /tmp.
      /bin/rm -rf "/tmp/${name}" "/private/tmp/${name}" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT

# ─────────────────────────────────────────────────────────────────────────
# DoD #2 — broadcast lands in JSONL
# ─────────────────────────────────────────────────────────────────────────

dod2() {
  log "=== DoD #2: broadcast lands in JSONL ==="
  spawn_session 1
  local name="$CURRENT_NAME"
  wait_for_claude_active "$name"

  local nonce="fleet-nonce-${PID}-${TS}-$RANDOM"
  log "broadcasting nonce=$nonce to filter=$name"
  if [ "$DRY_RUN" -eq 0 ]; then
    "$AINB" fleet --format json broadcast "$nonce" --filter "$name" 2>&1 | tee -a "$LOG"
  fi

  if [ "$DRY_RUN" -eq 0 ]; then
    local transcript
    transcript=$(transcript_path_for "$name")
    log "polling transcript '${transcript:-<not-found>}' for nonce…"
    local elapsed=0
    while [ "$elapsed" -lt 30 ]; do
      if [ -n "$transcript" ] && [ -f "$transcript" ] && grep -q "$nonce" "$transcript" 2>/dev/null; then
        log "PASS — nonce appears in JSONL"
        grep "$nonce" "$transcript" | head -1 | tee -a "$LOG"
        return 0
      fi
      sleep 2
      elapsed=$((elapsed + 2))
      # Refresh path each poll — file may be created mid-wait.
      [ -z "$transcript" ] && transcript=$(transcript_path_for "$name")
    done
    log "FAIL — nonce not found in JSONL within 30s (transcript=${transcript:-?})"
    return 1
  fi
}

# ─────────────────────────────────────────────────────────────────────────
# DoD #3 — daemon auto-continues on injected error
# ─────────────────────────────────────────────────────────────────────────

dod3() {
  log "=== DoD #3: daemon auto-continues on injected error ==="
  spawn_session 1
  local name="$CURRENT_NAME"
  wait_for_claude_active "$name"

  local tmux_session
  tmux_session=$("$AINB" list --format json \
    | jq -r --arg n "$name" '.[] | select(.workspace_name == $n) | .tmux_session_name')
  log "tmux_session=$tmux_session"

  # Start daemon in background.
  log "starting ainb fleet daemon --verbose"
  if [ "$DRY_RUN" -eq 0 ]; then
    "$AINB" fleet daemon --verbose > "$EVIDENCE_DIR/${name}-daemon.log" 2>&1 &
    local daemon_pid=$!
    sleep 5

    # Inject the error text into the pane (literal mode is critical).
    log "injecting 'rate_limited: please retry' into pane"
    tmux send-keys -t "$tmux_session" -l "rate_limited: please retry"
    tmux send-keys -t "$tmux_session" Enter

    # Wait up to 15s for daemon to log auto-continue.
    local elapsed=0
    while [ "$elapsed" -lt 15 ]; do
      if grep -q "auto-continue.*${tmux_session}" "$EVIDENCE_DIR/${name}-daemon.log" 2>/dev/null; then
        log "PASS — daemon emitted auto-continue for $tmux_session"
        grep "auto-continue" "$EVIDENCE_DIR/${name}-daemon.log" | tee -a "$LOG"
        kill "$daemon_pid" 2>/dev/null || true
        return 0
      fi
      sleep 1
      elapsed=$((elapsed + 1))
    done

    log "FAIL — daemon did not auto-continue within 15s"
    kill "$daemon_pid" 2>/dev/null || true
    return 1
  fi
}

# ─────────────────────────────────────────────────────────────────────────
# DoD #4 — sequence runs 3 steps with ack between each
# ─────────────────────────────────────────────────────────────────────────

dod4() {
  log "=== DoD #4: sequence ack-gated multi-step ==="
  spawn_session 1
  local name="$CURRENT_NAME"
  wait_for_claude_active "$name"

  local s1="seq-step1-${TS}"
  local s2="seq-step2-${TS}"
  local s3="seq-step3-${TS}"

  log "running ainb fleet sequence with 3 steps (5s/step max)"
  if [ "$DRY_RUN" -eq 0 ]; then
    local t_start
    t_start=$(date +%s)
    "$AINB" fleet sequence "$s1" "$s2" "$s3" --all --timeout 30 2>&1 | tee -a "$LOG"
    local t_end
    t_end=$(date +%s)
    local elapsed=$((t_end - t_start))
    log "sequence elapsed: ${elapsed}s"

    local transcript
    transcript=$(transcript_path_for "$name")
    log "asserting all 3 steps appear in JSONL"
    for s in "$s1" "$s2" "$s3"; do
      if grep -q "$s" "$transcript" 2>/dev/null; then
        log "  ✓ $s found in transcript"
      else
        log "  ✗ $s NOT found in transcript"
        return 1
      fi
    done

    # ack-gated means total elapsed > ~6s (at minimum 2 inter-step waits).
    if [ "$elapsed" -ge 4 ]; then
      log "PASS — sequence took ${elapsed}s, consistent with ack-gating"
      return 0
    else
      log "WARN — sequence completed in ${elapsed}s, may not have ack-gated"
      return 1
    fi
  fi
}

# ─────────────────────────────────────────────────────────────────────────
# DoD #5 — `ainb fleet needs` surfaces a session whose claude fired AskUserQuestion
# ─────────────────────────────────────────────────────────────────────────

dod5_needs() {
  log "=== DoD #5: needs detects ASK signal from fired AskUserQuestion ==="

  spawn_session needs   # interactive claude — no -p flag
  local name="$CURRENT_NAME"
  wait_for_claude_active "$name"

  if [ "$DRY_RUN" -eq 0 ]; then
    # Send the prompt via the same send-route the broadcast verb uses.
    log "broadcasting AskUserQuestion-triggering prompt to $name"
    "$AINB" fleet broadcast \
      "Use the AskUserQuestion tool to ask me to pick a favorite colour from: red, green, blue. Just fire the tool; do not narrate." \
      --filter "$name" 2>&1 | tee -a "$LOG"

    # Wait for AskUserQuestion tool_use to land in JSONL → surfaced by needs.
    log "waiting up to 90s for AskUserQuestion to land + needs to surface ASK…"
    local elapsed=0
    local found=0
    while [ "$elapsed" -lt 90 ]; do
      sleep 5
      elapsed=$((elapsed + 5))
      local out
      out=$("$AINB" --format json fleet needs 2>/dev/null)
      if echo "$out" | jq -e --arg n "$name" '
        any(.[]; (.session.tmux_session // "" | contains($n)) and .kind == "ASK")
      ' >/dev/null 2>&1; then
        log "PASS — needs surfaced $name with kind=ASK at ${elapsed}s"
        echo "$out" | jq --arg n "$name" '
          .[] | select((.session.tmux_session // "") | contains($n))
            | { kind, question: .context.question, options: [.context.options[].label] }
        ' | tee -a "$LOG"
        found=1
        break
      fi
    done
    if [ "$found" -eq 0 ]; then
      log "FAIL — needs did not surface $name as ASK within 90s"
      log "current needs first row:"
      "$AINB" --format json fleet needs 2>/dev/null \
        | jq '.[0] | { kind, name: .session.tmux_session }' | tee -a "$LOG"
      return 1
    fi
  fi
}

# ─────────────────────────────────────────────────────────────────────────
# Entry
# ─────────────────────────────────────────────────────────────────────────

cmd=""
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    dod2|dod3|dod4|dod5_needs|all) cmd="$arg" ;;
    *) echo "usage: $0 [--dry-run] {dod2|dod3|dod4|dod5_needs|all}"; exit 2 ;;
  esac
done

[ -z "$cmd" ] && { echo "usage: $0 [--dry-run] {dod2|dod3|dod4|dod5_needs|all}"; exit 2; }

log "fleet-test starting — prefix=$PREFIX dry_run=$DRY_RUN cmd=$cmd"
log "evidence log: $LOG"

case "$cmd" in
  dod2) dod2 ;;
  dod3) dod3 ;;
  dod4) dod4 ;;
  dod5_needs) dod5_needs ;;
  all)  dod2 && dod3 && dod4 && dod5_needs ;;
esac
