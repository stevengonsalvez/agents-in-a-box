# shellcheck shell=bash
# P1 ainb-app extraction (PRs #973, #975, #982): one renderer-free dispatcher
# behind three intent kinds, a chord from the keymap table, a named command,
# and a pointer, all resolved through the one CommandId registry.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with [session_list] learnings = \"L\", L opens Learnings and m no longer does (Intent::Key through the table); :recall from the session list opens Learnings (Intent::Command by name); a click on a session row selects it (a pointer intent with no chord)"

scenario() {
  fixture_session || { check "the first fixture session starts" false; return; }
  fixture_session || { check "the second fixture session starts" false; return; }
  printf '[session_list]\nlearnings = "L"\n' >"$HOME/.agents-in-a-box/keymap.toml"
  save_output keymap-session-list "$AINB_BIN" keymap list --format json
  check "keymap list binds learnings to L" \
    jq -e 'any(.[]; .context == "session_list" and .event == "learnings" and .chord == "L")' "$NODE_DIR/keymap-session-list.txt"
  check "the pointer command select_row is a registry row with no chord" \
    jq -e 'any(.[]; .context == "session_list" and .event == "select_row" and .chord == null)' "$NODE_DIR/keymap-session-list.txt"

  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  open_session_list tui
  keys tui m
  sleep 2
  check "m no longer opens Learnings" wait_gone tui '🧠 Learnings' 2
  capture tui after-m
  keys tui L
  check "L opens Learnings" wait_screen tui '🧠 Learnings' 10
  capture tui after-L
  keys tui Escape
  wait_screen tui 'Workspaces \(' 10 || { keys tui s; wait_screen tui 'Workspaces \(' 10; }

  keys tui :
  sleep 0.5
  type_text tui recall
  keys tui Enter
  check ":recall opens Learnings by command name" wait_screen tui '🧠 Learnings' 10
  capture tui after-recall
  keys tui Escape
  wait_screen tui 'Workspaces \(' 10 || { keys tui s; wait_screen tui 'Workspaces \(' 10; }

  wait_screen tui '▶ 1 ' 5
  local row
  row="$(row_of tui '^│ +2 ☐')"
  [[ -z "$row" ]] && row="$(row_of tui ' 2 ☐ ')"
  observe "second session row on screen: ${row:-none}"
  click tui 12 "$row"
  check "a click on row 2 selects it" wait_screen tui '▶ 2 ' 5
  capture tui after-click
}
