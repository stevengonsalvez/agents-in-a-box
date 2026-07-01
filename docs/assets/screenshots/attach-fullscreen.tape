# vhs tape: 'a' full-screen attach — TUI suspends into the real tmux client,
# a command runs inside the session, Ctrl+B d detaches back to ainb.
#
# Seed (once, before recording — isolated HOME + isolated tmux server so no
# real session names leak into the frames):
#
#   DEMO=/tmp/ainb-attach-demo
#   mkdir -p "$DEMO/home/.agents-in-a-box/config" "$DEMO/tmux" && chmod 700 "$DEMO/tmux"
#   printf 'completed = true\ncompleted_at = "2026-06-11T00:00:00+00:00"\nversion = "%s"\nskipped_dependencies = []\ngit_directories = []\n' \
#     "$(./target/debug/ainb --version | awk '{print $2}')" > "$DEMO/home/.agents-in-a-box/config/onboarding.toml"
#   echo '{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}' \
#     > "$DEMO/home/.agents-in-a-box/install.json"
#   env -u TMUX HOME="$DEMO/home" TMUX_TMPDIR="$DEMO/tmux" tmux new-session -d -s demo-agent -x 220 -y 50 \
#     'bash -c "clear; echo BANNER + a few fake agent log lines; exec env PS1=\"demo-agent \\$ \" bash --norc --noprofile"'
#
# Run from ainb-tui/:  vhs ../docs/assets/screenshots/attach-fullscreen.tape
# Then optimise:       gifsicle -O3 --lossy=60 --colors 200 --resize-width 1100 -b <gif>
# Cleanup:             env -u TMUX TMUX_TMPDIR="$DEMO/tmux" tmux kill-session -t demo-agent; rm -rf "$DEMO"
Output "../docs/assets/screenshots/attach-fullscreen.gif"

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

# 'a' full-screen attach: TUI suspends, real tmux fills the terminal
Type "a"
Sleep 4s

# type a command inside the session (live input proof)
Type@150ms "echo FULLSCREEN_OK"
Sleep 1s
Enter
Sleep 3.5s

# Ctrl+B d detaches; ainb chrome returns, session still listed
Ctrl+B
Sleep 500ms
Type "d"
Sleep 5s
