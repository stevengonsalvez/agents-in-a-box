# shellcheck shell=bash
# Phase 3 UiState (PR #945, G6 steps 8 and 10): hit-test rects live in
# UiState and scroll keys are UiActions that never fall through to the
# session list or to quit.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a single click on a home sidebar item selects it and a double click opens it; a click on the session list legend collapses it and a click on the collapsed hint restores it; shift+up on a live preview says it has no scrollback without moving the session cursor off row 2, and esc does not quit"

scenario() {
  fixture_session || { check "the first fixture session starts" false; return; }
  fixture_session || { check "the second fixture session starts" false; return; }
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  local pid row
  pid="$(tui_pid tui)"

  # Step 10, sidebar.
  row="$(row_of tui 'Stats +\[i\]')"
  click tui 8 "$row"
  check "a single click moves the sidebar selection to Stats" \
    wait_for 5 bash -c "tmux -L proof capture-pane -t '=tui:' -p | sed -n '${row}p' | grep -q '^█'"
  check "a single click does not open Stats" wait_gone tui 'Usage Analytics' 2
  capture tui sidebar-clicked
  double_click tui 8 "$row"
  check "a double click opens Stats, as enter would" wait_screen tui 'Usage Analytics' 10
  capture tui sidebar-double-clicked
  keys tui Escape
  wait_screen tui 'Stats +\[i\]' 10

  # Step 10, legend.
  open_session_list tui
  row="$(row_of tui 'Session actions')"
  click tui 40 "$((row + 1))"
  check "a click on the legend collapses it" wait_gone tui 'Session actions' 5
  capture tui legend-collapsed
  row="$(pane_text tui | grep -nE '⇧M expand/collapse' | tail -1 | cut -d: -f1)"
  click tui 70 "$row"
  check "a click on the collapsed hint row restores it" wait_screen tui 'Session actions' 5
  capture tui legend-restored

  # Step 8 on today's live preview.
  keys tui Down
  wait_screen tui '▶ 2 ' 5
  keys tui S-Up
  check "shift+up on a live preview says it has no scrollback" \
    wait_screen tui 'Live preview has no scrollback' 5
  capture tui shift-up
  check "the session cursor stays on row 2 (shift+up is not previous)" \
    bash -c "tmux -L proof capture-pane -t '=tui:' -p | grep -q '▶ 2 ' && ! tmux -L proof capture-pane -t '=tui:' -p | grep -q '▶ 1 '"
  keys tui Escape
  sleep 2
  check "esc does not quit the TUI" kill -0 "$pid"
  capture tui after-esc
  observe "G6 step 8's scrollback now needs A (interactive): live previews are read-only since 91c28f712"
}
