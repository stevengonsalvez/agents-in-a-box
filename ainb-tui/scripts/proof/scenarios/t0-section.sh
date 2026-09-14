# shellcheck shell=bash
# T0-section (PR #1019): agent status as one joined daemon read, rendered with
# its evidence clock, and a rendered (not logged) failure story.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a fixture session with a hook-raised ASK renders waiting · hook · tier 0 · Ns (a real age in seconds) in the Fleet panel, and once the daemon is stopped the panel says so on screen: offline, states unverifiable, host unreachable"

# Filed from this harness's first runs, each with its capture and source cite:
# the panel clock never ticks so the age renders `?` (#1054), and a stopped
# daemon shows only the offline banner, not the unreachable story (#1058).
T0_SECTION_AGE_ISSUE=1054
T0_SECTION_STORY_ISSUE=1058

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  ask_session "Proof T0-section: ready?" proof-t0s || { check "the ASK session exists" false; return; }
  wait_for 15 bash -c "'$AINB_BIN' fleet needs --format json | jq -e 'length > 0' >/dev/null"
  open_hangar_screen tui fleet 'ACTION QUEUE' || { check "the Fleet panel opens" false; return; }
  wait_screen tui 'waiting · hook · tier 0' 20
  sleep 5
  capture tui fleet-live

  local line
  line="$(panel_status_line tui)"
  observe "Fleet panel detail line 5 s after the read: ${line:-none}"
  save_output fleet-needs "$AINB_BIN" fleet needs --format json
  check "the panel renders the tuple waiting · hook · tier 0" grep -q '^waiting · hook · tier 0 · ' <<<"$line"
  local age_ok=1
  check "the tuple's age is a real duration (Ns, Nm or Nh), not ?" grep -qE ' · [0-9]+[smh]$' <<<"$line"
  grep -qE ' · [0-9]+[smh]$' <<<"$line" || age_ok=0

  "$AINB_BIN" hangar daemon stop >"$NODE_DIR/daemon-stop.txt" 2>&1
  CAPTURES+=("daemon-stop.txt")
  wait_for 15 bash -c "! '$AINB_BIN' hangar daemon status 2>/dev/null | grep -q 'daemon: running'"
  observe "daemon after stop: $("$AINB_BIN" hangar daemon status 2>&1 | head -1)"
  wait_screen tui 'offline' 20
  sleep 10
  capture tui fleet-daemon-stopped

  check "the panel header says the daemon is offline" \
    bash -c "grep -q 'Fleet daemon offline' '$NODE_DIR/fleet-daemon-stopped.txt'"
  check "the lens body says states unverifiable" \
    bash -c "grep -q 'states unverifiable' '$NODE_DIR/fleet-daemon-stopped.txt'"
  check "the lens body names the unreachable host and why" \
    bash -c "grep -qE 'unreachable since|daemon not reachable' '$NODE_DIR/fleet-daemon-stopped.txt'"
  observe "the card still reads: $(panel_status_line tui)"

  if ((!age_ok)); then
    known_issue "$T0_SECTION_AGE_ISSUE"
    observe "known failures: age renders ? (#$T0_SECTION_AGE_ISSUE); stopped-daemon story (#$T0_SECTION_STORY_ISSUE)"
  elif [[ ${#FAILED_CHECKS[@]} -gt 0 ]]; then
    known_issue "$T0_SECTION_STORY_ISSUE"
  fi
}
