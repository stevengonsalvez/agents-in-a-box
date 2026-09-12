#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
T kill-session -t ss 2>/dev/null; sleep 0.3
T new-session -d -s ss -x 120 -y 40 -n w0 -e TERM=xterm-256color -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 \
  "sh $SD/fixtures/f11-altlink.sh"
T set-option -g window-size manual; T set-option -g default-terminal xterm-256color
T resize-window -t ss -x 120 -y 40
sleep 3
P=$(T display -p -t ss '#{pane_id}')
echo "tmux says: alt=$(T display -p -t ss '#{alternate_on}')"
SPIKE2_NO_RAW=1 timeout 60 "$SD/target/release/spike2" live ainb-spike2 ss "$P" 120 40 ${BE:-wezterm14} \
  "$SD/steps-seedstate.txt" "$SD/out/seedstate"
T kill-session -t ss 2>/dev/null; true
