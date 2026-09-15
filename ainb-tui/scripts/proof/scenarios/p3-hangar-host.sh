# shellcheck shell=bash
# P3 hangar host and daemons (PR #1008): the Hangar plugin's screen actions go
# through dispatch and its ui.state view, and a daemon verb is an Effect whose
# result comes back as a report.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="ctrl+p fleet then ctrl+p control switch the Hangar plugin's screen by name; on the Daemons screen, start on the headroom proxy row runs the verb and the row reports 'headroom proxy started' with a pid that answers /health"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  check "ctrl+p fleet reaches the Fleet screen" open_hangar_screen tui fleet 'ACTION QUEUE'
  capture tui hangar-fleet
  check "ctrl+p control reaches the Control Center" open_hangar_screen tui control 'Control  ·'
  capture tui hangar-control
  local i
  for i in 1 2 3 4 5; do
    pane_text tui | grep -qE 'Stats +\[i\]' && break
    keys tui Escape
    sleep 0.7
    pane_text tui | grep -qE 'Stats +\[i\]' && break
    keys tui q
    sleep 0.7
  done

  keys tui d
  check "d opens the Daemons screen" wait_screen tui 'Daemons +runtime health' 10
  capture tui daemons-before
  for _ in $(seq 1 12); do
    pane_text tui | grep -q '▶ headroom proxy' && break
    keys tui Down
  done
  check "the cursor reaches the headroom proxy row" bash -c "tmux -L proof capture-pane -t '=tui:' -p | grep -q '▶ headroom proxy'"
  keys tui Enter
  check "Enter offers the verbs" wait_screen tui '▶ start' 5
  capture tui daemons-verbs
  keys tui Enter
  check "the row reports the verb's result" wait_screen tui 'headroom proxy started' 15
  local pid_wait
  for _ in 1 2 3 4 5 6; do
    pane_text tui | grep -qE 'headroom proxy +process +● running' && break
    keys tui r
    sleep 2
  done
  capture tui daemons-after
  local pid
  pid="$(cat "$HOME/.agents-in-a-box/headroom/proxy.pid" 2>/dev/null)"
  observe "proxy pid file: ${pid:-none}; row: $(pane_text tui | grep -oE 'headroom proxy +process +● running +[0-9]+' | head -1)"
  check "the row's pid is the proxy's pid" bash -c "tmux -L proof capture-pane -t '=tui:' -p | grep -qE 'headroom proxy +process +● running +${pid:-x} '"
  check "that proxy answers /health" curl -fsS -o /dev/null "http://127.0.0.1:$PROOF_HEADROOM_PORT/health"
}
