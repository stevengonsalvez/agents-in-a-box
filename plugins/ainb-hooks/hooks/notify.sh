#!/usr/bin/env bash
# ainb-hooks: universal notification hook for Claude Code, Codex CLI, and Copilot CLI.
#
# Reads a hook event payload (Claude/Copilot pipe JSON on stdin; Codex passes
# JSON as argv[1]), builds a normalized envelope, and delivers it to ainb-notifyd
# via a Unix socket at $HOME/.agents-in-a-box/notify.sock. On any delivery
# failure the payload is appended to a fallback JSONL file that notifyd
# replays on its next startup. The script always exits 0 so a delivery
# failure never blocks the host agent.

set -u

# ----- 1. Read input ----------------------------------------------------------
# Codex: argv[1] contains the JSON payload.
# Claude / Mastra / Droid: JSON arrives on stdin.
if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
  AINB_INPUT="$1"
  AINB_INPUT_SOURCE="argv"
else
  AINB_INPUT="$(cat 2>/dev/null || printf '')"
  AINB_INPUT_SOURCE="stdin"
fi

# An empty input is a noop — exit cleanly.
if [ -z "${AINB_INPUT}" ]; then
  exit 0
fi

# ----- 2. Determine agent ----------------------------------------------------
# AINB_AGENT can be set explicitly by the registering side (preferred).
# Otherwise we infer: argv-delivery => codex, stdin-delivery => claude.
if [ -z "${AINB_AGENT:-}" ]; then
  case "${AINB_INPUT_SOURCE}" in
    argv)   AINB_AGENT="codex" ;;
    stdin)  AINB_AGENT="claude" ;;
    *)      AINB_AGENT="unknown" ;;
  esac
fi

# ----- 3. Extract event + session via jq when available ----------------------
# We require jq; if it's missing fall back to grep parsing for the two
# strict fields we care about so the script still works in minimal envs.
ainb_has_jq=0
if command -v jq >/dev/null 2>&1; then
  ainb_has_jq=1
fi

if [ "${ainb_has_jq}" = "1" ]; then
  AINB_RAW_EVENT="$(printf '%s' "${AINB_INPUT}" | jq -r '.hook_event_name // .type // ""' 2>/dev/null)"
  AINB_SESSION_ID="$(printf '%s' "${AINB_INPUT}" | jq -r '.session_id // .sessionId // .resourceId // ""' 2>/dev/null)"
  AINB_CWD="$(printf '%s' "${AINB_INPUT}" | jq -r '.cwd // .working_directory // ""' 2>/dev/null)"
  AINB_MATCHER="$(printf '%s' "${AINB_INPUT}" | jq -r '.matcher // .hook_matcher // ""' 2>/dev/null)"
else
  AINB_RAW_EVENT="$(printf '%s' "${AINB_INPUT}" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | sed -E 's/.*"([^"]*)"$/\1/' | head -n1)"
  [ -z "${AINB_RAW_EVENT}" ] && AINB_RAW_EVENT="$(printf '%s' "${AINB_INPUT}" | grep -oE '"type"[[:space:]]*:[[:space:]]*"[^"]*"' | sed -E 's/.*"([^"]*)"$/\1/' | head -n1)"
  AINB_SESSION_ID="$(printf '%s' "${AINB_INPUT}" | grep -oE '"session_id"[[:space:]]*:[[:space:]]*"[^"]*"' | sed -E 's/.*"([^"]*)"$/\1/' | head -n1)"
  AINB_CWD="$(printf '%s' "${AINB_INPUT}" | grep -oE '"cwd"[[:space:]]*:[[:space:]]*"[^"]*"' | sed -E 's/.*"([^"]*)"$/\1/' | head -n1)"
  AINB_MATCHER=""
fi

# If we can't even identify the event, silently drop. False positives are
# worse than silence.
if [ -z "${AINB_RAW_EVENT}" ]; then
  exit 0
fi

# Codex emits `type` values like `agent-turn-complete`, `request_user_input`.
# We preserve the raw_event verbatim per spec (no canonical mapping in MVP)
# but include a matcher slot for Claude's `Notification:idle_prompt` and
# Codex's `PermissionRequest` style.
if [ -n "${AINB_MATCHER}" ]; then
  AINB_RAW_EVENT="${AINB_RAW_EVENT}:${AINB_MATCHER}"
fi

