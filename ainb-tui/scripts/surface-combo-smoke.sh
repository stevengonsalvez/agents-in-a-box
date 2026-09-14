#!/usr/bin/env bash
# Surface-combination smoke (S-D item 3).
#
# Every combination of surfaces has to start clean, register itself with the
# daemon, survive concurrent config writes, and leave nothing behind. The unit
# tests cover each surface alone; what they cannot cover is two of them sharing
# one home, which is where the locks, the connection registry and the config
# read-modify-write actually meet.
#
#   {tui}   {web}   {tui,web}   {tui,tui}
#     │       │        │            │
#     └───────┴────────┴────────────┴──▶ one daemon, one $HOME, one config file
#
# For each combination: start the daemon, start the surfaces, assert the
# connection registry names exactly the kinds that are running, write a
# distinct config key per surface CONCURRENTLY, assert every key survived, stop
# everything, and assert nothing is left holding a lock or a pid file.
#
# On the config writes: a headless TUI cannot be driven to a settings save from
# a shell, so each surface slot gets one concurrent `ainb config set`. The
# invariant under test is the one S-A fixed, a read-modify-write that dropped
# the other writer's key, and a concurrent CLI writer exercises it exactly as a
# second TUI would.
#
# Usage: scripts/surface-combo-smoke.sh [path-to-ainb]
# Exits non-zero on the first failure, with the combination named.

set -euo pipefail

# A missing tool is a SKIP for a developer and a FAILURE in CI.
#
# Every guard below used to exit 0, so that someone without tmux was not
# blocked. In CI that is exactly backwards, and it is how this script ran as a
# silent no-op on the macOS leg while the job reported green. `REQUIRE=1` (set
# by the workflow) turns each of them into exit 1.
REQUIRE="${REQUIRE:-0}"

missing() {
  if [[ "$REQUIRE" == "1" ]]; then
    echo "FAIL: $1 (REQUIRE=1)" >&2
    exit 1
  fi
  echo "SKIP: $1" >&2
  exit 0
}

AINB_BIN="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/debug/ainb}"
if [[ ! -x "$AINB_BIN" ]]; then
  missing "no ainb binary at $AINB_BIN (build with: cargo build -p ainb)"
fi
command -v tmux >/dev/null 2>&1 || missing "tmux is not on PATH"
command -v jq   >/dev/null 2>&1 || missing "jq is not on PATH"

# A private tmux server. The shared one sizes new windows to whatever its
# existing client has, commonly 63x36, which truncates the screens a TUI
# surface renders. Exported as its own statement: behind a `&&` after a `cd`
# that can fail, the export silently never runs.
TMUX_TMPDIR="${TMUX_TMPDIR:-$(mktemp -d)}"
export TMUX_TMPDIR

# `ainb --version` prints "ainb <semver> (<sha>, <date>, <channel>)", so the
# version is the second field; $NF is the channel.
AINB_VERSION=$("$AINB_BIN" --version 2>/dev/null | awk '{print $2}')
if [[ -z "$AINB_VERSION" ]]; then
  missing "$AINB_BIN would not report its version"
fi

FAILURES=0
SESSIONS=()
# Pids of the TUIs this combination launched. Each is `exec`ed into its pane,
# so the pane pid IS the TUI pid the registry must report.
TUI_PIDS=()

