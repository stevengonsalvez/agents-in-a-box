# shellcheck shell=bash
# Phase 2 versioned sections (PR #956): every section moves only its own
# version slot and the draw path compares then sets, so a change that arrives
# from outside the keyboard repaints on its own.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with no key pressed, the live preview repaints newer agent output and a hook-raised ASK moves an open Fleet panel from 0 to 1 needs input (the daemon's Fleet generation bump reaches the section); a refreshed session list shows a CLI-created session"

scenario() {
  fixture_session || { check "the first fixture session starts" false; return; }
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  open_session_list tui
  check "the session list shows one workspace" wait_screen tui 'Workspaces \(1\)' 10
  capture tui sessions-one

  # The live preview section repaints as the agent prints, with no key.
  local tick_a tick_b
  tick_a="$(pane_text tui | grep -oE 'agent tick [0-9]+' | awk '{print $3}' | sort -n | tail -1)"
  sleep 5
  tick_b="$(pane_text tui | grep -oE 'agent tick [0-9]+' | awk '{print $3}' | sort -n | tail -1)"
  observe "newest tick on screen: ${tick_a:-?}, then ${tick_b:-?} five seconds later with no key"
  check "the preview repaints newer agent output with no key pressed" \
    test "${tick_b:-0}" -gt "${tick_a:-0}"

  local first_cwd="$FIXTURE_CWD" first_tmux="$FIXTURE_TMUX"
  fixture_session || { check "the second fixture session starts" false; return; }
  sleep 10
  observe "session rows 10 s after a CLI ainb run, no key: $(pane_text tui | grep -oE 'repo \([0-9]+\)' | head -1) (the list rescans disk on f or its own interval)"
  keys tui f
  check "after f the session list shows both sessions" wait_screen tui 'repo \(2\)' 15
  capture tui sessions-two

  keys tui q
  open_hangar_screen tui fleet 'ACTION QUEUE' || { check "the Fleet panel opens" false; return; }
  wait_screen tui '1 needs input [0-9]+' 10
  local before
  before="$(panel_lens_count tui 'needs input')"
  observe "Fleet panel needs input before the ASK: ${before:-?}"
  capture tui fleet-before
  FIXTURE_CWD="$first_cwd" FIXTURE_TMUX="$first_tmux" raise_ask "Proof Phase 2: repaint?" proof-phase2 >/dev/null
  observe "raised an ASK through the hook; no key sent to the TUI"
  check "the Fleet panel repaints to 1 needs input within 20 s, no key pressed" \
    wait_screen tui '1 needs input 1' 20
  capture tui fleet-after
  check "it started from 0" test "${before:-x}" = 0
}
