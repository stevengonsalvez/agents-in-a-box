#!/bin/sh
# Plain scrolling: more lines than either viewport is tall, so the live window
# and the wrap of over-long lines are both exercised.
i=1
while [ $i -le 60 ]; do
  printf '%03d ' $i
  printf 'lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor\r\n'
  i=$((i + 1))
done
printf 'SCROLL-END\r\n'
sleep 0.5
