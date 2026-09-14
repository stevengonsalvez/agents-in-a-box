# shellcheck shell=bash
# S-B ConnectionRegistry: the daemon lists every connected surface by kind,
# pid and host (G6 step 6).

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with one TUI, ainb web and the listing CLI connected, hangar connections list shows exactly one tui row (the TUI's pid), one web row and one cli row"

scenario() {
  start_tui tui || { check "the TUI reaches the home screen" false; return; }
  local pid
  pid="$(tui_pid tui)"
  start_web || { check "ainb web answers /api/snapshot" false; return; }
  wait_for 10 rows_is web "" ge 1

  # A second tui row at the TUI's pid (#1040) can come and go, so sample.
  local sample max_at_pid=0 seen
  for sample in 1 2 3 4 5 6; do
    seen="$(count_kind tui "$pid")"
    printf 't=%ss tui rows at TUI pid: %s\n' "$sample" "$seen" >>"$NODE_DIR/tui-row-samples.txt"
    ((seen > max_at_pid)) && max_at_pid="$seen"
    sleep 1
  done
  CAPTURES+=("tui-row-samples.txt")
  observe "most tui rows at the TUI's pid across 6 one-second samples: $max_at_pid"

  local json
  json="$(connections_json)"
  printf '%s\n' "$json" | redact_host >"$NODE_DIR/connections.json"
  CAPTURES+=("connections.json")
  save_output connections-list "$AINB_BIN" hangar connections list
  capture tui tui-home

  local tui_rows tui_at_pid web_rows cli_rows
  tui_rows="$(jq '[.connections[].surface | select(.kind == "tui")] | length' <<<"$json")"
  tui_at_pid="$(jq --argjson p "$pid" '[.connections[].surface | select(.kind == "tui" and .pid == $p)] | length' <<<"$json")"
  web_rows="$(jq '[.connections[].surface | select(.kind == "web")] | length' <<<"$json")"
  cli_rows="$(jq '[.connections[].surface | select(.kind == "cli")] | length' <<<"$json")"
  observe "rows: tui=$tui_rows (at TUI pid $pid: $tui_at_pid), web=$web_rows, cli=$cli_rows"

  check "every row carries a pid and the daemon host" \
    jq -e '[.connections[] | select((.surface.pid // 0) == 0 or (.host // "") == "")] | length == 0' <<<"$json"
  check "exactly one web row" test "$web_rows" -eq 1
  check "exactly one cli row (the listing itself)" test "$cli_rows" -eq 1
  check "exactly one tui row, at the TUI's pid, in the final listing" test "$tui_rows" -eq 1 -a "$tui_at_pid" -eq 1
  check "never more than one tui row at the TUI's pid in any sample" test "$max_at_pid" -le 1
  if ((tui_at_pid > 1 || max_at_pid > 1)); then known_issue 1040; fi
}
