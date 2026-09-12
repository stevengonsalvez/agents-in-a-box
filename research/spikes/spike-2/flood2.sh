#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -u
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
be=$1
mkdir -p "$SD/out"
# 50 MB of attributed agent-transcript content, regenerated on demand so the
# parked harness is self-contained and the file itself stays out of the repo.
[ -f "$SD/out/flood50.bin" ] || python3 "$SD/make-workload.py" --flood
GO=$SD/out/FLOODGO; rm -f "$GO"
T kill-session -t fl 2>/dev/null; sleep 0.3
T new-session -d -s fl -x 120 -y 40 -n w0 "sh -c 'while [ ! -f $GO ]; do sleep 0.05; done; cat $SD/out/flood50.bin; sleep 60'"
T set-option -g window-size manual; T resize-window -t fl -x 120 -y 40
P=$(T display -p -t fl '#{pane_id}'); SRV=$(T display -p '#{pid}')
cat > "$SD/steps-flood.txt" <<EOS
seed
sleep 22000
capture flood
EOS
( sleep 3; touch "$GO" ) &
# Sample the tmux server's own CPU while the flood is live; the server exits
# with the session at the end of the run, so a before/after read misses it.
( while [ -r /proc/$SRV/stat ]; do
    read -r _ _ _ _ _ _ _ _ _ _ _ _ _ u s _ < /proc/$SRV/stat
    echo "$u $s" > "$SD/out/srvcpu.$be"
    sleep 0.5
  done ) &
SAMP=$!
read -r _ _ _ _ _ _ _ _ _ _ _ _ _ ut0 st0 _ < /proc/$SRV/stat
S0=$(date +%s.%N)
SPIKE2_NO_RAW=1 timeout 90 /usr/bin/time -f "harness_user_s=%U harness_sys_s=%S harness_wall_s=%e harness_maxrss_kb=%M" \
  "$SD/target/release/spike2" live ainb-spike2 fl "$P" 120 40 "$be" "$SD/steps-flood.txt" "$SD/out/flood-$be"
rc=$?
S1=$(date +%s.%N)
kill $SAMP 2>/dev/null; wait $SAMP 2>/dev/null
read -r ut1 st1 < "$SD/out/srvcpu.$be"
echo "tmux_server_cpu_s=$(echo "scale=2; ($ut1-$ut0+$st1-$st0)/100" | bc)"
echo "rc=$rc wall_s=$(echo "scale=2; $S1-$S0" | bc)"
T kill-session -t fl 2>/dev/null; rm -f "$GO"; true
