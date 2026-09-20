# shellcheck shell=bash
# S-A locks and atomic writes (G6 steps 4 and 5): two TUIs writing different
# settings both survive, config search filters as typed, and a second TUI
# neither spawns a second headroom proxy nor stops the first one's.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="two running TUIs each change a different setting through the Config screen and both values survive a restart; typing in config search narrows the matches to the typed key; with a headroom session live in TUI A, TUI B leaves the proxy pid unchanged, logs no spawn, and quitting B leaves A's proxy running"

CONFIG_TOML="config/config.toml"

# config_edit <session> <search text> <value>: `o`, `/`, type, enter on the
# first match, clear the field, type the value, enter to save.
config_edit() {
  local session="$1" search="$2" value="$3"
  keys "$session" o
  wait_screen "$session" 'Configuration +\([0-9]+ settings\)' 15 || return 1
  keys "$session" /
  type_text "$session" "$search"
  wait_screen "$session" "Search $search.*\\([0-9]+ match" 10 || return 1
  capture "$session" "search-$search"
  keys "$session" Enter
  wait_screen "$session" 'Enter save \| Esc cancel' 10 || return 1
  for _ in $(seq 1 24); do ptmux send-keys -t "=$session:" BSpace; done
  type_text "$session" "$value"
  keys "$session" Enter
  wait_screen "$session" 'Saved 1 setting' 10
}

config_value() { "$AINB_BIN" config get "$1" 2>/dev/null | tail -1; }

