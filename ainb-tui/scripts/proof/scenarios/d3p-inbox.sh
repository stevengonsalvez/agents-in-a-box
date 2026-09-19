# shellcheck shell=bash
# D3-prime inbox: an issue created by a separate CLI process is aggregated by
# the daemon into the local human's inbox, the open desktop window carries it
# in the inbox section it subscribes to from its first batch (read from the
# renderer's own telemetry rather than its pixels), the TUI's inbox screen
# draws the same row from the same section, and a mark-all-read sweep sent by
# a third process through the daemon's RPC is what both surfaces then show.
#
# Opening the page and sweeping from it is the wdio inbox journey's leg
# (crates/ainb-desktop/e2e/specs/inbox.e2e.js), which also asserts the row ids
# by value: a plain window has no driver to click with, exactly as d2-board
# leaves answering to answer.e2e.js.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="an issue created by a separate CLI process lands in the local human's inbox as the daemon aggregates it, the open desktop window carries it in the inbox section it subscribes to and applies, the TUI's inbox screen draws the same row from the same section, and a mark-all-read sweep sent by a third process through the daemon's RPC is what both surfaces then show as read"

TITLE="proof d3p inbox $(date +%s)"

# inbox_json: the local human's inbox as the daemon's store lists it.
inbox_json() { "$AINB_BIN" hangar inbox list --format json --limit 200 2>/dev/null; }

# inbox_has <issue-id>: an entry names the issue.
inbox_has() {
  jq -e --arg id "$1" 'map(select(.subject_id == $id)) | length >= 1' <<<"$(inbox_json)" >/dev/null 2>&1
}

# inbox_read <issue-id>: the entry naming the issue is recorded read.
inbox_read() {
  jq -e --arg id "$1" 'map(select(.subject_id == $id)) | length >= 1 and all(.read_at != null)' <<<"$(inbox_json)" >/dev/null 2>&1
}

# applied_inbox_rows / applied_inbox_unread: the inbox counts on the last
# batch the renderer applied (`inbox_rows=` and `inbox_unread=` on the line).
applied_inbox_rows() {
  sed -n 's/.*renderer applied .*inbox_rows=\([0-9][0-9]*\).*/\1/p' "$DESKTOP_LOG" 2>/dev/null | tail -1
}
applied_inbox_unread() {
  sed -n 's/.*renderer applied .*inbox_unread=\(-\{0,1\}[0-9][0-9]*\).*/\1/p' "$DESKTOP_LOG" 2>/dev/null | tail -1
}
inbox_rows_at_least() {
  local seen
  seen="$(applied_inbox_rows)"
  [[ -n "$seen" ]] && ((seen >= $1))
}
inbox_unread_is() { test "$(applied_inbox_unread)" = "$1"; }

scenario() {
  desktop_ready || return

  fixture_session || { check "the CLI seeded a session before the window opened" false; return; }

  if ! start_desktop; then
    check "the desktop window applied a frame batch within 90 s" false
    [[ -s "$PROOF_WORLD/desktop.stderr" ]] && observe "window stderr: $(tail -3 "$PROOF_WORLD/desktop.stderr")"
    return
  fi
  observe "sections in the first batch the renderer applied: $(applied_sections)"
  check "the window subscribes to the inbox section from its first batch" \
    grep -q '"inbox"' <<<"$(applied_sections)"

  # Created by this process, not the window or the TUI. An unassigned issue
  # lands in its creator's own inbox: the local human's.
  local out id
  out="$("$AINB_BIN" hangar issue create --title "$TITLE" 2>&1)"
  printf '%s\n' "$out" >"$NODE_DIR/issue-create.txt"
  CAPTURES+=("issue-create.txt")
  id="$(sed -n 's/^created issue //p' <<<"$out" | head -1)"
  observe "issue created by a separate process: ${id:-none}"
  check "a separate CLI process created an issue" test -n "$id"
  [[ -n "$id" ]] || return

  check "the daemon aggregated the issue into the local human's inbox within 60 s" \
    wait_for 60 inbox_has "$id"
  inbox_json | redact_host >"$NODE_DIR/inbox-before.json"
  CAPTURES+=("inbox-before.json")

  check "the window's inbox section carries the row within 60 s, in its own telemetry" \
    wait_for 60 inbox_rows_at_least 1
  observe "inbox rows and unread on the last batch the renderer applied: $(applied_inbox_rows) rows, $(applied_inbox_unread) unread"
  check "the window counts it unread" test "$(applied_inbox_unread)" -ge 1

  # The TUI, over the same daemon: its inbox screen draws the same row.
  if start_tui tui; then
    keys tui b
    check "the TUI's inbox screen draws the issue's row within 30 s" \
      wait_screen tui "New issue: $TITLE" 30
    capture tui "tui-inbox.txt"
  else
    check "the TUI reached its home screen" false
  fi

  # A third process sweeps through the daemon's own RPC, with an op id of
  # its own minting, as any surface's sweep is sent.
  local op
  op="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  rpc_call 1 1 hangar/inbox_mark_read \
    "$(jq -nc --arg op "$op" '{workspace_id: "default", recipient: "member:me", op_id: $op}')" \
    | tail -1 >"$NODE_DIR/sweep.json"
  CAPTURES+=("sweep.json")
  observe "the third process's sweep: $(jq -c '.result // .error' "$NODE_DIR/sweep.json" 2>/dev/null)"
  check "the daemon recorded the entry read" wait_for 30 inbox_read "$id"
  check "the window's inbox section shows nothing unread within 60 s" wait_for 60 inbox_unread_is 0
  if ptmux has-session -t "=tui:" 2>/dev/null; then
    check "the TUI's inbox screen shows 0 unread within 30 s" wait_screen tui "0 unread" 30
    capture tui "tui-inbox-after.txt"
    keys tui Escape
    quit_tui tui
  fi

  if [[ -s "$DESKTOP_LOG" ]]; then
    tail -n 400 "$DESKTOP_LOG" | redact_host >"$NODE_DIR/desktop-log.txt"
    CAPTURES+=("desktop-log.txt")
  fi
}