# ----- 4. Build envelope ------------------------------------------------------
# Portable epoch milliseconds:
# - GNU date supports `%s%3N` natively.
# - BSD date (macOS) silently emits `<seconds>3N` (literal 3N suffix).
# - Both support `%s`; we multiply by 1000 when %3N isn't usable.
AINB_TS_RAW="$(date +%s%3N 2>/dev/null || true)"
case "${AINB_TS_RAW}" in
  *[!0-9]* | "" )
    # Non-numeric / empty — fall back to seconds × 1000.
    AINB_TS_MS=$(($(date +%s) * 1000))
    ;;
  *)
    AINB_TS_MS="${AINB_TS_RAW}"
    ;;
esac
AINB_PROJECT="$(basename "${AINB_CWD:-$PWD}" 2>/dev/null)"
[ -z "${AINB_PROJECT}" ] && AINB_PROJECT="unknown"

if [ "${ainb_has_jq}" = "1" ]; then
  AINB_ENVELOPE="$(printf '%s' "${AINB_INPUT}" | jq -c \
    --arg agent "${AINB_AGENT}" \
    --arg raw_event "${AINB_RAW_EVENT}" \
    --arg session_id "${AINB_SESSION_ID}" \
    --arg cwd "${AINB_CWD}" \
    --arg project "${AINB_PROJECT}" \
    --argjson ts "${AINB_TS_MS}" \
    '{protocol_version: 1, agent: $agent, raw_event: $raw_event, session_id: $session_id, cwd: $cwd, project: $project, ts: $ts, payload: .}' 2>/dev/null)"
else
  # Minimal envelope without payload nesting when jq is missing.
  ainb_json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/$/\\n/g' | tr -d '\n' | sed -e 's/\\n$//'
  }
  AINB_ENVELOPE="$(printf '{"protocol_version":1,"agent":"%s","raw_event":"%s","session_id":"%s","cwd":"%s","project":"%s","ts":%s,"payload":{"_raw":"%s"}}' \
    "${AINB_AGENT}" \
    "$(ainb_json_escape "${AINB_RAW_EVENT}")" \
    "$(ainb_json_escape "${AINB_SESSION_ID}")" \
    "$(ainb_json_escape "${AINB_CWD}")" \
    "$(ainb_json_escape "${AINB_PROJECT}")" \
    "${AINB_TS_MS}" \
    "$(ainb_json_escape "${AINB_INPUT}")")"
fi

if [ -z "${AINB_ENVELOPE}" ]; then
  exit 0
fi

# ----- 5. Delivery ------------------------------------------------------------
AINB_DIR="${HOME}/.agents-in-a-box"
AINB_SOCK="${AINB_DIR}/notify.sock"
AINB_FALLBACK="${AINB_DIR}/notify.fallback.jsonl"
AINB_SPAWN_LOCK="${AINB_DIR}/notify.spawn.lock"

mkdir -p "${AINB_DIR}" 2>/dev/null

ainb_send() {
  # Returns 0 on successful socket send, non-zero otherwise.
  if [ ! -S "${AINB_SOCK}" ]; then
    return 1
  fi
  if command -v nc >/dev/null 2>&1; then
    printf '%s\n' "${AINB_ENVELOPE}" | nc -U -w 1 "${AINB_SOCK}" >/dev/null 2>&1
    return $?
  fi
  # Fallback delivery via /dev/tcp-like trick using socat if present.
  if command -v socat >/dev/null 2>&1; then
    printf '%s\n' "${AINB_ENVELOPE}" | socat - UNIX-CONNECT:"${AINB_SOCK}" >/dev/null 2>&1
    return $?
  fi
  # No socket client available — caller will write to fallback.
  return 2
}

ainb_socket_alive() {
  # True only when a daemon is actually ACCEPTING on the socket. A bare
  # socket *file* left behind by a crashed daemon must NOT count — testing
  # `[ -S ]` alone is what let a stale socket block respawn for days.
  [ -S "${AINB_SOCK}" ] || return 1
  if command -v nc >/dev/null 2>&1; then
    # `-N` closes the socket after stdin EOF so an accepting daemon answers
    # instantly; without it BSD/macOS nc waits out the full `-w` timeout.
    : | nc -U -N -w 1 "${AINB_SOCK}" >/dev/null 2>&1
    return $?
  fi
  if command -v socat >/dev/null 2>&1; then
    : | socat - UNIX-CONNECT:"${AINB_SOCK}" >/dev/null 2>&1
    return $?
  fi
  # No probe client available — assume present to avoid spawn storms.
  return 0
}

