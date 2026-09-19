# shellcheck shell=bash
# PR #1173 (D2a seams): `session_list.select_tab` is the pointer command a
# click on the session tab strip names. The strip is the right pane's border
# title, `preview │ ask │ err │ thread │ pal │ log`, and the active tab is the
# one drawn bold and underlined. This node clicks the `log` label, then the
# `pal` label, in the TUI and reads the pane's own text for which tab is active.
#
# `tab` is the control: it cycles the same strip through the keyboard, so a
# read that cannot see the active tab change fails there first, not on the
# click.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a click on the log label of the session tab strip makes log the active tab, and a click on the pal label then makes pal active (session_list.select_tab), as tab already moves the strip off preview"

# strip_row <session>: the 1-based screen row the tab strip is drawn on.
strip_row() { row_of "$1" 'preview │ ask │ err │'; }

# active_tab <session> <row>: the strip label drawn underlined on <row>, read
# from the ANSI capture. Only the active tab carries the underline attribute.
active_tab() {
  pane_text "$1" -e | sed -n "${2}p" | python3 -c '
import re, sys
line = sys.stdin.read()
underline = False
for code, text in re.findall(r"\x1b\[([0-9;]*)m|([^\x1b]+)", line):
    if text:
        if underline:
            word = text.strip()
            if word:
                print(word)
                break
        continue
    params = code.split(";") if code else ["0"]
    i = 0
    while i < len(params):
        p = params[i]
        if p in ("38", "48") and i + 1 < len(params):
            i += 3 if params[i + 1] == "5" else 5
            continue
        if p in ("", "0", "24"):
            underline = False
        elif p == "4":
            underline = True
        i += 1
'
}

# active_is <session> <row> <label>
active_is() { [[ "$(active_tab "$1" "$2")" == "$3" ]]; }

# active_is_not <session> <row> <label>: a tab is active, and it is not <label>.
active_is_not() { local now; now="$(active_tab "$1" "$2")"; [[ -n "$now" && "$now" != "$3" ]]; }

# label_col <session> <row> <label>: the 1-based column of <label> on <row>.
label_col() {
  pane_text "$1" | sed -n "${2}p" \
    | python3 -c 'import sys; line = sys.stdin.read(); i = line.find(" " + sys.argv[1] + " "); print(i + 2 if i >= 0 else "")' "$3"
}

scenario() {
  fixture_session || { check "the fixture session starts" false; return; }
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  open_session_list tui
  # The load toast is drawn over the strip's row; the click and the read both
  # wait until it has gone.
  wait_gone tui 'Workspaces loaded' 15
  local row col
  row="$(strip_row tui)"
  if [[ -z "$row" ]]; then
    capture tui no-strip
    check "the session list draws the tab strip" false
    return
  fi
  capture tui strip-before
  observe "tab strip on row $row; active tab before any input: '$(active_tab tui "$row")'"
  check "preview is the active tab when the session list opens" active_is tui "$row" preview

  # The click, from a clean preview.
  col="$(label_col tui "$row" log)"
  if [[ -z "$col" ]]; then
    check "the strip names a log tab" false
    return
  fi
  observe "clicking 'log' at column $col, row $row"
  click tui "$col" "$row"
  check "a click on the log label makes log the active tab" wait_for 5 active_is tui "$row" log
  observe "active tab after the click: '$(active_tab tui "$row")'"
  capture tui strip-after-click

  # A second label, so a select_tab that ignored the column and always picked
  # the last tab would still fail here. `pal`, not `ask`: with no pending ASK
  # the strip dims `ask`, and `resolve` sends a click on a dimmed tab to
  # preview, so `ask` could not become active even with a correct click.
  col="$(label_col tui "$row" pal)"
  if [[ -z "$col" ]]; then
    check "the strip names a pal tab" false
    return
  fi
  observe "clicking 'pal' at column $col, row $row"
  click tui "$col" "$row"
  check "a click on the pal label makes pal the active tab" wait_for 5 active_is tui "$row" pal
  observe "active tab after the second click: '$(active_tab tui "$row")'"
  capture tui strip-after-second-click

  # Control: the keyboard moves the same strip, and the same read sees it, so
  # a failed click above is the click, not the read.
  local before_tab
  before_tab="$(active_tab tui "$row")"
  keys tui Tab
  check "tab moves the active tab off '$before_tab' (the read sees a change)" \
    wait_for 5 active_is_not tui "$row" "$before_tab"
  observe "active tab after tab: '$(active_tab tui "$row")'"
  capture tui strip-after-tab
}
