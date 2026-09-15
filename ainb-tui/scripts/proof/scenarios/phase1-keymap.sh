# shellcheck shell=bash
# Phase 1 keymap (G6 steps 1 to 3): TOML overrides reach the dispatcher, the
# interactive embed passes ctrl+c to the agent, and the help overlay agrees
# with the generated keymap.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with [session_list] attach = \"o\", keymap list prints attach -> o, enter no longer attaches and o does; in the interactive embed ctrl+c reaches the agent without quitting the TUI and ctrl+q releases; the help overlay agrees with keymap list on n, a, f, F, g and p"

# overlay_has <key column> <description regex>: a row of the help overlay.
overlay_has() { grep -qE "│ +$1 +$2" "$NODE_DIR/help-overlay.txt"; }
keymap_has() { "$AINB_BIN" keymap list --format json | jq -e --arg c "$1" --arg e "$2" \
  'any(.[]; .context == "session_list" and .chord == $c and .event == $e)' >/dev/null; }

# The live preview holds a read-only tmux client on the fixture session (P4's
# terminal observer), so an attach is a client WITHOUT the read-only flag.
fixture_clients() {
  ftmux list-clients -t "=$FIXTURE_TMUX" -F '#{client_flags}' 2>/dev/null | grep -vc 'read-only'
}

scenario() {
  fixture_session || { check "the fixture session starts" false; return; }

  # Step 3 first, on the default keymap: the overlay against the generated table.
  start_tui help || { check "the TUI reaches the home screen" false; return; }
  keys help s
  wait_screen help 'Workspaces \(1\)' 20
  keys help '?'
  wait_screen help 'Help - Press' 10
  capture help help-overlay
  save_output keymap-list "$AINB_BIN" keymap list
  local pair
  for pair in 'n|new_session|New session' 'a|attach_tmux|Attach' 'f|refresh|Refresh workspaces' \
              'F|cycle_filter|Cycle session filter' 'g|git|Show git view' 'p|quick_commit|Commit'; do
    IFS='|' read -r chord event words <<<"$pair"
    local col="$chord"
    [[ "$chord" == F ]] && col='Shift\+F'
    check "help overlay and keymap list agree: $chord -> $event ($words)" \
      bash -c "$(declare -f overlay_has keymap_has); NODE_DIR='$NODE_DIR' AINB_BIN='$AINB_BIN'; overlay_has '$col' '$words' && keymap_has '$chord' '$event'"
  done
  if overlay_has d 'Daemons' && keymap_has d delete; then
    observe "known drift outside the spot check: overlay 'd Daemons', keymap 'd delete' (lane H, PR #964)"
  fi
  quit_tui help

  # Step 1: the override.
  printf '[session_list]\nattach = "o"\n' >"$HOME/.agents-in-a-box/keymap.toml"
  local line
  line="$("$AINB_BIN" keymap list --format json | jq -r '.[] | select(.context=="session_list" and .event=="attach") | "\(.event) -> \(.chord)"')"
  printf '%s\n' "$line" >"$NODE_DIR/keymap-attach.txt"
  CAPTURES+=("keymap-attach.txt")
  observe "keymap list: $line"
  check "keymap list prints exactly attach -> o" test "$line" = "attach -> o"

  start_tui tui || { check "the TUI restarts with the override" false; return; }
  keys tui s
  wait_screen tui 'agent tick' 20
  capture tui session-list
  keys tui Enter
  sleep 4
  save_output clients-after-enter ftmux list-clients -t "=$FIXTURE_TMUX" -F '#{client_tty} #{client_flags}'
  observe "writable tmux clients on the fixture 4 s after enter: $(fixture_clients)"
  check "enter no longer attaches" test "$(fixture_clients)" -eq 0
  capture tui after-enter
  keys tui o
  check "o attaches (a writable tmux client appears on the fixture session)" \
    wait_for 10 bash -c "[ \$(tmux list-clients -t '=$FIXTURE_TMUX' -F '#{client_flags}' | grep -vc read-only) -ge 1 ]"
  save_output clients-after-o ftmux list-clients -t "=$FIXTURE_TMUX" -F '#{client_tty} #{client_flags}'
  capture tui after-o-attached
  keys tui C-b d
  wait_screen tui 'Workspaces \(1\)' 15
  capture tui after-detach

  # Step 2: the interactive embed.
  local pid
  pid="$(tui_pid tui)"
  keys tui A
  check "A enters the interactive embed" wait_screen tui 'INTERACTIVE \| Ctrl\+Q release' 10
  capture tui interactive
  keys tui C-c
  check "ctrl+c reaches the fixture agent" wait_for 10 fixture_says 'AGENT GOT SIGINT'
  check "the TUI survives ctrl+c (same pid $pid)" kill -0 "$pid"
  capture tui interactive-after-ctrl-c
  keys tui C-q
  check "ctrl+q releases back to the preview strip" wait_screen tui 'preview │ ask │ err' 10
  check "no INTERACTIVE marker remains" wait_gone tui 'INTERACTIVE' 5
  capture tui released
}
