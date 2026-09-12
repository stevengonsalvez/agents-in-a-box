#!/bin/sh
# Alt-screen TUI: enters the alternate buffer, sets a scroll region, paints a
# framed agent-style panel with 16/256/truecolor runs and SGR attributes, then
# leaves the cursor parked inside the log region without ever leaving alt mode.
printf '\033]0;spike2 agent panel\007'
printf '\033[?1049h\033[?25l\033[2J\033[H'
printf '\033[1;1H\033[38;5;39m┌─ agent ───────────────────────────────┐\033[0m'
printf '\033[2;1H\033[38;5;39m│\033[0m \033[1mclaude\033[0m  \033[3mmodel\033[0m  \033[4mopus\033[0m  \033[7mWORKING\033[0m'
printf '\033[3;1H\033[38;5;39m│\033[0m \033[38;2;255;128;0mtruecolor orange\033[0m \033[48;5;236m dim bg \033[0m'
printf '\033[4;1H\033[38;5;39m└───────────────────────────────────────┘\033[0m'
printf '\033[6;1H'
printf '\033[6;18r'
i=1
while [ $i -le 24 ]; do
  printf '\033[38;5;%dmlog line %02d  the quick brown fox jumps over the lazy dog\033[0m\r\n' $((16 + i)) $i
  i=$((i + 1))
done
printf '\033[r'
printf '\033[20;1H\033[7m status: 24 lines emitted \033[0m'
printf '\033[10;12H'
printf '\033[?25h'
sleep 0.5
