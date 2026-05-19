# vhs tape: mouse-drag a tile border to resize. vhs 0.11 doesn't expose
# MouseDown / MouseMove / MouseUp directives, so this tape only captures
# the static rendering of two-pane state. End-to-end drag-resize gif
# requires either a vhs version with mouse-event support OR manual
# capture via screen-recorder once p6-wiring lands.

Output "tests/vhs/bsp/out/drag-resize.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

Screenshot "tests/vhs/bsp/out/drag-resize.png"
Type "q"
Sleep 1s
