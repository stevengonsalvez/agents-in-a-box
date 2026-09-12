#!/bin/sh
# Bounded flood of a monotonic counter, so a viewer's loss is measurable token
# by token, followed by a static tail. 12 s at full pty speed.
timeout 6 awk 'BEGIN{for(i=1;i<=200000000;i++) printf "%09d%s", i, (i%12==0?"\n":" ")}'
printf '\r\nFLOOD-END\r\n'
sleep 90
