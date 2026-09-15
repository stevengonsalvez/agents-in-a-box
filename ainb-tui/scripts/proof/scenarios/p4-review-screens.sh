# shellcheck shell=bash
# P4 review screens (PR #1021): git_view and code_review state out of the host,
# review clicks as git_view.select_review_row, in-place attach through
# AttachTerminal(InPlace) and its report, and the command context gate.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="g opens the Review tab listing the worktree's changed files and a click on the second file selects it and shows its diff; ainb diff-review opens the same Code Review surface directly; A shows the agent's ticks in place and ctrl+q returns the strip; :recall opens Learnings from the session list"

scenario() {
  fixture_session || { check "the fixture session starts" false; return; }
  printf 'proof change\n' >"$FIXTURE_CWD/PROOF.md"
  printf 'second change\n' >"$FIXTURE_CWD/SECOND.md"
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  open_session_list tui

  keys tui g
  check "g opens the Review tab" wait_screen tui 'Code Review 2 files' 10
  check "the tab strip names Review, Commits and Markdown" wait_screen tui 'Review │ · Commits +· Markdown' 5
  capture tui review-open
  local row
  row="$(row_of tui '\[\?\] SECOND\.md')"
  click tui 10 "$row"
  check "a click on SECOND.md selects that row" wait_screen tui '▶ \[\?\] SECOND\.md' 5
  check "the diff pane follows the selection" wait_screen tui 'second change' 5
  capture tui review-clicked
  keys tui Escape
  wait_screen tui 'Workspaces \(' 10

  keys tui A
  check "A opens the in-place embed" wait_screen tui 'INTERACTIVE \| Ctrl\+Q release' 10
  check "the embed shows the agent's ticks" wait_screen tui 'agent tick [0-9]+' 5
  capture tui in-place
  keys tui C-q
  check "ctrl+q returns the preview strip" wait_screen tui 'preview │ ask │ err' 10

  keys tui :
  sleep 0.5
  type_text tui recall
  keys tui Enter
  check ":recall opens Learnings from the session list" wait_screen tui '🧠 Learnings' 10
  capture tui recall
  quit_tui tui

  ptmux new-session -d -s review -x "$PROOF_COLS" -y "$PROOF_ROWS" \
    "cd '$FIXTURE_CWD' && env -u TMUX -u TMUX_PANE '$AINB_BIN' diff-review ."
  check "ainb diff-review opens the Code Review surface" wait_screen review 'Code Review 2 files' 20
  capture review diff-review
  keys review q
}
