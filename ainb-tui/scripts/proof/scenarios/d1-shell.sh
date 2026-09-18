# shellcheck shell=bash
# D1 desktop shell: the real window, headless in a private world, reaching its
# sessions sidebar over the same daemon a separate CLI call finds, and seeing a
# session the CLI creates while it is open.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="the desktop window renders the sessions sidebar from framed sections, over the daemon a separate CLI call finds, holding exactly one desktop connection row while it runs and none after it closes, and a session the CLI creates reaches the open sidebar without a restart"

scenario() {
  if [[ ! -x "$DESKTOP_BIN" ]]; then
    check "the desktop shell is built at $DESKTOP_BIN" false
    return
  fi
  if ! command -v xvfb-run >/dev/null; then
    # Not a failure: a box with no headless X server has falsified nothing
    # about the window, so the result says why the node could not run.
    skip "xvfb-run is not installed, so the window has no display to open on"
    return
  fi

  fixture_session || { check "the CLI seeded a session before the window opened" false; return; }

  if ! start_desktop; then
    check "the desktop window applied a frame batch within 90 s" false
    [[ -s "$PROOF_WORLD/desktop.stderr" ]] && observe "window stderr: $(tail -3 "$PROOF_WORLD/desktop.stderr")"
    return
  fi
  observe "sections in the first batch the renderer applied: $(applied_sections)"
  check "the renderer applied the sessions section" \
    grep -q '"sessions"' <<<"$(applied_sections)"
  check "the sidebar held the seeded session within 60 s" wait_for 60 sessions_at_least 1
  observe "session rows the renderer held: $(applied_sessions)"

  # The daemon: the window's own, found by a separate CLI process.
  save_output daemon-status "$AINB_BIN" hangar daemon status
  local daemon_pid
  daemon_pid="$(sed -n 's/.*running (pid \([0-9]*\).*/\1/p' "$NODE_DIR/daemon-status.txt")"
  observe "daemon pid from a CLI process: ${daemon_pid:-none}"
  check "a separate CLI call finds a running daemon" test -n "$daemon_pid"
  # The window's own sidecar names the daemon it attached to, so the two are
  # compared rather than each being asserted alive on its own.
  local window_pid
  window_pid="$(sed -n 's/.*attached to the hangar daemon.*daemon_pid=Some(\([0-9][0-9]*\)).*/\1/p' "$DESKTOP_LOG" 2>/dev/null | tail -1)"
  observe "daemon pid the window attached to: ${window_pid:-not logged}"
  if [[ -n "$window_pid" ]]; then
    check "the window and the CLI found the same daemon" test "$window_pid" = "$daemon_pid"
  fi

  # The window registers its connection once its sidecar has said hello, which
  # is not done the moment the first frame is drawn.
  wait_for 60 rows_is desktop "" ge 1
  local json desktop_rows
  json="$(connections_json)"
  printf '%s\n' "$json" | redact_host >"$NODE_DIR/connections.json"
  CAPTURES+=("connections.json")
  desktop_rows="$(jq '[.connections[].surface | select(.kind == "desktop")] | length' <<<"$json")"
  observe "desktop rows in the connection registry while the window runs: $desktop_rows"
  check "exactly one desktop row while the window runs" test "$desktop_rows" -eq 1

  # A session created by another process reaches the open window.
  local before
  before="$(applied_sessions)"
  fixture_session || { check "the CLI created a second session while the window was open" false; return; }
  check "the new session reached the open sidebar without a restart" \
    wait_for 90 sessions_at_least "$((before + 1))"
  observe "session rows after the CLI created one more: $(applied_sessions)"

  # Closing the window takes its row with it.
  local pid
  pid="$(tui_pid desktop)"
  ptmux send-keys -t "=desktop:" C-c
  wait_for 20 bash -c "! kill -0 $pid 2>/dev/null"
  wait_for 20 bash -c "test \"\$('$AINB_BIN' hangar connections list --format json 2>/dev/null | jq '[.connections[].surface | select(.kind == \"desktop\")] | length')\" -eq 0"
  local after
  after="$(connections_json | jq '[.connections[].surface | select(.kind == "desktop")] | length')"
  observe "desktop rows after the window closed: $after"
  check "no desktop row after the window closed" test "$after" -eq 0

  # Taken at the end, so the capture holds the whole run rather than the first
  # seconds of it.
  if [[ -s "$DESKTOP_LOG" ]]; then
    tail -n 400 "$DESKTOP_LOG" | redact_host >"$NODE_DIR/desktop-log.txt"
    CAPTURES+=("desktop-log.txt")
  fi
}
