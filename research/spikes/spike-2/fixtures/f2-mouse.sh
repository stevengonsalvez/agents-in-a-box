#!/bin/sh
# Mouse tracking: walks up through every DECSET reporting mode and leaves
# button-event tracking, SGR encoding, focus reporting, bracketed paste and
# DECCKM all enabled, so the final mode is unambiguous in a snapshot.
printf 'mouse fixture\r\n'
printf '\033[?1000h'; printf 'X10+click on\r\n'
printf '\033[?1005h'; printf 'utf8 ext on\r\n'
printf '\033[?1005l'; printf 'utf8 ext off\r\n'
printf '\033[?1002h'; printf 'btn-event on\r\n'
printf '\033[?1006h'; printf 'sgr ext on\r\n'
printf '\033[?1004h'; printf 'focus on\r\n'
printf '\033[?2004h'; printf 'bracketed paste on\r\n'
printf '\033[?1h'; printf 'application cursor on\r\n'
printf 'final: 1000 1002 1006 1004 2004 DECCKM\r\n'
sleep 0.5
