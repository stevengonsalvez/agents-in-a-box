#!/bin/sh
# Column-advance probe. Each row prints one grapheme at column 1 followed by a
# marker; the marker's column is that grapheme's cell advance plus one.
printf '\033[2J\033[H'
printf '\033[1;1HA|'
printf '\033[2;1H\344\270\255|'
printf '\033[3;1H\360\237\230\200|'
printf '\033[4;1H\342\235\244|'
printf '\033[5;1H\342\235\244\357\270\217|'
printf '\033[6;1H\342\235\244\357\270\216|'
printf '\033[7;1He\314\201|'
printf '\033[8;1H\360\237\221\251\342\200\215\360\237\222\273|'
printf '\033[9;1H\360\237\217\264\363\240\201\247\363\240\201\242\363\240\201\245\363\240\201\256\363\240\201\247\363\240\201\277|'
printf '\033[10;1H\357\275\261|'
printf '\033[11;1H\342\226\210|'
printf '\033[12;1H\342\224\200|'
printf '\033[14;1H'
sleep 0.5
