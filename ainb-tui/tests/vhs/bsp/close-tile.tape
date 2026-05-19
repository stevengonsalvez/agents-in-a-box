# vhs tape: Ctrl+W x closes the focused tile, sibling promotes to fill.

Output "tests/vhs/bsp/out/close-tile.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

# First split so there's something to close
Ctrl+w
Sleep 200ms
Type "v"
Sleep 2s

# Then close the focused tile
Ctrl+w
Sleep 200ms
Type "x"
Sleep 2s

Screenshot "tests/vhs/bsp/out/close-tile.png"
Type "q"
Sleep 1s
