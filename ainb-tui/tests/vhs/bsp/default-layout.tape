# vhs tape: default BSP layout matches the legacy 40/60 split visually.
# Run via `/test-ainb --vhs default-layout`. See ~/.claude/skills/test-ainb/references/layers.md
# for sleep budgets (cold ainb home ~30s).

Output "tests/vhs/bsp/out/default-layout.gif"
Set FontSize 14
Set Width 1200
Set Height 600
Set Theme "Dracula"
Set Padding 20

Type "ainb"
Enter
Sleep 30s

# capture stationary frame
Screenshot "tests/vhs/bsp/out/default-layout.png"

# graceful quit
Type "q"
Sleep 1s
