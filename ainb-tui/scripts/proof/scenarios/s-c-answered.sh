# shellcheck shell=bash
# S-C card retirement (PR #936): an answered card retires on every surface and
# names who answered (G6 step 7).

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="an ASK answered from the web retires the TUI control-center card at once with 'answered by web@<host>'; an ASK answered in the TUI shows 'answered by tui@<host>' and the web's later POST /api/answer returns already_answered by tui"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  start_web || { check "ainb web answers /api/snapshot" false; return; }
  observe "web on ${WEB_URL##*:} (port)"

  # The web's snapshot poller starts behind two concurrent `ainb fleet cost`
  # runs that take 120 s each (#1055); the answer path is only testable once
  # the web has caught up with the session list.
  fixture_session || { check "the first fixture session starts" false; return; }
  local waited
  if waited="$(web_sync_sessions 180)"; then
    observe "web snapshot listed the fixture session after ${waited}s (#1055)"
  else
    check "the web snapshot lists the fixture session within 180 s" false
    return
  fi

  # 1. Answered from the web.
  raise_ask "Proof S-C: answered from the web?" proof-sc-web >/dev/null
  open_hangar_screen tui control 'Control  ·' || { check "the control center opens" false; return; }
  check "the TUI control center shows the web-bound card" wait_screen tui '1 need you' 20
  capture tui control-before-web-answer
  local id reply
  web_card_id 'answered from the web' web-card
  id="$WEB_CARD_ID"
  curl -sS "$WEB_URL/api/snapshot" | redact_host >"$NODE_DIR/web-snapshot-before-answer.json"
  CAPTURES+=("web-snapshot-before-answer.json")
  reply="$(web_answer "$id" 2)"
  observe "POST /api/answer (web): $reply"
  check "the web answer is delivered" jq -e '.outcome == "delivered"' <<<"$reply"
  check "the TUI card retires within 2 s" wait_screen tui '0 need you' 2
  check "the TUI title names the web as the answerer" wait_screen tui 'answered by web@' 2
  capture tui control-after-web-answer

  # 2. Answered in the TUI, then the web tries.
  sleep 4
  ask_session "Proof S-C: answered in the TUI?" proof-sc-tui || { check "second ASK session" false; return; }
  check "the TUI shows the second card" wait_screen tui '1 need you' 20
  web_card_id 'answered in the TUI' tui-card
  id="$WEB_CARD_ID"
  capture tui control-before-tui-answer
  keys tui Enter
  check "the TUI title names the TUI as the answerer" wait_screen tui 'answered by tui@' 5
  capture tui control-after-tui-answer
  reply="$(web_answer "$id" 1)"
  observe "POST /api/answer (web, after the TUI answered): $reply"
  check "the web is told already_answered by tui@" jq -e '.outcome == "already_answered" and (.by | startswith("tui@"))' <<<"$reply"
  printf '%s\n' "$reply" | redact_host >"$NODE_DIR/web-answer-after-tui.json"
  CAPTURES+=("web-answer-after-tui.json")
}
