#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# tmux's own VT model of the same fixture, the independent fidelity reference
# for the crate comparison. The pane is held open with a trailing sleep instead
# of remain-on-exit, because the "Pane is dead" banner scrolls the grid.
set -u
fx=$1; C=$2; R=$3; OUT=$4
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
T kill-session -t ref 2>/dev/null
T new-session -d -s ref -x "$C" -y "$R" -n w0 \
   -e TERM=xterm-256color -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 \
   "sh -c 'sh $SD/fixtures/$fx.sh; sleep 20'"
T set-option -g window-size manual
T set-option -g default-terminal xterm-256color
T resize-window -t ref -x "$C" -y "$R"
sleep 2
T capture-pane -p -N -t ref > "$OUT"
T capture-pane -p -N -e -t ref > "${OUT%.txt}-sgr.txt"
T display -p -t ref 'history_size=#{history_size} size=#{pane_width}x#{pane_height}' > "${OUT%.txt}-meta.txt"
T kill-session -t ref 2>/dev/null
