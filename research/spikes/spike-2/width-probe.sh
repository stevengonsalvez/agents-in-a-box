#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Exact cell advance per glyph. tmux is asked with #{cursor_x} after writing the
# glyph straight to the pane's tty; each candidate crate is asked with the same
# bytes through the harness. Both answers are cursor columns, so they compare.
set -u
B=$SD/target/release/spike2
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
declare -a LABEL BYTES
add() { LABEL+=("$1"); BYTES+=("$2"); }
add 'ASCII A'            'A'
add 'CJK U+4E2D'         '\344\270\255'
add 'emoji U+1F600'      '\360\237\230\200'
add 'U+2764 bare'        '\342\235\244'
add 'U+2764 + VS16'      '\342\235\244\357\270\217'
add 'U+2764 + VS15'      '\342\235\244\357\270\216'
add 'e + U+0301'         'e\314\201'
add 'ZWJ U+1F469...'     '\360\237\221\251\342\200\215\360\237\222\273'
add 'tag flag U+1F3F4'   '\360\237\217\264\363\240\201\247\363\240\201\242\363\240\201\245\363\240\201\256\363\240\201\247\363\240\201\277'
add 'halfwidth U+FF71'   '\357\275\261'
add 'block U+2588'       '\342\226\210'
add 'box U+2500'         '\342\224\200'
add 'U+00E9 precomposed' '\303\251'

T kill-session -t wp 2>/dev/null
T new-session -d -s wp -x 60 -y 20 -n w0 -e LANG=C.UTF-8 -e LC_ALL=C.UTF-8 'sleep 120'
T set-option -g window-size manual; T resize-window -t wp -x 60 -y 20
TTY=$(T display -p -t wp '#{pane_tty}')
printf '%-22s %6s %8s %10s %9s %10s\n' glyph tmux vt100 alacritty wezterm wezterm14
for i in "${!LABEL[@]}"; do
  printf '\033[2J\033[H' > "$TTY"
  printf "${BYTES[$i]}" > "$TTY"
  sleep 0.12
  tx=$(T display -p -t wp '#{cursor_x}')
  f=$SD/out/wp-$i.bin
  printf '\033[2J\033[H' > "$f"
  printf "${BYTES[$i]}" >> "$f"
  row=""
  for be in vt100 alacritty wezterm wezterm14; do
    row="$row $(timeout 30 "$B" cursor-col "$f" 60 20 "$be")"
  done
  printf '%-22s %6s %8s %10s %9s %10s\n' "${LABEL[$i]}" "$tx" $row
done
T kill-session -t wp 2>/dev/null
