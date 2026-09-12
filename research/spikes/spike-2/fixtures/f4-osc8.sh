#!/bin/sh
# OSC 8 hyperlinks: BEL and ST terminated, with and without an id, two adjacent
# links, one link that runs past the right margin, and a closed link followed
# by plain text.
printf 'links:\r\n'
printf '\033]8;;https://example.invalid/a\033\\ALPHA\033]8;;\033\\ plain\r\n'
printf '\033]8;id=x1;https://example.invalid/b\007BRAVO\033]8;;\007 plain\r\n'
printf '\033]8;;https://example.invalid/c\033\\CHAR\033]8;;https://example.invalid/d\033\\DELTA\033]8;;\033\\\r\n'
printf '\033]8;;https://example.invalid/long\033\\'
printf 'LONGLINK-abcdefghijklmnopqrstuvwxyz-0123456789-abcdefghijklmnopqrstuvwxyz'
printf '\033]8;;\033\\ end\r\n'
printf 'no link here\r\n'
sleep 0.5
