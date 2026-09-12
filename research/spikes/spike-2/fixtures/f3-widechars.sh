#!/bin/sh
# Wide characters: CJK, emoji with and without a variation selector, combining
# marks, a ZWJ sequence, a tag-sequence flag, a double-width glyph that fits
# exactly in the last two columns at 40 wide, one that straddles the margin and
# must wrap, and an overwrite of a wide cell's left half.
printf 'CJK: \344\275\240\345\245\275\344\270\226\347\225\214 tail\r\n'
printf 'EMO: \360\237\230\200 \360\237\232\200 \342\235\244\357\270\217 \342\235\244 \360\237\221\251\342\200\215\360\237\222\273 tail\r\n'
printf 'CMB: e\314\201 a\314\212 o\314\210 n\314\203 tail\r\n'
# 38 narrow columns, then a wide glyph: at 40 columns it fits exactly.
printf '%s' 'FIT :abcdefghijklmnopqrstuvwxyz0123456'
printf '\344\270\255 after\r\n'
# 39 narrow columns, then a wide glyph: at 40 columns it cannot fit and wraps.
printf '%s' 'STRD:abcdefghijklmnopqrstuvwxyz01234567'
printf '\344\270\255 after\r\n'
# Absolute placement from here on, so both viewport sizes address the same rows.
printf '\033[12;1HOVR: \344\270\255\346\226\207 done'
printf '\033[12;6HX'
printf '\033[14;1HTAG: \360\237\217\264\363\240\201\247\363\240\201\242\363\240\201\245\363\240\201\256\363\240\201\247\363\240\201\277 end'
printf '\033[16;1H'
sleep 0.5
