#!/bin/sh
# A long-lived alt-screen panel that repaints on a timer, so a control client
# attaching partway through still has traffic to tail.
printf '\033]0;spike2 live panel\007'
printf '\033[?1049h\033[?25l\033[2J\033[H'
printf '\033[1;1H\033[38;5;39m┌─ live agent ──────────────────────────┐\033[0m'
printf '\033[4;1H\033[38;5;39m└───────────────────────────────────────┘\033[0m'
i=1
while [ $i -le 200 ]; do
  printf '\033[2;3H\033[1mtick %04d\033[0m  \033[3mstate\033[0m \033[7mWORKING\033[0m  ' $i
  printf '\033[3;3H\033[38;5;%dmspinner %s\033[0m  \344\270\255\346\226\207 \360\237\232\200   ' $((16 + i % 200)) "$(printf '%s' '|/-\\' | cut -c$((i % 4 + 1)))"
  printf '\033[6;1H\033[38;5;244mlog %04d the quick brown fox jumps over the lazy dog\033[0m\r\n' $i
  i=$((i + 1))
  sleep 0.1
done
