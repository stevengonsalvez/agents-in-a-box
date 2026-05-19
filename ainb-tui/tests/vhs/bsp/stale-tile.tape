# vhs tape: restore a layout that references an unknown / removed
# component; placeholder tile renders the "⚠ unknown component" message.
# Requires p7-wiring follow-up for the placeholder render path.
#
# PRE-STEP (run by hand before recording):
#   mkdir -p ~/.agents-in-a-box
#   printf '%s\n' '{"version":1,"root":{"kind":"pane","id":"removed_component","focused":true},"min_cols":30,"min_rows":10}' \
#     > ~/.agents-in-a-box/layout.json

Output "tests/vhs/bsp/out/stale-tile.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

Screenshot "tests/vhs/bsp/out/stale-tile.png"
Type "q"
Sleep 1s