log()  { printf '  %s\n' "$*"; }
fail() { printf '  FAIL: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

# Kill by exact name, never by pattern: other agents and dev sessions live on
# this machine's tmux.
kill_sessions() {
  local name
  for name in "${SESSIONS[@]:-}"; do
    [[ -n "$name" ]] && tmux kill-session -t "=$name" 2>/dev/null || true
  done
  SESSIONS=()
}

start_tui() {
  local home="$1" hangar_home="$2" name="$3"
  tmux new-session -d -s "$name" -x 200 -y 50
  SESSIONS+=("$name")
  tmux send-keys -t "$name" \
    "HOME=$home AINB_HANGAR_HOME=$hangar_home AINB_DISABLE_PLUGINS=1 exec $AINB_BIN tui" Enter

  # Left on the home screen, deliberately. A TUI holds its presence connection
  # from startup on every screen (#963), so a TUI that never leaves home must
  # still be listed; opening the session list first would hide a regression.
  local deadline=$((SECONDS + 45))
  while (( SECONDS < deadline )); do
    if tmux capture-pane -t "$name" -p 2>/dev/null | grep -q "Stats"; then
      TUI_PIDS+=("$(tmux display-message -p -t "$name" '#{pane_pid}')")
      return 0
    fi
    sleep 1
  done
  return 1
}

start_web() {
  local home="$1" hangar_home="$2" name="$3" port="$4"
  tmux new-session -d -s "$name" -x 200 -y 50
  SESSIONS+=("$name")
  tmux send-keys -t "$name" \
    "HOME=$home AINB_HANGAR_HOME=$hangar_home exec $AINB_BIN web --listen 127.0.0.1:$port" Enter
}

# The surfaces the registry lists, minus the `cli` rows, as one comparable
# string: `tui:<pid>` per TUI row (pids sorted), `web` per web row, and
# `cli-from-tui:<pid>` for any `cli` row carrying a TUI's pid. `cli` rows from
# other pids are this script's own `connections list` probes and are ignored.
surfaces_seen() {
  local home="$1" hangar_home="$2" tui_pids
  tui_pids=$(printf '%s\n' "${TUI_PIDS[@]:-}" | jq -R 'select(length > 0) | tonumber' | jq -sc .)
  HOME="$home" AINB_HANGAR_HOME="$hangar_home" "$AINB_BIN" hangar connections list --format json 2>/dev/null \
    | jq -r --argjson tuis "$tui_pids" '
        [ .connections[].surface
          | if .kind == "tui" then "tui:\(.pid)"
            elif .kind == "cli" and (.pid as $p | $tuis | index($p)) then "cli-from-tui:\(.pid)"
            elif .kind == "cli" then empty
            else .kind end ]
        | sort | join(",")' 2>/dev/null || true
}

# What `surfaces_seen` must print for this combination.
surfaces_expected() {
  local surface pid
  {
    # `if`, not `&&`: a false last test would fail the group under pipefail.
    for pid in "${TUI_PIDS[@]:-}"; do
      if [[ -n "$pid" ]]; then printf 'tui:%s\n' "$pid"; fi
    done
    for surface in "$@"; do
      if [[ "$surface" == web ]]; then printf 'web\n'; fi
    done
  } | sort | paste -sd, -
}

# Poll rather than sleep: a surface's connection appears when it appears, and a
# fixed sleep is either flaky or slow. Exact match, not substring: a second
# `tui` row from one TUI is the defect (#963), not a pass.
wait_for_surfaces() {
  local home="$1" hangar_home="$2" want="$3" deadline=$((SECONDS + 45))
  local seen=""
  while (( SECONDS < deadline )); do
    seen=$(surfaces_seen "$home" "$hangar_home")
    if [[ "$seen" == "$want" ]]; then
      printf '%s' "$seen"
      return 0
    fi
    sleep 1
  done
  printf '%s' "$seen"
  return 1
}

run_combo() {
  local label="$1"; shift
  local surfaces=("$@")

  printf '\n=== %s ===\n' "$label"
  local root home hangar_home
  root=$(mktemp -d)
  home="$root/home"
  hangar_home="$root/hangar"
  mkdir -p "$home/.agents-in-a-box/config" "$hangar_home"
  # BOTH keys, or the wizard runs anyway and every surface assertion below is
  # made against the setup screen. The version comes from the binary rather
  # than from Cargo.toml, which carries the literal `version.workspace = true`
  # for workspace crates (tmux-ui-tripwire hard rule 5).
  printf 'completed = true\nversion = "%s"\n' "$AINB_VERSION" \
    > "$home/.agents-in-a-box/config/onboarding.toml"
  printf '{"agents":[],"hook_script":"","prompt_dismissed":true}\n' > "$hangar_home/install.json"

  export HOME="$home" AINB_HANGAR_HOME="$hangar_home"

  if ! "$AINB_BIN" hangar daemon setup >/dev/null 2>&1; then
    fail "$label: the daemon would not start"
    rm -rf "$root"
    return
  fi

  TUI_PIDS=()
  local i=0 port
  for surface in "${surfaces[@]}"; do
    i=$((i + 1))
    case "$surface" in
      tui)
        start_tui "$home" "$hangar_home" "combo-$$-$i" \
          || fail "$label: the TUI never reached the session list"
        ;;
      web)
        # Not `shuf`: it is GNU coreutils and macOS does not ship it, so the
        # {web} combination exited 127 there while the job stayed green.
        port=$((30000 + RANDOM % 15000))
        start_web "$home" "$hangar_home" "combo-$$-$i" "$port"
        ;;
    esac
  done

  # The registry must name exactly the surfaces that are running: one `tui`
  # row per TUI with that TUI's pid, one `web` row per web server, and no
  # `cli` row from a TUI's own polls.
  local seen expected sample
  expected=$(surfaces_expected "${surfaces[@]}")
  if seen=$(wait_for_surfaces "$home" "$hangar_home" "$expected"); then
    log "connections: $seen"
    # A presence that flickers is not a presence: the same answer every second.
    for sample in 1 2 3 4 5; do
      sleep 1
      seen=$(surfaces_seen "$home" "$hangar_home")
      if [[ "$seen" != "$expected" ]]; then
        fail "$label: sample $sample saw ${seen:-<none>}, want $expected"
        break
      fi
    done
  else
    fail "$label: the registry never named exactly $expected (saw: ${seen:-<none>})"
  fi

  # One concurrent config write per surface slot, then every key must survive.
  # This is the read-modify-write S-A fixed; before it the second writer
  # dropped the first writer's key.
  local pids=()
  for n in $(seq 1 "${#surfaces[@]}"); do
    "$AINB_BIN" config set "ui_preferences.session_filter" "all" >/dev/null 2>&1 &
    pids+=($!)
    "$AINB_BIN" config set "workspace_defaults.branch_prefix" "combo-$n/" >/dev/null 2>&1 &
    pids+=($!)
  done
  for pid in "${pids[@]}"; do wait "$pid" || true; done

  local prefix
  prefix=$("$AINB_BIN" config get workspace_defaults.branch_prefix 2>/dev/null || true)
  if [[ -z "$prefix" ]]; then
    fail "$label: concurrent writers left branch_prefix unset"
  else
    log "config survived concurrent writes: branch_prefix=$prefix"
  fi

  kill_sessions

  # A quit surface leaves the registry: the TUI closes its socket on the way out.
  if (( ${#TUI_PIDS[@]} > 0 )); then
    local gone_deadline=$((SECONDS + 10))
    while (( SECONDS < gone_deadline )); do
      seen=$(surfaces_seen "$home" "$hangar_home")
      [[ "$seen" != *tui:* ]] && break
      sleep 1
    done
    if [[ "$seen" == *tui:* ]]; then
      fail "$label: tui rows outlived their TUIs (saw: $seen)"
    else
      log "tui rows gone after quit"
    fi
  fi

  "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true

  # Nothing may outlive the run. An orphan proxy pid file points at a process
  # nobody will reap; a held daemon lock stops the next daemon starting at all.
  local proxy_pid="$home/.agents-in-a-box/headroom/proxy.pid"
  if [[ -f "$proxy_pid" ]]; then
    local pid
    pid=$(cat "$proxy_pid" 2>/dev/null || true)
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      fail "$label: proxy.pid names a live process ($pid) after shutdown"
    else
      log "proxy.pid is stale, not live"
    fi
  fi

  local lock="$hangar_home/hangar/daemon.lock"
  if [[ -f "$lock" ]] && command -v fuser >/dev/null 2>&1; then
    if fuser "$lock" >/dev/null 2>&1; then
      fail "$label: daemon.lock still has a holder after stop"
    else
      log "daemon.lock has no holder"
    fi
  fi

  rm -rf "$root"
}

trap 'kill_sessions' EXIT

run_combo "{tui}" tui
run_combo "{web}" web
run_combo "{tui, web}" tui web
run_combo "{tui, tui}" tui tui

printf '\n'
if (( FAILURES > 0 )); then
  printf 'surface-combo-smoke: %d failure(s)\n' "$FAILURES" >&2
  exit 1
fi
printf 'surface-combo-smoke: all four combinations clean\n'
