# shellcheck shell=bash
# T0-daemon (PRs #934, #967, #978): the daemon's status store is the one truth.
# A hook-raised ASK lands as one fleet session whose tuple is identical across
# the CLI read, the daemon's own fleet/status reply and the TUI Fleet panel.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="after a hook-raised ASK, ainb fleet needs, the daemon's fleet/status reply and the TUI Fleet panel all report the fixture session as waiting · hook · tier 0"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  ask_session "Proof T0: which env ships first?" proof-t0 \
    || { check "the fixture session and its ASK exist" false; return; }

  check "ainb fleet needs lists the session within 15 s" \
    wait_for 15 bash -c "'$AINB_BIN' fleet needs --format json | jq -e 'any(.[]; .session_key == \"claude:proof-t0\")' >/dev/null"
  save_output fleet-needs "$AINB_BIN" fleet needs --format json
  local needs status
  needs="$(jq -c '.[] | select(.session_key == "claude:proof-t0") | {state, source, tier}' "$NODE_DIR/fleet-needs.txt")"
  status="$(fleet_status_rows | jq -c 'select(.session_key == "claude:proof-t0")')"
  printf '%s\n' "$status" | redact_host >"$NODE_DIR/fleet-status-rpc.json"
  CAPTURES+=("fleet-status-rpc.json")
  observe "fleet needs tuple: $needs"
  observe "fleet/status tuple: $(jq -c '{state, provenance, tier, has_open_request, wait_kind}' <<<"$status")"

  open_hangar_screen tui fleet 'ACTION QUEUE'
  wait_screen tui 'waiting · hook · tier 0' 20
  capture tui fleet-panel
  local line
  line="$(panel_status_line tui)"
  observe "Fleet panel detail line: ${line:-none}"

  check "the CLI read says waiting, hook, tier 0" \
    jq -e '.state == "waiting" and .source == "hook" and .tier == 0' <<<"$needs"
  check "the daemon's fleet/status row says waiting, provenance hook, with an open ask" \
    jq -e '.state == "waiting" and .provenance == "hook" and .has_open_request and .wait_kind == "ask"' <<<"$status"
  check "the Fleet panel renders the same state, provenance and tier" \
    grep -q '^waiting · hook · tier 0 · ' <<<"$line"
  check "ainb list still knows the session the hook named by cwd" \
    bash -c "'$AINB_BIN' list --format json | jq -e --arg c '$FIXTURE_CWD' 'any(.[]; .worktree_path == \$c)' >/dev/null"
}
