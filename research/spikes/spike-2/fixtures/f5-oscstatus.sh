#!/bin/sh
# Custom OSC 9999 status frame, both forms spike 1 settled on: a bare OSC and a
# tmux DCS-wrapped copy with the inner ESC doubled. Interleaved with visible
# text so a dropped frame and a corrupted grid are distinguishable.
n=1
while [ $n -le 6 ]; do
  printf 'HEARTBEAT %02d\r\n' $n
  printf '\033]9999;{"v":1,"state":"working","form":"bare","n":%d}\007' $n
  printf '\033Ptmux;\033\033]9999;{"v":1,"state":"working","form":"dcs","n":%d}\007\033\\' $n
  n=$((n + 1))
done
printf '\033]9999;{"v":1,"state":"idle","form":"bare","n":0}\007'
printf 'done\r\n'
sleep 0.5
