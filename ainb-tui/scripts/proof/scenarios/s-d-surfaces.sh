# shellcheck shell=bash
# S-D concurrency (PR #964): the surface-combination smoke on this binary, and
# a live answer race where two surfaces answer the same card at once.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="scripts/surface-combo-smoke.sh passes all four combinations ({tui} {web} {tui,web} {tui,tui}) on this binary, and two concurrent POST /api/answer calls for one ASK yield exactly one delivered and one already_answered"

scenario() {
  local status=0
  # TMPDIR inside the world: the smoke makes its own homes with mktemp, and
  # there they stay visible to teardown and to run.sh's leftover probe.
  TMPDIR="$PROOF_WORLD" REQUIRE=1 bash "$AINB_TUI_DIR/scripts/surface-combo-smoke.sh" "$AINB_BIN" \
    >"$PROOF_WORLD/smoke.out" 2>&1 || status=$?
  redact_host <"$PROOF_WORLD/smoke.out" >"$NODE_DIR/surface-combo-smoke.txt"
  CAPTURES+=("surface-combo-smoke.txt")
  observe "surface-combo-smoke exit $status: $(grep -E 'all four|FAIL' "$NODE_DIR/surface-combo-smoke.txt" | head -3 | tr '\n' ' ')"
  check "the surface-combination smoke exits 0" test "$status" -eq 0
  local combo
  for combo in '{tui}' '{web}' '{tui, web}' '{tui, tui}'; do
    check "the smoke ran $combo" grep -qF "=== $combo ===" "$NODE_DIR/surface-combo-smoke.txt"
  done

  # The answer race, live.
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  start_web || { check "ainb web answers" false; return; }
  fixture_session || { check "the fixture session starts" false; return; }
  local waited
  waited="$(web_sync_sessions 180)" || { check "the web snapshot lists the fixture within 180 s" false; return; }
  observe "web snapshot caught up after ${waited}s"
  raise_ask "Proof S-D: race?" proof-sd >/dev/null
  # Up to 180 s, as in s-c-answered: the web's poller can lag behind the
  # daemon for minutes (#1055), and a shorter wait failed run 11 on that alone.
  local id
  if ! web_card_id 'Proof S-D' race-card 180; then
    check "the web lists the card within 180 s" false
    return
  fi
  id="$WEB_CARD_ID"
  web_answer "$id" 1 >"$PROOF_WORLD/race-1.json" &
  web_answer "$id" 2 >"$PROOF_WORLD/race-2.json" &
  wait
  cat "$PROOF_WORLD/race-1.json" "$PROOF_WORLD/race-2.json" | redact_host >"$NODE_DIR/answer-race.txt"
  CAPTURES+=("answer-race.txt")
  observe "race replies: $(tr '\n' ' ' <"$NODE_DIR/answer-race.txt")"
  local delivered already
  delivered="$(cat "$PROOF_WORLD"/race-*.json | jq -s '[.[] | select(.outcome == "delivered")] | length')"
  already="$(cat "$PROOF_WORLD"/race-*.json | jq -s '[.[] | select(.outcome == "already_answered")] | length')"
  check "exactly one concurrent answer is delivered" test "$delivered" -eq 1
  check "the other is told already_answered" test "$already" -eq 1
}
