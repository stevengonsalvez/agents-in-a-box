# shellcheck shell=bash
# #983 (PR #1026): the redaction layer. The frame shape is locked to the
# committed fixture, and credential-shaped text never reaches a frame.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="ainb doctor --wire-shape reports no drift from the committed fixture, a token-shaped string in a session label never appears in hangar connections list, the wire-shape frame output, or the web snapshot frame, an ASK whose question carries the token reaches the web snapshot as a card with no cwd and no token, and no absolute path appears anywhere in the web snapshot"

# Shaped like a GitHub classic token so the redactor's table matches it. Built
# from two halves so no token-shaped literal sits in the source, and replaced
# by <canary> in every published capture once the checks have read them.
PROOF_TOKEN="ghp""_ProofCanary0123456789abcdefghijklmnopq"

# How long to wait for the web's cost panel to carry the recorded turn. The
# web's cost task abandons a fetch at 30 s and doubles its bound (60, 120, up
# to 240) until one lands, so a slow box can take about 210 s to land the
# first cost. This sits above that, and the node records the wait it actually
# took rather than assuming a quick one. Overridable, so the timeout branch
# can be exercised deliberately.
COST_PANEL_WAIT="${COST_PANEL_WAIT:-360}"

scenario() {
  save_output wire-shape "$AINB_BIN" doctor --wire-shape
  local status=0
  "$AINB_BIN" doctor --wire-shape >/dev/null 2>&1 || status=$?
  observe "doctor --wire-shape exit status: $status"
  check "doctor --wire-shape exits 0 (no drift)" test "$status" -eq 0
  check "doctor --wire-shape lists no added or removed path" \
    bash -c "! grep -qE '^[[:space:]]*[+-] ' '$NODE_DIR/wire-shape.txt'"
  save_output wire-shape-json "$AINB_BIN" doctor --wire-shape --format json

  fixture_session || { check "the fixture session starts" false; return; }
  "$AINB_BIN" label --set "deploy $PROOF_TOKEN" "$FIXTURE_ID" >"$PROOF_WORLD/label.out" 2>&1
  observe "label exit: $(tail -1 "$PROOF_WORLD/label.out")"
  save_output list-json "$AINB_BIN" list --format json

  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  wait_for 10 rows_is tui "" ge 1
  save_output connections-list "$AINB_BIN" hangar connections list
  save_output connections-json "$AINB_BIN" hangar connections list --format json
  save_output fleet-needs "$AINB_BIN" fleet needs --format json
  check "the token never appears in hangar connections list" \
    bash -c "! grep -qF '$PROOF_TOKEN' '$NODE_DIR/connections-list.txt' '$NODE_DIR/connections-json.txt'"
  check "the token never appears in the wire-shape frame output" \
    bash -c "! grep -qF '$PROOF_TOKEN' '$NODE_DIR/wire-shape.txt' '$NODE_DIR/wire-shape-json.txt'"

  # The live frame a remote surface receives today is the web snapshot.
  # An ASK whose question carries the canary, raised through the real hook
  # path, so needs[] has a card built from a request that holds the token (#1081).
  raise_ask "Proof 983: deploy with $PROOF_TOKEN?" proof-983-ask >/dev/null
  # One recorded Claude turn in the fixture's directory, so `fleet cost` has a
  # session row with an absolute cwd and the web cost panel is populated: the
  # no-absolute-path check below then covers cost too (#1113).
  # Claude names a project directory after its cwd with `/` and `.` turned
  # into `-`; derive it from the fixture's cwd rather than hardcode one.
  local projects="$HOME/.claude/projects/$(sed 's#[/.]#-#g' <<<"$FIXTURE_CWD")"
  mkdir -p "$projects"
  observe "recorded Claude turn under .claude/projects/${projects##*/}"
  jq -nc --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg cwd "$FIXTURE_CWD" '{
    type: "assistant", timestamp: $ts, sessionId: "proof-983-usage", cwd: $cwd,
    gitBranch: "main", message: {model: "claude-sonnet-4-5",
      content: [{type: "text", text: "proof"}],
      usage: {input_tokens: 100, output_tokens: 20}}
  }' >"$projects/proof-983-usage.jsonl"
  save_output fleet-cost "$AINB_BIN" fleet cost --format json
  local cost_sessions
  cost_sessions="$(jq '[.sessions[]? | select(.cwd)] | length' "$NODE_DIR/fleet-cost.txt" 2>/dev/null || echo 0)"
  observe "fleet cost sessions with a cwd: $cost_sessions"
  start_web || { check "ainb web answers" false; return; }
  local waited
  waited="$(web_sync_sessions 180)" || true
  observe "web snapshot caught up after ${waited}s"
  # The card reaches the snapshot on a later poll than the session list does:
  # the daemon ingests the hook line, then the web's next core poll reads it.
  local card_wait=$SECONDS
  wait_for 180 bash -c "curl -sS '$WEB_URL/api/snapshot' | jq -e '.needs[]? | select((.payload.question // \"\") | test(\"Proof 983\"))' >/dev/null"
  observe "the ASK card reached the web snapshot after $((SECONDS - card_wait))s"
  # The cost task fetches at startup; wait for the panel before capturing. Only
  # when `fleet cost` saw the recorded turn: a world without burndown usage has
  # no panel to wait for.
  #
  # The wait has to outlast the web's OWN backoff, not just a quick fetch. The
  # cost task abandons a fetch at 30 s and doubles the bound (60, 120, up to
  # 240) until one lands, and #1055 measured `ainb fleet cost` at 120 s under
  # contention, so a busy box can spend about 30 + 60 + 120 s before the first
  # cost lands. A 60 s wait lost that race during run 30 on a loaded box, and
  # failed a node whose product path was working: the same node passed on the
  # same binary once the box was quieter.
  local cost_wait=$SECONDS
  if (( cost_sessions > 0 )); then
    if wait_for "$COST_PANEL_WAIT" bash -c "curl -sS '$WEB_URL/api/snapshot' | jq -e '(.cost.totals.bucket.call_count // 0) > 0' >/dev/null"; then
      observe "the web cost panel landed after $((SECONDS - cost_wait))s"
    else
      observe "the web cost panel did not land within ${COST_PANEL_WAIT}s (web cost backoff: 30 s abandoned, doubling to 240 s; #1055)"
    fi
  fi
  curl -sS "$WEB_URL/api/snapshot" | redact_host >"$NODE_DIR/web-snapshot.json"
  CAPTURES+=("web-snapshot.json")
  observe "operator's own ainb list carries the label: $(grep -c "$PROOF_TOKEN" "$NODE_DIR/list-json.txt") line(s)"
  check "the token never appears in the web snapshot frame" \
    bash -c "! grep -qF '$PROOF_TOKEN' '$NODE_DIR/web-snapshot.json'"
  if (( cost_sessions > 0 )); then
    check "the web cost panel is populated from the recorded turn" \
      jq -e '(.cost.totals.bucket.call_count // 0) > 0' "$NODE_DIR/web-snapshot.json"
    check "the web cost panel carries no per-session rows" jq -e '.cost | has("sessions") | not' "$NODE_DIR/web-snapshot.json"
    check "no string in the web cost panel is an absolute path" \
      jq -e '[.cost | .. | strings | select(startswith("/"))] | length == 0' "$NODE_DIR/web-snapshot.json"
    check "the fixture session's directory never appears in the web cost panel" \
      bash -c "! jq -c '.cost' '$NODE_DIR/web-snapshot.json' | grep -qF '$FIXTURE_CWD'"
  else
    observe "fleet cost saw no recorded turn in this world; the cost panel checks are skipped"
  fi
  jq '.needs' "$NODE_DIR/web-snapshot.json" >"$NODE_DIR/web-needs.json"
  observe "web snapshot needs cards: $(jq 'length' "$NODE_DIR/web-needs.json")"
  check "the canary-bearing ASK reaches the web snapshot as a card" \
    jq -e 'any(.[]; (.payload.question // "") | test("Proof 983"))' "$NODE_DIR/web-needs.json"
  check "no web needs card carries a cwd" jq -e 'all(.[]; has("cwd") | not)' "$NODE_DIR/web-needs.json"
  check "the fixture session's directory never appears in the web needs cards" \
    bash -c "! grep -qF '$FIXTURE_CWD' '$NODE_DIR/web-needs.json'"
  # No absolute path anywhere in the response, and no path into this world
  # even inside a longer string (#1097).
  local absolute
  absolute="$(jq '[.. | strings | select(startswith("/"))] | length' "$NODE_DIR/web-snapshot.json")"
  observe "strings in the web snapshot that are absolute paths: $absolute"
  check "no string in the web snapshot is an absolute path" test "$absolute" -eq 0
  check "the proof world's directory never appears in the web snapshot" \
    bash -c "! grep -qF '$PROOF_WORLD' '$NODE_DIR/web-snapshot.json'"

  # Checks are done; keep the canary out of what gets published.
  local file
  while IFS= read -r file; do
    sed -i "s/$PROOF_TOKEN/<canary>/g" "$file"
  done < <(grep -rlF "$PROOF_TOKEN" "$NODE_DIR" 2>/dev/null)
  OBSERVED=("${OBSERVED[@]//$PROOF_TOKEN/<canary>}")
  observe "the canary is written as <canary> in the published captures"
}
