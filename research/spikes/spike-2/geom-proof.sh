#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Does the daemon's control client move the window out from under a second,
# ordinary attached client? Four cells: window-size manual or default, control
# client sizing itself with refresh-client -C or leaving its default.
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
B=$SD/target/release/spike2

cell() {
  wsmode=$1; ctlsize=$2
  T kill-session -t geo 2>/dev/null; sleep 0.3
  T new-session -d -s geo -x 120 -y 40 -n w0 'sleep 60'
  T set-option -g window-size "$wsmode"
  [ "$wsmode" = manual ] && T resize-window -t geo -x 120 -y 40
  P=$(T display -p -t geo '#{pane_id}')

  # A second, ordinary client at 100x30: the human with `tmux attach`.
  "$B" attach-client ainb-spike2 geo 100 30 22 >/dev/null &
  APID=$!
  sleep 1.5
  CLI=$(T list-clients -t geo -F '#{client_name}' | head -1)
  before=$(T display -p -c "$CLI" '#{window_width}x#{window_height}')

  if [ "$ctlsize" = "sized" ]; then
    cmds=$(printf 'refresh-client -C 120x40\nrefresh-client -f pause-after=2\nrefresh-client -A "%s:on"\n' "$P")
  else
    cmds=$(printf 'refresh-client -f pause-after=2\nrefresh-client -A "%s:on"\n' "$P")
  fi
  ( printf '%s\n' "$cmds"; sleep 8 ) | timeout 12 tmux -L ainb-spike2 -f /dev/null -C attach-session -t geo >/dev/null 2>&1 &
  CPID=$!
  sleep 2
  during=$(T display -p -c "$CLI" '#{window_width}x#{window_height}')
  nclients=$(T list-clients -t geo | wc -l)
  sleep 2
  during2=$(T display -p -c "$CLI" '#{window_width}x#{window_height}')
  wait $CPID 2>/dev/null
  sleep 1.5
  after=$(T display -p -c "$CLI" '#{window_width}x#{window_height}')
  printf '%-8s %-7s %-9s %-9s %-9s %-9s %s\n' "$wsmode" "$ctlsize" "$before" "$during" "$during2" "$after" "clients=$nclients"
  kill $APID 2>/dev/null
  wait $APID 2>/dev/null
  T kill-session -t geo 2>/dev/null
}

printf '%-8s %-7s %-9s %-9s %-9s %-9s %s\n' window-size ctl-C before during during2 'after detach' note
cell manual sized
cell manual unsized
cell latest sized
cell latest unsized

true
