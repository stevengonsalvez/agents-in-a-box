# shellcheck shell=bash
# W0-wire (PR #935): protocol version and capability catalogue in hello, so a
# second, separate CLI process negotiates with the SAME daemon the TUI uses.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a separate ainb CLI call reaches the TUI's own daemon (its own cli row beside the TUI row), and that daemon negotiates auth/hello: protocol 1 selected with a capability catalogue, protocol 99 refused with -32007"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  local pid daemon_pid
  pid="$(tui_pid tui)"
  wait_for 10 rows_is tui "$pid" ge 1

  save_output daemon-status "$AINB_BIN" hangar daemon status
  daemon_pid="$(sed -n 's/.*running (pid \([0-9]*\).*/\1/p' "$NODE_DIR/daemon-status.txt")"
  observe "daemon pid from a CLI process: ${daemon_pid:-none}"
  check "the daemon a CLI call finds is running" test -n "$daemon_pid"
  check "that daemon was started by the TUI (its parent chain holds the TUI pid)" \
    bash -c "ps -o ppid= -p $daemon_pid | grep -qw $pid || grep -qx 'AINB_HANGAR_HOME=$AINB_HANGAR_HOME' <(tr '\0' '\n' </proc/$daemon_pid/environ)"

  local json my_cli
  json="$(connections_json)"
  printf '%s\n' "$json" | redact_host >"$NODE_DIR/connections.json"
  CAPTURES+=("connections.json")
  my_cli="$(jq '[.connections[].surface | select(.kind == "cli")] | length' <<<"$json")"
  observe "cli rows seen by the listing: $my_cli; tui rows at pid $pid: $(jq --argjson p "$pid" '[.connections[].surface | select(.kind=="tui" and .pid==$p)] | length' <<<"$json")"
  check "the listing sees its own cli row" test "$my_cli" -ge 1
  # shellcheck disable=SC2016  # $p is a jq variable
  check "the listing sees the TUI's row on the same daemon" \
    jq -e --argjson p "$pid" 'any(.connections[].surface; .kind == "tui" and .pid == $p)' <<<"$json"

  # The hello handshake itself, framed as the wire contract frames it.
  local agreed refused
  agreed="$(hello_probe 1 1)"
  refused="$(hello_probe 99 99)"
  printf '%s\n%s\n' "$agreed" "$refused" >"$NODE_DIR/hello-probe.txt"
  CAPTURES+=("hello-probe.txt")
  observe "hello {1,1}: $(jq -c '.result | {protocol, selected, capabilities, daemon_version}' <<<"$agreed")"
  observe "hello {99,99}: $(jq -c '.error | {code, message}' <<<"$refused")"
  check "a client speaking protocol 1 is served with selected=1 and a capability catalogue" \
    jq -e '.result.selected == 1 and (.result.capabilities | test("^[1-9][0-9]* strings$"))' <<<"$agreed"
  check "a client speaking only protocol 99 is refused with -32007 PROTOCOL_INCOMPATIBLE" \
    jq -e '.error.code == -32007' <<<"$refused"
}
