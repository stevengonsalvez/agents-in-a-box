#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
name=$1; cmdline=$2
for be in vt100 alacritty wezterm wezterm14; do
  T kill-session -t ag 2>/dev/null
  T new-session -d -s ag -x 120 -y 40 -n w0 -c "$SD/agentcwd" \
    -e TERM=xterm-256color -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 "$cmdline"
  T set-option -g window-size manual; T set-option -g default-terminal xterm-256color
  T resize-window -t ag -x 120 -y 40
  sleep 8
  P=$(T display -p -t ag '#{pane_id}')
  cat > "$SD/steps-agent.txt" <<EOS
seed
sleep 1200
capture s0
cmd send-keys -t $P Down
sleep 1200
capture s1
cmd send-keys -t $P Up
sleep 1500
capture s2
EOS
  echo "--- $name / $be ---"
  timeout 90 $SD/target/release/spike2 live ainb-spike2 ag "$P" 120 40 "$be" "$SD/steps-agent.txt" "$SD/out/live-$name-$be"
done
T kill-session -t ag 2>/dev/null
true
