# shellcheck shell=bash
# P6e concurrency: the TUI, `ainb web` and the CLI against ONE daemon, with the
# sessions capability on, each surface reading and writing the daemon's table
# rather than its own copy of sessions.json.
#
#   {tui} {web} {tui,web} {tui,tui}   each with a CLI leg
#         │       │          │
#         └───────┴──────────┴──▶ one daemon, one $HOME, one sessions table
#
# What it proves, per combination: a session the CLI creates reaches every
# surface that is up (for a TUI, on its next list reload, which is a key the
# operator presses, not a restart); a session killed from the CLI leaves every surface; and
# an ASK answered from one surface folds on the others. The capability is dark
# in a release build, so the binaries the harness builds carry `test-support`
# and this scenario turns it on with AINB_TEST_WORKSPACE_SESSIONS=1 (the
# daemon reads the same variable in rpc/auth.rs).
#
# Combination names match scripts/surface-combo-smoke.sh, which is where the
# four came from (P6e open question 5).

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with the sessions capability on, one daemon serves the TUI, ainb web and the CLI together in every combination ({tui} {web} {tui,web} {tui,tui}): a CLI-created session reaches every surface that is up, a CLI kill leaves every surface, and an ASK answered from one surface folds on the others"

# The capability, on for every process this scenario starts: the binaries the
# harness builds carry `test-support`, and a release build ignores it.
p6_capability_on() {
  export AINB_TEST_WORKSPACE_SESSIONS=1
}

# p6_binaries_ready: the CLI answers with the capability switch set. A harness
# build without `test-support` ignores the switch and stays on the file, which
# would prove the file path twice over rather than the table, so that is a
# failure here, not a pass.
p6_binaries_ready() {
  if ! "$AINB_BIN" list --format json >/dev/null 2>&1; then
    check "the CLI runs with AINB_TEST_WORKSPACE_SESSIONS=1" false
    return 1
  fi
  return 0
}

# p6_cli_sessions: the session ids `ainb list` reports, one per line.
p6_cli_sessions() {
  "$AINB_BIN" list --format json 2>/dev/null | jq -r '.[].session_id'
}

# p6_cli_has <id>: the CLI lists that session.
p6_cli_has() { p6_cli_sessions | grep -qF "$1"; }
# p6_cli_lacks <id>: the CLI no longer lists it.
p6_cli_lacks() { ! p6_cli_has "$1"; }

# p6_table_rows: the session ids the daemon's table holds, through the CLI's
# own daemon path (the capability is on, so this is a table read).
p6_table_rows() { p6_cli_sessions | sort; }

# p6_row_needle <tmux name>: what the TUI's session list actually shows for a
# session: not the tmux name, but the 8-hex suffix its branch carries
# (`tmux_repo-7d755792` runs on `agents/7d755792`).
p6_row_needle() { printf '%s' "${1##*-}"; }

# p6_tui_reload <session>: leave the session list and open it again, which is
# where the TUI reloads its workspaces. Same process, no restart: the reload
# is the operator's own key, and what it reads is the process's session source.
p6_tui_reload() {
  keys "$1" Escape
  sleep 0.5
  open_session_list "$1"
}

# p6_tui_has <session> <needle>: the TUI's session list shows it.
p6_tui_has() { pane_text "$1" | grep -qF -- "$2"; }
p6_tui_lacks() { ! p6_tui_has "$1" "$2"; }

# p6_web_has <needle>: the web snapshot names it.
p6_web_has() {
  curl -sS "$WEB_URL/api/snapshot" 2>/dev/null | jq -e --arg n "$1" \
    '[.sessions[]? | tostring | select(test($n))] | length > 0' >/dev/null
}
p6_web_lacks() { ! p6_web_has "$1"; }

