# vhs tape: split layout, quit, relaunch, verify the layout restored.
# Depends on p7-wiring follow-up to actually run end-to-end.

Output "tests/vhs/bsp/out/persist-restore.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

# Split
Ctrl+w
Sleep 200ms
Type "v"
Sleep 2s

# Quit (save expected on shutdown)
Type "q"
Sleep 2s

# Relaunch — layout should be restored
Type "ainb"
Enter
Sleep 30s

Screenshot "tests/vhs/bsp/out/persist-restore.png"
Type "q"
Sleep 1s
