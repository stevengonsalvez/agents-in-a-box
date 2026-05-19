# vhs tape: Ctrl+W v splits the focused tile vertically (stacks).
# Requires BSP wiring under follow-up bead p3-wiring; until that lands,
# the chord is a no-op and the tape captures the unchanged layout.

Output "tests/vhs/bsp/out/split-vertical.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

# Ctrl+W v
Ctrl+w
Sleep 200ms
Type "v"
Sleep 2s

Screenshot "tests/vhs/bsp/out/split-vertical.png"
Type "q"
Sleep 1s
