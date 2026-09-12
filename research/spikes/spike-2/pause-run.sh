#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Reproduce spike 7's pause/resume once, and measure what it does to the
# rendered grid: compare the emulator against tmux right after %continue with no
# re-snapshot, then again after seeding from capture-pane -e.
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
B=$SD/target/release/spike2
be=$1
T kill-session -t pz 2>/dev/null; sleep 0.3
T new-session -d -s pz -x 120 -y 40 -n w0 -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 \
  "sh $SD/fixtures/f10-flood.sh"
T set-option -g window-size manual; T resize-window -t pz -x 120 -y 40
P=$(T display -p -t pz '#{pane_id}')
cat > "$SD/steps-pause.txt" <<EOS
sleep 300
seed
sleep 8000
cmd refresh-client -A "$P:continue"
sleep 25000
capture after_continue_noreseed
seed
sleep 1000
capture after_reseed
EOS
SPIKE2_TRACE=1 SPIKE2_NO_RAW=1 SPIKE2_READ_DELAY_MS=5 timeout 180 "$B" live ainb-spike2 pz "$P" 120 40 "$be" \
  "$SD/steps-pause.txt" "$SD/out/pause-$be"
T kill-session -t pz 2>/dev/null
true