# p6_combination <name> <tui count> <web:0|1>: one combination, end to end.
p6_combination() {
  local name="$1" tuis="$2" web="$3" i
  say "combination $name"
  observe "=== $name ==="

  # One daemon for this combination, started before any surface, so {web}
  # (which autostarts nothing) has the same daemon the TUIs would have.
  "$AINB_BIN" hangar daemon start >"$PROOF_WORLD/p6-daemon-$name.txt" 2>&1 || true
  if ! wait_for 45 daemon_running; then
    observe "$name: daemon start said $(tail -2 "$PROOF_WORLD/p6-daemon-$name.txt" | tr '\n' ' ')"
    check "$name: one daemon is up for every surface" false
    return 1
  fi
  check "$name: one daemon is up for every surface" true

  local sessions=()
  for ((i = 1; i <= tuis; i++)); do
    if ! start_tui "tui$i"; then
      check "$name: the TUI in tui$i reaches the home screen" false
      return 1
    fi
    sessions+=("tui$i")
    open_session_list "tui$i"
  done
  if [[ "$web" == "1" ]]; then
    start_web || { check "$name: ainb web answers" false; return 1; }
  fi

  # 1. The CLI creates a session; every surface that is up sees it.
  fixture_session || { check "$name: the CLI created a session" false; return 1; }
  local created="$FIXTURE_ID" tmux_name="$FIXTURE_TMUX" row
  row="$(p6_row_needle "$FIXTURE_TMUX")"
  observe "$name: the CLI created $tmux_name ($created), listed as $row"
  check "$name: the CLI lists the session it created" p6_cli_has "$created"
  for i in "${sessions[@]}"; do
    p6_tui_reload "$i"
    check "$name: the session the CLI created reached $i" \
      wait_for 60 p6_tui_has "$i" "$row"
  done
  if [[ "$web" == "1" ]]; then
    check "$name: the session the CLI created reached ainb web" \
      wait_for 180 p6_web_has "$tmux_name"
  fi

  # 2. An ASK answered on one surface folds on the others.
  if [[ "$web" == "1" ]]; then
    raise_ask "Proof P6e: $name?" "p6-$name" >/dev/null
    if web_card_id "Proof P6e" "p6-card-$name" 180; then
      web_answer "$WEB_CARD_ID" 1 >"$PROOF_WORLD/p6-answer-$name.json"
      observe "$name: web answered $(tr -d '\n' <"$PROOF_WORLD/p6-answer-$name.json")"
      check "$name: the web answer was delivered" \
        grep -q '"outcome": *"delivered"' "$PROOF_WORLD/p6-answer-$name.json"
      for i in "${sessions[@]}"; do
        check "$name: the answered card folded on $i" \
          wait_for 90 p6_tui_lacks "$i" "Proof P6e: $name?"
      done
    else
      check "$name: the web lists the card within 180 s" false
    fi
  fi

  # 3. The CLI kills the session; every surface drops it.
  "$AINB_BIN" kill "$created" --force >"$PROOF_WORLD/p6-kill-$name.txt" 2>&1 \
    || observe "$name: ainb kill said $(tail -1 "$PROOF_WORLD/p6-kill-$name.txt")"
  check "$name: the CLI no longer lists the killed session" wait_for 30 p6_cli_lacks "$created"
  for i in "${sessions[@]}"; do
    p6_tui_reload "$i"
    check "$name: the killed session left $i" wait_for 90 p6_tui_lacks "$i" "$row"
  done
  if [[ "$web" == "1" ]]; then
    check "$name: the killed session left ainb web" wait_for 180 p6_web_lacks "$tmux_name"
  fi

  # 4. Nothing the surfaces did left the stores disagreeing.
  local table file
  table="$(p6_table_rows | tr '\n' ' ')"
  file="$(jq -r '.sessions | keys[]?' "$HOME/.agents-in-a-box/sessions.json" 2>/dev/null | sort | tr '\n' ' ')"
  observe "$name: sessions the daemon serves: ${table:-none}"
  observe "$name: sessions.json keys: ${file:-none}"
  check "$name: the killed session is in neither store" \
    bash -c "! grep -qF '$tmux_name' <<<'$file'"

  # Close the surfaces this combination started, and its daemon, so the next
  # combination starts from the same place.
  for i in "${sessions[@]}"; do
    quit_tui "$i" 2>/dev/null || ptmux kill-session -t "=$i:" 2>/dev/null || true
  done
  if [[ "$web" == "1" ]]; then
    ptmux kill-session -t "=web:" 2>/dev/null || true
  fi
  "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true
  return 0
}

scenario() {
  p6_capability_on
  p6_binaries_ready || return

  # Each combination brings up one daemon and every surface it names against
  # it, which is the point of the node.
  p6_combination tui 1 0 || return
  p6_combination web 0 1 || return
  p6_combination tui-web 1 1 || return
  p6_combination tui-tui 2 0 || return

  if [[ -s "$HOME/.agents-in-a-box/sessions.json" ]]; then
    redact_host <"$HOME/.agents-in-a-box/sessions.json" >"$NODE_DIR/sessions-json.txt"
    CAPTURES+=("sessions-json.txt")
  fi
  save_output daemon-status "$AINB_BIN" hangar daemon status
}
