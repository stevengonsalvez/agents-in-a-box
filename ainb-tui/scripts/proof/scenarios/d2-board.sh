# shellcheck shell=bash
# D2 desktop board: a question an agent's hook raises reaches the open
# window's board as a waiting card, read from the renderer's own telemetry
# rather than its pixels; a second surface answers it, the daemon records
# which surface won, a third reads that winner back, and the board draws the
# waiting column again without it.
#
# The desktop answering from its own banner is the wdio answer journey's leg
# (crates/ainb-desktop/e2e/specs/answer.e2e.js): a plain window has no driver
# to click with, so this node proves the board and the daemon's record.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a question raised by a separate hook process reaches the open desktop window's board as a waiting card, from framed sections, a second surface answers it and the daemon records that surface, a third surface reads the same winner, and the window's waiting column drops the card"

QUESTION="Ship the d2 board to which environment?"
PROVIDER_ID="proof-d2-board"

# attention_answered <id>: the daemon no longer lists the row in its open
# feed, read through its own `attention/list` (#1193). A row leaves the feed
# the instant its answer is recorded, so this is the daemon's word that the
# row is answered; the winner is read from the refusal a later answer gets.
# False when the call fails, so a daemon that is down reads as not answered.
attention_answered() {
  local rows
  rows="$(rpc_call 1 1 attention/list '{"fleet": true}' | tail -1)" || return 1
  jq -e --arg id "$1" '(.result.attention // empty) | map(select(.id == $id)) | length == 0' \
    <<<"$rows" >/dev/null 2>&1
}

waiting_at_least() {
  local seen
  seen="$(applied_cards waiting)"
  [[ -n "$seen" ]] && ((seen >= $1))
}

waiting_is() { test "$(applied_cards waiting)" = "$1"; }

scenario() {
  desktop_ready || return

  fixture_session || { check "the CLI seeded a session before the window opened" false; return; }

  if ! start_desktop; then
    check "the desktop window applied a frame batch within 90 s" false
    [[ -s "$PROOF_WORLD/desktop.stderr" ]] && observe "window stderr: $(tail -3 "$PROOF_WORLD/desktop.stderr")"
    return
  fi
  observe "sections in the first batch the renderer applied: $(applied_sections)"
  check "the renderer applied the agent_status section" \
    grep -q '"agent_status"' <<<"$(applied_sections)"
  observe "board columns before the question: $(applied_board)"
  # Empty first, so the card the next check waits for is this question's and
  # not one the column already held.
  check "the waiting column is empty before the question" wait_for 30 waiting_is 0

  # Raised through the agent's own hook command, by a process that is not the
  # window, against the fixture session's worktree.
  raise_ask "$QUESTION" "$PROVIDER_ID" >"$NODE_DIR/raise.txt" 2>&1
  CAPTURES+=("raise.txt")
  check "the board drew a waiting card within 60 s" wait_for 60 waiting_at_least 1
  observe "board columns with the question open: $(applied_board)"

  # A second surface answers: the web dashboard, on the same daemon.
  start_web || { check "the web dashboard came up" false; return; }
  web_card_id "$QUESTION" web 60 || { check "the web dashboard listed the question" false; return; }
  local id="$WEB_CARD_ID"
  observe "attention id the hook raised: $id"
  web_answer "$id" beta | redact_host >"$NODE_DIR/web-answer.json"
  CAPTURES+=("web-answer.json")
  observe "web answer result: $(jq -c . "$NODE_DIR/web-answer.json" 2>/dev/null)"
  # The harness's fixture agent reads nothing, so the pane's own echo of the
  # typed line is the proof it arrived.
  check "the answer reached the agent's pane" wait_for 30 fixture_says "beta"

  # The daemon's record, read through its own RPC by a process that is not
  # the window: the row has left the open feed, and a third surface's answer
  # to the same question loses to the web's, the refusal naming the winner.
  check "the daemon dropped the row from its open feed once answered" \
    wait_for 30 attention_answered "$id"
  rpc_call 1 1 attention/answer \
    "$(jq -nc --arg id "$id" '{attention_id: $id, answer: "alpha", answered_by: "tui", is_answer: true}')" \
    | tail -1 >"$NODE_DIR/third-surface.json"
  CAPTURES+=("third-surface.json")
  observe "a third surface's answer to $id: $(jq -c '.result' "$NODE_DIR/third-surface.json" 2>/dev/null)"
  # `<surface>@<host>`: the surface is the one that answered.
  check "the daemon recorded the web as the winner, read by the third surface" \
    grep -qE '^already_answered\|web@' <<<"$(jq -r '.result.outcome + "|" + .result.by' "$NODE_DIR/third-surface.json" 2>/dev/null)"

  # The window draws the waiting column again without the answered card.
  check "the board's waiting column dropped the card within 60 s" wait_for 60 waiting_is 0
  observe "board columns after the answer: $(applied_board)"

  if [[ -s "$DESKTOP_LOG" ]]; then
    tail -n 400 "$DESKTOP_LOG" | redact_host >"$NODE_DIR/desktop-log.txt"
    CAPTURES+=("desktop-log.txt")
  fi
}
