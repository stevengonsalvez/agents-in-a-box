# shellcheck shell=bash
# D1 desktop shell: the real window, headless in a private world, reaching its
# sessions sidebar over the same daemon a separate CLI call finds, and seeing a
# session the CLI creates while it is open.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="the desktop window renders the sessions sidebar from framed sections, over the daemon a separate CLI call finds, holding exactly one desktop connection row while it runs and none after it closes, and a session the CLI creates reaches the open sidebar without a restart"

# The window under test: a debug build of the desktop shell, built with the
# `bundled` feature so it serves `ui/dist` itself rather than a dev server
# (`cargo build --features bundled` in `crates/ainb-desktop`).
DESKTOP_BIN="${AINB_DESKTOP_BIN:-$AINB_TUI_DIR/crates/ainb-desktop/target/debug/ainb-desktop}"
# A debug build takes its sidecar from here; a bundle carries it beside itself.
DESKTOP_DAEMON_BIN="${AINB_DESKTOP_DAEMON_BIN:-${CARGO_TARGET_DIR:-$AINB_TUI_DIR/target}/debug/ainb-hangar-daemon}"

DESKTOP_LOG=""

# start_desktop: the window in a harness pane, under a headless X server,
# waited on until its renderer has applied a batch.
start_desktop() {
  DESKTOP_LOG="$AINB_HANGAR_HOME/desktop.log"
  ptmux new-session -d -s desktop -x "$PROOF_COLS" -y "$PROOF_ROWS" \
    "env -u TMUX -u TMUX_PANE AINB_DESKTOP_DAEMON_BIN='$DESKTOP_DAEMON_BIN' \
       xvfb-run -a '$DESKTOP_BIN' 2>>'$PROOF_WORLD/desktop.stderr'"
  wait_for 90 grep -q "renderer applied" "$DESKTOP_LOG" 2>/dev/null
}

# applied_sessions: the session count on the last batch the renderer applied.
applied_sessions() {
  sed -n 's/.*renderer applied .*sessions=\([0-9][0-9]*\).*/\1/p' "$DESKTOP_LOG" 2>/dev/null | tail -1
}

# applied_sections: the section names on the first batch it applied.
applied_sections() {
  sed -n 's/.*renderer applied sections=\(\[[^]]*\]\).*/\1/p' "$DESKTOP_LOG" 2>/dev/null | head -1
}

# sessions_at_least <n>: the renderer has applied a batch carrying n rows.
sessions_at_least() {
  local seen
  seen="$(applied_sessions)"
  [[ -n "$seen" ]] && ((seen >= $1))
}

scenario() {
  if [[ ! -x "$DESKTOP_BIN" ]]; then
    check "the desktop shell is built at $DESKTOP_BIN" false
    return
  fi
  if ! command -v xvfb-run >/dev/null; then
    check "xvfb-run is installed, for a window with no display" false
    return
  fi

  fixture_session || { check "the CLI seeded a session before the window opened" false; return; }

  if ! start_desktop; then
    check "the desktop window applied a frame batch within 90 s" false
    [[ -s "$PROOF_WORLD/desktop.stderr" ]] && observe "window stderr: $(tail -3 "$PROOF_WORLD/desktop.stderr")"
    return
  fi
  cp "$DESKTOP_LOG" "$NODE_DIR/desktop-log.txt" 2>/dev/null && CAPTURES+=("desktop-log.txt")

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
}