# mark_headroom_enabled <sessions.json>: set headroom_enabled on every session
# in the mirror and in the daemon's table.
#
# The table is the source a surface reads (P6e-6), and a reconcile pass never
# overwrites a table row from its file row, so the mirror alone would leave the
# watchdog reading `false` for ever.
mark_headroom_enabled() {
  local store="$1"
  jq '.sessions |= map_values(.headroom_enabled = true)' "$store" >"$store.tmp" \
    && mv "$store.tmp" "$store" || return 1
  jq -e '[.sessions[] | .headroom_enabled] | length > 0 and all' "$store" >/dev/null || return 1
  # The same row through the daemon, field for field as the wire carries it.
  local entry
  entry="$(jq -c '.sessions | to_entries[0].value | {
    session_id, tmux_session_name, worktree_path, workspace_name,
    created_at, agent_type,
    headroom_enabled: true,
    rtk_enabled: (.rtk_enabled // false),
    skip_permissions, model,
    model_source: (.model_source // "LegacyTyped"),
    codex_model, codex_thread_id
  }' "$store")" || return 1
  local reply
  reply="$(rpc_call 1 1 workspace/session_upsert "{\"session\": $entry}" 2>/dev/null | tail -1)"
  observe "headroom flag through the daemon: ${reply:-no reply}"
  [[ "$reply" == *'"ok":true'* || "$reply" == *'"result"'* ]]
}

scenario() {
  local cfg="$HOME/.agents-in-a-box/$CONFIG_TOML"

  # ---- Step 4: two writers, one config ------------------------------------
  start_tui a || { check "TUI A reaches the home screen" false; return; }
  start_tui b || { check "TUI B reaches the home screen" false; return; }
  observe "before: branch_prefix=$(config_value workspace_defaults.branch_prefix), scan_max_depth=$(config_value workspace_defaults.scan_max_depth)"

  check "TUI A saves workspace_defaults.branch_prefix = proofa/" \
    config_edit a branch_prefix proofa/
  capture a a-saved
  check "TUI B, still holding its startup snapshot, saves workspace_defaults.scan_max_depth = 4" \
    config_edit b scan_max_depth 4
  capture b b-saved
  grep -nE '^(branch_prefix|scan_max_depth) =' "$cfg" >"$NODE_DIR/config-toml-after-both.txt"
  CAPTURES+=("config-toml-after-both.txt")

  # Search narrows: the header count for `branch` is a handful, not 193.
  local matches
  matches="$(grep -oE 'Search branch_prefix.*\(([0-9]+) match' "$NODE_DIR/search-branch_prefix.txt" | grep -oE '\([0-9]+' | tr -d '(')"
  observe "config search 'branch_prefix': ${matches:-?} match(es)"
  check "config search narrows to the typed key (1 to 3 matches, first row is the key)" \
    bash -c "[[ '${matches:-0}' -ge 1 && '${matches:-0}' -le 3 ]] && grep -qE '▶ workspace_defaults.branch_prefix' '$NODE_DIR/search-branch_prefix.txt'"

  quit_tui a
  quit_tui b
  start_tui c || { check "a fresh TUI starts after both quit" false; return; }
  keys c o
  keys c /
  type_text c branch_prefix
  wait_screen c 'workspace_defaults.branch_prefix' 10
  capture c restart-branch-prefix
  quit_tui c
  observe "after restart: branch_prefix=$(config_value workspace_defaults.branch_prefix), scan_max_depth=$(config_value workspace_defaults.scan_max_depth)"
  check "A's setting survived B's write and a restart" test "$(config_value workspace_defaults.branch_prefix)" = "proofa/"
  check "B's setting survived too" test "$(config_value workspace_defaults.scan_max_depth)" = "4"
  check "the restarted Config screen shows A's value" \
    grep -q 'workspace_defaults.branch_prefix *: proofa/' "$NODE_DIR/restart-branch-prefix.txt"

  # ---- Step 5: one headroom proxy -----------------------------------------
  fixture_session || { check "the headroom fixture session starts" false; return; }
  local store="$HOME/.agents-in-a-box/sessions.json" pidfile="$HOME/.agents-in-a-box/headroom/proxy.pid"
  # Headroom is a launch-time choice with no CLI verb, so the proof sets the
  # flag itself. Since the flip the daemon's table is what a surface reads, so
  # the flag goes there as well as into the mirror: an edit to sessions.json
  # alone no longer reaches the watchdog, which is the point of the flip.
  check "the fixture session is marked headroom_enabled in the session store" \
    mark_headroom_enabled "$store"

  start_tui a || { check "TUI A restarts for the headroom step" false; return; }
  check "TUI A's watchdog starts the proxy (pid file within 30 s)" wait_for 30 test -s "$pidfile"
  local pid_a
  pid_a="$(cat "$pidfile" 2>/dev/null)"
  check "the proxy answers /health on its port" curl -fsS -o /dev/null "http://127.0.0.1:$PROOF_HEADROOM_PORT/health"
  observe "proxy pid with A running: ${pid_a:-none}"

  local logs_before
  logs_before="$(find "$HOME/.agents-in-a-box/logs" -name '*.jsonl' | sort)"
  sleep 1.2
  start_tui b || { check "TUI B starts beside A" false; return; }
  sleep 25
  local log_b
  log_b="$(comm -13 <(printf '%s\n' "$logs_before") <(find "$HOME/.agents-in-a-box/logs" -name '*.jsonl' | sort) | tail -1)"
  observe "proxy pid 25 s into B: $(cat "$pidfile" 2>/dev/null); B's log: ${log_b##*/}"
  check "the pid is unchanged while B runs" test "$(cat "$pidfile" 2>/dev/null)" = "$pid_a"
  check "B's log records no headroom spawn" \
    bash -c "[[ -n '$log_b' ]] && ! grep -q 'spawned headroom proxy' '$log_b'"
  # One proxy means one listener on the proxy port, whoever owns it.
  ss -ltnpH "sport = :$PROOF_HEADROOM_PORT" >"$NODE_DIR/headroom-listeners.txt" 2>&1 || true
  CAPTURES+=("headroom-listeners.txt")
  observe "listeners on the headroom port: $(grep -c LISTEN "$NODE_DIR/headroom-listeners.txt")"
  check "exactly one headroom proxy listens on its port" \
    test "$(grep -c LISTEN "$NODE_DIR/headroom-listeners.txt")" -eq 1

  quit_tui b
  sleep 12
  observe "proxy pid 12 s after B quit: $(cat "$pidfile" 2>/dev/null || echo none)"
  check "quitting B leaves A's proxy alive (same pid, still healthy)" \
    bash -c "kill -0 $pid_a && curl -fsS -o /dev/null http://127.0.0.1:$PROOF_HEADROOM_PORT/health"
  check "B's log records no SIGTERM to the proxy" \
    bash -c "! grep -q 'sent SIGTERM to headroom proxy' '$log_b'"
  if [[ -n "$log_b" ]]; then
    grep -h 'headroom' "$log_b" | redact_host >"$NODE_DIR/tui-b-headroom-log.txt" || true
    CAPTURES+=("tui-b-headroom-log.txt")
  fi
}
