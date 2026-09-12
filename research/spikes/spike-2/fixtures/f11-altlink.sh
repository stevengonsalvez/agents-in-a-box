#!/bin/sh
# Alt screen plus per-cell state that capture-pane -e cannot express: an OSC 8
# hyperlink, a window title, and mouse reporting. Used to measure what a
# capture-pane re-seed does and does not restore.
printf '\033]0;spike2 altlink\007'
printf '\033[?1049h\033[?25l\033[2J\033[H'
printf '\033[?1000h\033[?1002h\033[?1006h'
printf '\033[1;1H\033[38;5;39mstatic frame, painted once\033[0m'
printf '\033[2;1H\033]8;;https://example.invalid/seed\033\\SEEDLINK\033]8;;\033\\ plain'
i=1
while [ $i -le 200 ]; do
  printf '\033[4;1H\033[1mtick %04d\033[0m' $i
  i=$((i + 1))
  sleep 0.1
done
