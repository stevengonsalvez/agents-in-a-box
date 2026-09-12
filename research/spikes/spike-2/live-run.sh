#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
fx=$1; steps=$2; tag=$3
for be in vt100 alacritty wezterm wezterm14; do
  T kill-session -t live 2>/dev/null
  T new-session -d -s live -x 120 -y 40 -n w0 -e TERM=xterm-256color -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 \
    "sh -c 'sh $SD/fixtures/$fx.sh'"
  T set-option -g window-size manual; T set-option -g default-terminal xterm-256color
  T resize-window -t live -x 120 -y 40
  sleep 3
  P=$(T display -p -t live '#{pane_id}')
  timeout 90 $SD/target/release/spike2 live ainb-spike2 live "$P" 120 40 "$be" "$SD/$steps" "$SD/out/live-$tag-$be"
done
T kill-session -t live 2>/dev/null
