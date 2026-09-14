# shellcheck shell=bash
# P2 sessions and effects (PR #1000): every side effect is an Effect the host
# runs after the step commits, with a notice on failure.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a (AttachTerminal Session) suspends the TUI into a writable tmux client on the fixture and ctrl+b d resumes the session list; A (AttachTerminal InPlace) shows the agent's ticks in the embed and ctrl+q (Detach) returns the strip; o (OpenEditor) with no editor configured says how to set one"

writable_clients() {
  ftmux list-clients -t "=$FIXTURE_TMUX" -F '#{client_flags}' 2>/dev/null | grep -vc 'read-only'
}

scenario() {
  fixture_session || { check "the fixture session starts" false; return; }
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  open_session_list tui
  local pid
  pid="$(tui_pid tui)"

  keys tui a
  check "a opens a writable tmux client on the fixture session" wait_for 10 bash -c "[ \$(tmux list-clients -t '=$FIXTURE_TMUX' -F '#{client_flags}' | grep -vc read-only) -ge 1 ]"
  check "the pane now shows the agent full screen" wait_gone tui 'Workspaces \(' 5
  capture tui attached-full-screen
  keys tui C-b d
  check "ctrl+b d resumes the TUI's session list" wait_screen tui 'Workspaces \(' 15
  check "the same TUI process resumed (pid $pid)" kill -0 "$pid"
  observe "writable clients after detach: $(writable_clients)"
  capture tui resumed

  keys tui A
  check "A opens the in-place embed" wait_screen tui 'INTERACTIVE \| Ctrl\+Q release' 10
  check "the embed shows the fixture agent's ticks" wait_screen tui 'agent tick [0-9]+' 5
  capture tui in-place
  keys tui C-q
  check "ctrl+q detaches back to the preview strip" wait_screen tui 'preview │ ask │ err' 10
  capture tui detached

  keys tui o
  check "o with no editor configured says how to set one" wait_screen tui '[Ee]ditor' 10
  capture tui editor-notice
  observe "editor notice: $(pane_text tui | grep -oE '[^│╭╰]*[Ee]ditor[^│╮╯]*' | head -1 | sed 's/  */ /g')"
}
