#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Double-width glyph at the right margin. With N narrow columns already filled
# in a 60-column terminal, where does a following wide glyph land?
set -u
B=$SD/target/release/spike2
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
T kill-session -t mp 2>/dev/null
T new-session -d -s mp -x 60 -y 20 -n w0 -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 'sleep 120'
T set-option -g window-size manual; T resize-window -t mp -x 60 -y 20
TTY=$(T display -p -t mp '#{pane_tty}')
printf '%-10s %-22s %-22s %s\n' 'filled' 'tmux row1/row2' 'crate' 'row1 / row2'
for n in 57 58 59; do
  pad=$(printf '%*s' "$n" '' | tr ' ' 'x')
  f=$SD/out/mp-$n.bin
  printf '\033[2J\033[H' > "$f"; printf '%s' "$pad" >> "$f"; printf '\344\270\255ZZ' >> "$f"
  printf '\033[2J\033[H' > "$TTY"; printf '%s' "$pad" > "$TTY"; printf '\344\270\255ZZ' > "$TTY"
  sleep 0.15
  t1=$(T capture-pane -p -N -t mp | sed -n '1p'); t2=$(T capture-pane -p -N -t mp | sed -n '2p')
  printf '%-10s %-22s %-22s %s\n' "$n" "$(echo "$t1" | tail -c 8)/$(echo "$t2" | head -c 8)" "tmux" ""
  for be in vt100 alacritty wezterm wezterm14; do
    s=$SD/out/mp-$n-$be.snap
    timeout 30 "$B" snapshot "$f" 60 20 "$be" 1000 > "$s"
    r1=$(grep '^r000 t |' "$s" | sed 's/^r000 t |//; s/|$//' | tail -c 8)
    r2=$(grep '^r001 t |' "$s" | sed 's/^r001 t |//; s/|$//' | head -c 8)
    printf '%-10s %-22s %-22s %s\n' "" "" "$be" "$r1 / $r2"
  done
done
T kill-session -t mp 2>/dev/null
