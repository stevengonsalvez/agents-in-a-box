# vhs tape: Shift+A in-pane attach — the preview pane becomes a LIVE embedded
# tmux client ("● INTERACTIVE — Ctrl+Q release" badge; the sidebar stays exactly
# as the user has it, typed keys land in the session). Ctrl+Q releases; the
# session survives.
#
# Seed: identical to attach-fullscreen.tape (see that file's header) — isolated
# HOME + isolated tmux server (TMUX_TMPDIR) holding a single demo-agent session.
# Recreate the demo-agent session between recordings for a clean scrollback.
#
# Run from ainb-tui/:  vhs ../docs/assets/screenshots/attach-in-pane.tape
# Then optimise:       gifsicle -O3 --lossy=60 --colors 200 --resize-width 1100 -b <gif>
Output "../docs/assets/screenshots/attach-in-pane.gif"

Set Shell "bash"
Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1200
Set Height 800
Set Padding 8
Set TypingSpeed 5ms

Env HOME "/tmp/ainb-attach-demo/home"
Env TMUX_TMPDIR "/tmp/ainb-attach-demo/tmux"
Env AINB_DISABLE_PLUGINS "1"

# cold start hidden: launch, settle home, hop to the session list
Hide
Type "unset TMUX; exec ./target/debug/ainb tui"
Enter
Sleep 8s
Type "s"
Sleep 5s
Show

# session list, demo-agent selected under "Other tmux"
Sleep 2.5s

# Shift+A: the pane becomes the live embed (badge + session content; sidebar stays as-is)
Type "A"
Sleep 4s

# typed keys go INTO the session; the echo renders inside the pane
Type@150ms "echo INPANE_OK"
Sleep 1s
Enter
Sleep 4s

# Ctrl+Q releases: badge gone, read-only panel back, sidebar unchanged, session survives
Ctrl+Q
Sleep 4s
