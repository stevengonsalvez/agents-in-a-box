# shellcheck shell=bash
# #962 (PR #1014): the TUI Fleet panel renders its state, lenses and counts
# from the daemon's fleet/status, never from its own reading of the snapshot.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with two sessions holding a hook-raised ASK and one with no hook, the Fleet panel's needs-input and all counts equal the waiting rows and total rows the daemon's fleet/status reply carries, and every card's state matches its row"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  ask_session "Proof 962: first question?" proof-962-a || { check "first ASK session" false; return; }
  ask_session "Proof 962: second question?" proof-962-b || { check "second ASK session" false; return; }
  fixture_session || { check "the session with no hook starts" false; return; }
  observe "fixtures: two ASK sessions (proof-962-a, proof-962-b) and $FIXTURE_TMUX with no hook"

  check "the daemon holds both ASK sessions within 15 s" \
    wait_for 15 bash -c "'$AINB_BIN' fleet needs --format json | jq -e '[.[] | select(.state == \"waiting\")] | length >= 2' >/dev/null"

  open_hangar_screen tui fleet 'ACTION QUEUE'
  wait_screen tui '1 needs input 2' 20
  keys tui 5
  sleep 2
  capture tui fleet-panel-all

  local rows waiting total panel_needs panel_all panel_idle panel_running
  rows="$(fleet_status_rows)"
  printf '%s\n' "$rows" | redact_host >"$NODE_DIR/fleet-status-rpc.jsonl"
  CAPTURES+=("fleet-status-rpc.jsonl")
  save_output fleet-needs "$AINB_BIN" fleet needs --format json
  waiting="$(jq -s '[.[] | select(.state == "waiting")] | length' <<<"$rows")"
  total="$(jq -s 'length' <<<"$rows")"
  panel_needs="$(panel_lens_count tui 'needs input')"
  panel_all="$(panel_lens_count tui all)"
  panel_idle="$(panel_lens_count tui idle)"
  panel_running="$(panel_lens_count tui running)"
  observe "fleet/status: $total rows, $waiting waiting, $(jq -s '[.[] | select(.state == "idle")] | length' <<<"$rows") idle, $(jq -s '[.[] | select(.state == "working")] | length' <<<"$rows") working"
  observe "panel lenses: needs input ${panel_needs:-?}, idle ${panel_idle:-?}, running ${panel_running:-?}, all ${panel_all:-?}"

  check "panel needs-input count equals the daemon's waiting rows" test "${panel_needs:-x}" = "$waiting"
  check "panel all count equals the daemon's row count" test "${panel_all:-x}" = "$total"
  check "panel idle count equals the daemon's idle rows" \
    test "${panel_idle:-x}" = "$(jq -s '[.[] | select(.state == "idle")] | length' <<<"$rows")"
  check "panel running count equals the daemon's working rows" \
    test "${panel_running:-x}" = "$(jq -s '[.[] | select(.state == "working")] | length' <<<"$rows")"
  check "two waiting cards carry the daemon's word: waiting · ask" \
    test "$(pane_text tui | grep -c '─ waiting · ask ─')" -eq 2
}