ainb_lazy_spawn() {
  # Best-effort lazy spawn of ainb notifyd. Silent on success and failure;
  # the fallback file is the safety net.
  if [ "${AINB_NOTIFY_DISABLE_LAZY_SPAWN:-}" = "1" ]; then
    return 1
  fi
  if ainb_socket_alive; then
    return 0
  fi
  if ! command -v ainb >/dev/null 2>&1; then
    return 1
  fi
  # Mutual exclusion across concurrent first-fires. `mkdir` is atomic on
  # POSIX *and* present on macOS — unlike `flock`, whose absence on macOS
  # silently disabled the old guard and let every fire spawn its own
  # daemon. The winner spawns; everyone else falls through to the fallback
  # file. A distinct `.d` path is used (not the old flock lock *file*) so a
  # leftover regular file from a previous version can't make mkdir fail
  # forever. A lock dir older than 60s (spawner crashed mid-flight) is
  # reclaimed so a stale lock can't wedge spawning permanently.
  ainb_lock_dir="${AINB_SPAWN_LOCK}.d"
  if [ -d "${ainb_lock_dir}" ] && command -v find >/dev/null 2>&1; then
    if [ -z "$(find "${ainb_lock_dir}" -maxdepth 0 -mmin -1 2>/dev/null)" ]; then
      rmdir "${ainb_lock_dir}" 2>/dev/null || true
    fi
  fi
  if ! mkdir "${ainb_lock_dir}" 2>/dev/null; then
    # Another fire is already spawning — let it win, we fall back.
    return 1
  fi
  # Detach hard: stdin from /dev/null (an inherited hook pipe on stdin is
  # the suspected cause of daemons wedging in startup before they ever
  # bind), stdout/stderr discarded, nohup so a closing pane/tmux can't
  # SIGHUP it.
  nohup ainb notifyd </dev/null >/dev/null 2>&1 &
  # Wait up to 1s for the socket to come up, then release the lock.
  ainb_attempts=0
  while [ "${ainb_attempts}" -lt 100 ]; do
    if ainb_socket_alive; then
      rmdir "${ainb_lock_dir}" 2>/dev/null || true
      return 0
    fi
    sleep 0.01 2>/dev/null || sleep 1
    ainb_attempts=$((ainb_attempts + 1))
  done
  rmdir "${ainb_lock_dir}" 2>/dev/null || true
  return 1
}

# Try the socket first.
if ! ainb_send; then
  # Either no socket or send failed — try lazy spawn and retry once.
  if ainb_lazy_spawn && ainb_send; then
    :
  else
    # Last resort: append envelope to fallback file (one JSON object per line).
    printf '%s\n' "${AINB_ENVELOPE}" >> "${AINB_FALLBACK}" 2>/dev/null || true
  fi
fi

# ----- 6. ATC plumbing (status files · durable inbox · Stop-drain) ------------
# Only active when this hook was installed under ATC management
# (AINB_MANAGED=atc, set by the settings.json managed block). Leaf sessions and
# the plain notifyd-only install skip every line below, so they pay nothing.
#
# `ainb fleet atc hook` does the real work in Rust (atomic status file, durable
# parent inbox commit, and — on Stop — the synchronous drain that prints
# {"decision":"block",...}). The shell only forwards what it already parsed and
# relays the command's stdout verbatim so Claude Code sees the block JSON.
if [ "${AINB_MANAGED:-}" = "atc" ] && command -v ainb >/dev/null 2>&1; then
  AINB_HOOK_EVENT="${AINB_HOOK_EVENT:-${AINB_RAW_EVENT}}"
  # Resolve the matcher to forward: the managed command sets AINB_HOOK_MATCHER
  # (e.g. AskUserQuestion for the PreToolUse hook); otherwise fall back to the
  # discriminator the payload-parse already pulled out (AINB_MATCHER:
  # notification_type / hook_matcher). The Rust side stamps it into the
  # appended events.jsonl line so PreToolUse/Notification/StopFailure carry
  # their kind without re-parsing the payload downstream.
  AINB_HOOK_MATCHER_FWD="${AINB_HOOK_MATCHER:-${AINB_MATCHER:-}}"
  # Forward the original payload on stdin so the Rust side can extract a
  # done_summary (last assistant line) + transcript_path without re-reading the
  # transcript.
  AINB_HOOK_OUT="$(printf '%s' "${AINB_INPUT}" | ainb fleet atc hook \
    --event "${AINB_HOOK_EVENT}" \
    --session-id "${AINB_SESSION_ID}" \
    --cwd "${AINB_CWD}" \
    --matcher "${AINB_HOOK_MATCHER_FWD}" 2>/dev/null)"
  # Relay any decision JSON (the Stop-drain block) to Claude Code on stdout.
  if [ -n "${AINB_HOOK_OUT}" ]; then
    printf '%s\n' "${AINB_HOOK_OUT}"
  fi
fi

exit 0
