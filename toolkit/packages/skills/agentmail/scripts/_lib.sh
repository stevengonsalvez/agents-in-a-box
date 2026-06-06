#!/usr/bin/env bash
# Shared lib for agentmail scripts. Sourced by create.sh/wait.sh/read.sh/etc.
# Resolves state dir, validates dependencies, exposes API base URL.

set -euo pipefail

AGENTMAIL_API="${AGENTMAIL_API:-https://myagentinbox.com/api}"

# Dependency check
for cmd in curl jq; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "ERROR: agentmail needs '$cmd' on PATH. Install via:" >&2
    case "$cmd" in
      jq)   echo "  brew install jq" >&2;;
      curl) echo "  brew install curl" >&2;;
    esac
    exit 1
  fi
done

# Resolve state directory (env > git root > ~/.cache)
resolve_state_dir() {
  if [ -n "${AGENTMAIL_STATE_DIR:-}" ]; then
    echo "$AGENTMAIL_STATE_DIR"
    return
  fi
  local git_root
  git_root=$(git rev-parse --show-toplevel 2>/dev/null || true)
  if [ -n "$git_root" ]; then
    echo "$git_root/.agents/agentmail"
  else
    echo "$HOME/.cache/agentmail"
  fi
}

state_file_for_slug() {
  local slug="$1"
  local dir
  dir=$(resolve_state_dir)
  mkdir -p "$dir/inboxes"
  echo "$dir/inboxes/${slug}.json"
}

# ISO8601 UTC now
iso_now() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

# Add 24h to ISO timestamp (portable across BSD/GNU date)
iso_plus_24h() {
  if date -u -d "+1 day" +"%Y-%m-%dT%H:%M:%SZ" >/dev/null 2>&1; then
    # GNU date
    date -u -d "+1 day" +"%Y-%m-%dT%H:%M:%SZ"
  else
    # BSD date (macOS)
    date -u -v+1d +"%Y-%m-%dT%H:%M:%SZ"
  fi
}

# Convert ISO timestamp to epoch seconds (portable)
iso_to_epoch() {
  local iso="$1"
  if date -u -d "$iso" +%s >/dev/null 2>&1; then
    date -u -d "$iso" +%s
  else
    # BSD date — strip Z, parse explicit format
    date -u -j -f "%Y-%m-%dT%H:%M:%SZ" "${iso%Z}Z" +%s
  fi
}

# curl wrapper that captures status code separately and retries once on network failure
api_call() {
  local method="$1" path="$2"
  local url="${AGENTMAIL_API}${path}"
  local attempt
  for attempt in 1 2; do
    local resp status
    resp=$(curl -sS -X "$method" "$url" \
      -H "Accept: application/json" \
      -w "\n__STATUS__%{http_code}" 2>&1) && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
      status="${resp##*__STATUS__}"
      body="${resp%__STATUS__*}"
      # Print "status<TAB>body" so caller can split
      printf "%s\t%s\n" "$status" "$body"
      return 0
    fi
    [ "$attempt" -eq 1 ] && sleep 2
  done
  echo -e "000\t{\"error\":{\"code\":\"network\",\"message\":\"curl failed after retry\"}}"
  return 1
}
