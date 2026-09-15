# shellcheck shell=bash
# #983 (PR #1026): the redaction layer. The frame shape is locked to the
# committed fixture, and credential-shaped text never reaches a frame.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="ainb doctor --wire-shape reports no drift from the committed fixture, and a token-shaped string in a session label never appears in hangar connections list, the wire-shape frame output, or the web snapshot frame"

# Shaped like a GitHub classic token so the redactor's table matches it. Built
# from two halves so no token-shaped literal sits in the source, and replaced
# by <canary> in every published capture once the checks have read them.
PROOF_TOKEN="ghp""_ProofCanary0123456789abcdefghijklmnopq"

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
  start_web || { check "ainb web answers" false; return; }
  local waited
  waited="$(web_sync_sessions 180)" || true
  observe "web snapshot caught up after ${waited}s"
  curl -sS "$WEB_URL/api/snapshot" | redact_host >"$NODE_DIR/web-snapshot.json"
  CAPTURES+=("web-snapshot.json")
  observe "operator's own ainb list carries the label: $(grep -c "$PROOF_TOKEN" "$NODE_DIR/list-json.txt") line(s)"
  check "the token never appears in the web snapshot frame" \
    bash -c "! grep -qF '$PROOF_TOKEN' '$NODE_DIR/web-snapshot.json'"

  # Checks are done; keep the canary out of what gets published.
  local file
  while IFS= read -r file; do
    sed -i "s/$PROOF_TOKEN/<canary>/g" "$file"
  done < <(grep -rlF "$PROOF_TOKEN" "$NODE_DIR" 2>/dev/null)
  OBSERVED=("${OBSERVED[@]//$PROOF_TOKEN/<canary>}")
  observe "the canary is written as <canary> in the published captures"
}
