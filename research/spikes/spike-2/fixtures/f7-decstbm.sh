#!/bin/sh
# Minimal DECSTBM probe: after setting a scroll region the cursor must move to
# the home position. If an emulator leaves the cursor where it was, MARK lands
# on row 6 instead of row 1.
printf '\033[2J\033[H'
printf '\033[6;1H'
printf '\033[6;18r'
printf 'MARK'
printf '\033[20;1H'
sleep 0.5
