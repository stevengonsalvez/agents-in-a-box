# shellcheck shell=bash
# #963 (PR #998): a running TUI holds one live daemon connection from startup,
# on the home screen, and it is gone once the TUI quits.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a TUI parked on the home screen is listed as a tui row with its own pid at once, and the row is gone after it quits"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  local pid
  pid="$(tui_pid tui)"
  observe "TUI pid $pid, left on the home screen"

  check "a tui row with the TUI's pid appears within 5 s" \
    wait_for 5 rows_is tui "$pid" ge 1
  save_output connections-while-running "$AINB_BIN" hangar connections list
  capture tui home-screen
  observe "tui rows at pid $pid while running: $(count_kind tui "$pid")"

  quit_tui tui
  check "the TUI process exits on ctrl+c" bash -c "! kill -0 $pid 2>/dev/null"
  check "no tui row names the TUI's pid within 10 s of quitting" \
    wait_for 10 rows_is tui "$pid" eq 0
  save_output connections-after-quit "$AINB_BIN" hangar connections list
}
