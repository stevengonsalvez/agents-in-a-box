# shellcheck shell=bash
# P6e concurrency: the TUI, `ainb web` and the CLI against ONE daemon, with the
# sessions capability on, each surface reading and writing the daemon's table
# rather than its own copy of sessions.json.
#
#   {tui} {web} {tui,web} {tui,tui}   each with a CLI leg
#         │       │          │
#         └───────┴──────────┴──▶ one daemon, one $HOME, one sessions table
#
# Per combination, in order: every surface resolved the DAEMON (checked first,
# so a harness built without `test-support` fails here instead of proving the
# file path twice over); a session created from the TUI, which is not the CLI,
# reaches every other surface; an ASK answered on the web folds on the TUIs; a
# kill from the TUI, again not the CLI, leaves every surface; and after a
# reconcile this scenario asks for by hand, the table and the file hold the
# same sessions.
#
# Three things stand outside the combinations, each falsifying one rule:
#
#   * degraded and file-first: with no daemon up, a create still works and its
#     row is in `sessions.json`; a TUI started there says degraded, shows the
#     notice and switches once its daemon is up; and the session written while
#     degraded is in the table after the switch. A writer that wrote the table
#     first, or refused without one, fails the first half.
#   * the flock: a held `sessions.json.lock` makes a write WAIT for it, and the
#     write lands after the lock goes with the file still valid JSON. A writer
#     that took no lock would return at once and fail the wait.
#   * the kill switch: `AINB_SESSION_SOURCE=file` puts one process on the file
#     end to end, and what it wrote is in the table after a reconcile.
#
#   * the mirror: with `sessions.json` moved away, the rows ainb web serves
#     are still the table's. While both agree, equality cannot tell a surface
#     reading the table from one reading the file; with no file to read, it
#     can.
#
# Since the flip (P6e-6) the capability comes from the binary's own catalogue,
# so this scenario sets no switch: an ordinary build of these binaries is what
# is under test.
#
# Combination names match scripts/surface-combo-smoke.sh, which is where the
# four came from (P6e open question 5).

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="with the capability advertised by the build itself, the CLI and every TUI resolve the daemon's sessions table and what ainb web serves is that table, and one daemon serves the TUI, ainb web and the CLI together in every combination ({tui} {web} {tui,web} {tui,tui}): a session created from the TUI reaches every surface that is up, a kill from the TUI leaves every surface, an ASK answered on the web folds on the TUIs, and the table and sessions.json agree across a reconcile; separately, a write while degraded is file-first and reaches the table after the switch, a write waits for the sessions.json flock, the rows ainb web serves are still the table's when sessions.json is gone, and AINB_SESSION_SOURCE=file works end to end"

# How long a surface has to show a change another surface made.
P6_REACH=90
# How long the web has to show it: its snapshot is polled.
P6_WEB_REACH=180
# How long the sessions.json lock is held in front of a write.
P6_LOCK_HOLD=5

# The environment every process this scenario starts inherits. Since the flip
# (P6e-6) the capability comes from the binary's own catalogue, so there is no
# switch to set: an ordinary build of these binaries is what is under test.
p6_env() {
  # Every surface says which source it resolved at info. A short command and
  # `ainb web` log to stderr at warn unless RUST_LOG raises them, and the
  # first clause keeps the rest of what a TUI logs where it was.
  export RUST_LOG="info,ainb=info,ainb_core=info,ainb_app=info"
}

# p6_binaries_ready: the CLI runs, and the build under test advertises the
# sessions capability. A build that does not is one the flip never reached,
# and every check below would be proving the file path.
p6_binaries_ready() {
  if ! "$AINB_BIN" list --format json >/dev/null 2>&1; then
    check "the CLI runs" false
    return 1
  fi
  if ! "$AINB_BIN" hangar daemon status >/dev/null 2>&1; then
    check "the daemon verb answers" false
    return 1
  fi
  return 0
}

# p6_repo: the proof world's git repo, made once, which every create uses.
p6_repo() {
  local repo="$PROOF_WORLD/repo"
  if [[ ! -d "$repo/.git" ]]; then
    git init -q -b main "$repo"
    git -C "$repo" -c user.email=proof@localhost -c user.name=proof \
      commit -q --allow-empty -m init
  fi
  printf '%s' "$repo"
}

# ---------------------------------------------------------------------------
# Which source a process resolved
# ---------------------------------------------------------------------------
#
# A surface's source decides nothing visible on screen until the stores
# disagree, so each process says which one it resolved, once, with its pid
# (`session source resolved` in cli/util.rs, and `session source switched`
# when a degraded one moves to the daemon). A long-lived surface writes that
# to its JSONL log; a short CLI command writes it to stderr under RUST_LOG.

# p6_logged <pid> <line> <source>: that pid's own log says it.
p6_logged() {
  grep -rlF "\"pid\":$1" "$HOME/.agents-in-a-box/logs" 2>/dev/null \
    | xargs -r grep -hF "$2" 2>/dev/null \
    | grep -F "\"pid\":$1" \
    | grep -qF "\"source\":\"$3\""
}

# p6_pane_pid <pane>: the pid of the ainb process in a harness pane.
p6_pane_pid() { ptmux display-message -p -t "=$1:" '#{pane_pid}' 2>/dev/null; }

# p6_pane_source <pane> <source>: that surface resolved that source.
p6_pane_source() {
  local pid
  pid="$(p6_pane_pid "$1")"
  [[ -n "$pid" ]] || return 1
  p6_logged "$pid" 'session source resolved' "$2"
}

# p6_pane_switched <pane> <source>: that surface moved to that source. The
# move is made by a background retry, and the surface reports it on its next
# workspace load, so each attempt reloads the list: the reload is inside the
# retry loop, not in front of it.
p6_pane_switched() {
  local pid
  pid="$(p6_pane_pid "$1")"
  [[ -n "$pid" ]] || return 1
  p6_tui_reload "$1" >/dev/null 2>&1
  p6_logged "$pid" 'session source switched' "$2" && return 0
  # The same move as the operator sees it: the surface says sessions are back
  # on the daemon.
  p6_pane_logged_text "$1" 'sessions are back on it'
}

# p6_pane_logged_text <pane> <text>: that surface's own log, or the stderr the
# harness keeps for its pane, carries that line. Before a surface marks itself
# long lived the resolver's notices go to stderr, after it they go to the log,
# and which of the two a notice lands in depends on how early it was said.
p6_pane_logged_text() {
  local pane="$1" text="$2" pid
  pid="$(p6_pane_pid "$pane")"
  if [[ -n "$pid" ]] && grep -rlF "\"pid\":$pid" "$HOME/.agents-in-a-box/logs" 2>/dev/null \
    | xargs -r grep -qF "$text" 2>/dev/null; then
    return 0
  fi
  grep -qF "$text" "$PROOF_WORLD/$pane.stderr" 2>/dev/null
}

# `ainb web` has no session reader of its own: it runs `ainb list --frame` as
# a child and serves what that prints (`ainb-web/src/data.rs`, "Sessions come
# from `ainb list --frame`"). So the web process never resolves a session
# source, and asking its log for one would fail whatever the source was. What
# the proof checks instead is the rows it serves: they are the daemon's table,
# read through the same CLI path the source check above covers.

# p6_web_rows: the tmux names `ainb web` serves, sorted.
p6_web_rows() {
  curl -sS "$WEB_URL/api/snapshot" 2>/dev/null \
    | jq -r '.sessions[]? | (.tmux_session_name // .tmux_session // empty)' 2>/dev/null | sort
}

# p6_web_serves_the_table: what the web serves is what the table holds.
p6_web_serves_the_table() {
  local table web
  table="$(p6_table_names)"
  web="$(p6_web_rows)"
  [[ -n "$table" && "$table" == "$web" ]]
}

# p6_pane_said_degraded <pane>: the surface told the operator sessions are on
# the file for now. Long-lived surfaces put that notice in the log rather than
# writing over the screen.
p6_pane_said_degraded() {
  p6_pane_logged_text "$1" 'sessions are on the local sessions.json'
}

# p6_pane_source_lines <pane>: every source line that surface logged, for the
# record, so a red check says what the surface actually reported.
p6_pane_source_lines() {
  local pid
  pid="$(p6_pane_pid "$1")"
  [[ -n "$pid" ]] || { printf 'no pid for %s' "$1"; return 0; }
  grep -rhoF --include='*.jsonl' "\"message\":\"session source" \
    "$HOME/.agents-in-a-box/logs" 2>/dev/null | head -4 | tr '\n' ' '
  grep -rho "session source [a-z]*.*source=[a-z\"]*" "$PROOF_WORLD/$1.stderr" 2>/dev/null \
    | head -2 | tr '\n' ' '
}

# p6_cli_source <source> [env...]: one CLI read resolves that source. RUST_LOG
# raises the sink a short command gets, so the line is on its stderr.
p6_cli_source() {
  local want="$1"; shift
  local log="$PROOF_WORLD/p6-cli-source-$want.log"
  env "$@" RUST_LOG=ainb_app=info "$AINB_BIN" list --format json >/dev/null 2>"$log"
  grep -q 'session source resolved' "$log" && grep -q "source=\"\?$want\"\?" "$log"
}

# ---------------------------------------------------------------------------
# The two stores, each read on its own terms
# ---------------------------------------------------------------------------

# p6_cli_sessions: the session ids `ainb list` reports, one per line.
p6_cli_sessions() {
  "$AINB_BIN" list --format json 2>/dev/null | jq -r '.[].session_id'
}
p6_cli_has() { p6_cli_sessions | grep -qF "$1"; }
p6_cli_lacks() { ! p6_cli_has "$1"; }

# p6_table_names: the tmux names the daemon's TABLE holds, asked of the daemon
# over its own socket rather than through a surface, sorted.
p6_table_names() {
  rpc_call 1 1 workspace/session_list '{"limit":500}' 2>/dev/null \
    | tail -1 | jq -r '.result.sessions[]?.tmux_session_name' 2>/dev/null | sort
}
p6_table_has() { p6_table_names | grep -qxF "$1"; }
p6_table_lacks() { ! p6_table_has "$1"; }

# p6_file_names: the tmux names `sessions.json` holds, sorted.
p6_file_names() {
  jq -r '.sessions | keys[]?' "$HOME/.agents-in-a-box/sessions.json" 2>/dev/null | sort
}
p6_file_has() { p6_file_names | grep -qxF "$1"; }
p6_file_lacks() { ! p6_file_has "$1"; }
p6_file_parses() { jq -e . "$HOME/.agents-in-a-box/sessions.json" >/dev/null 2>&1; }

# p6_gone_from_both <tmux name>: neither store holds it.
p6_gone_from_both() { p6_table_lacks "$1" && p6_file_lacks "$1"; }

# p6_reconcile: ask the daemon for a reconcile pass; prints its reply.
p6_reconcile() {
  rpc_call 1 1 workspace/session_reconcile '{}' 2>/dev/null | tail -1
}

# p6_stores_agree: the table and the file hold the same tmux names.
p6_stores_agree() { [[ "$(p6_table_names)" == "$(p6_file_names)" ]]; }

# ---------------------------------------------------------------------------
# What a surface shows
# ---------------------------------------------------------------------------

# p6_row_needle <tmux name>: what the TUI's session list actually shows for a
# session. Not the tmux name: the row is the branch, `agents/7d755792`, and the
# 8 hex characters of it are in the tmux name too, last of the ones it carries
# (`tmux_repo--agents-7d755792--<id>_agents_7d755792`).
p6_row_needle() { printf '%s' "$1" | grep -oE '[0-9a-f]{8}' | tail -1; }

# p6_tui_reload <pane>: put the pane on the session list and refresh it with
# `f`, the list's own key, which re-reads the session store. Leaving the list
# and opening it again is not a reload: it draws what the last read left, so a
# second TUI would never see another surface's session. Same process, no
# restart: what the refresh reads is the process's session source.
p6_tui_reload() {
  p6_tui_to_list "$1" || return 1
  keys "$1" f
  sleep 1
}

# p6_tui_to_list <pane>: put the pane on the session list from wherever it is.
# One Escape is not enough from a Hangar screen such as the control centre,
# which steps back to the Hangar first, so this walks back until the home
# screen is in front and then opens the list.
p6_tui_to_list() {
  local pane="$1" attempt
  for attempt in 1 2 3 4; do
    pane_text "$pane" | grep -qE 'Workspaces \(' && return 0
    if pane_text "$pane" | grep -qE 'Stats +\[i\]'; then
      keys "$pane" s
      wait_screen "$pane" 'Workspaces \(' 20 && return 0
    fi
    keys "$pane" Escape
    sleep 0.7
  done
  pane_text "$pane" | grep -qE 'Workspaces \('
}

# p6_tui_live <pane>: the TUI is still drawing its own screen. Every negative
# check is anchored on this: a pane whose app died shows no row either, and
# would otherwise read as the row having gone.
p6_tui_live() { pane_text "$1" | grep -qE 'Sessions|Workspaces|Stats +\[i\]'; }

# p6_tui_has <pane> <needle> / p6_tui_lacks <pane> <needle>: the list shows it,
# or the surface is alive and does not. Each attempt reloads the list first, so
# these go straight to wait_for with the reload inside the retry loop.
p6_tui_has() {
  p6_tui_reload "$1" >/dev/null 2>&1
  p6_tui_live "$1" && pane_text "$1" | grep -qF -- "$2"
}
p6_tui_lacks() {
  p6_tui_reload "$1" >/dev/null 2>&1
  p6_tui_live "$1" && ! pane_text "$1" | grep -qF -- "$2"
}

# The web, the same way: alive first, then the row.
p6_web_alive() { curl -sS "$WEB_URL/api/snapshot" >/dev/null 2>&1; }
p6_web_has() {
  curl -sS "$WEB_URL/api/snapshot" 2>/dev/null | jq -e --arg n "$1" \
    '[.sessions[]? | tostring | select(test($n))] | length > 0' >/dev/null
}
p6_web_lacks() { p6_web_alive && ! p6_web_has "$1"; }

# p6_without_the_mirror <name> <tmux name>: move `sessions.json` away, ask the
# daemon to reconcile, and require the web to go on serving the table's rows.
#
# Post-flip a missing file is a lost mirror, not an empty store: the pass
# deletes nothing and still commits. A surface that had been reading the file
# would have nothing to serve here, so this is the check equality cannot make
# while both stores agree. The file is put back before anything writes again.
p6_without_the_mirror() {
  local name="$1" tmux_name="$2" file="$HOME/.agents-in-a-box/sessions.json"
  if [[ ! -s "$file" ]]; then
    check "$name: there is a sessions.json to move away" false
    return 1
  fi
  mv "$file" "$file.aside"
  observe "$name: reconcile with no sessions.json said $(p6_reconcile)"
  check "$name: the daemon keeps its rows with no sessions.json" \
    wait_for 30 p6_table_has "$tmux_name"
  check "$name: ainb web still serves the table's rows with no sessions.json" \
    wait_for "$P6_WEB_REACH" p6_web_serves_the_table
  check "$name: the session is still on the web with no sessions.json" \
    p6_web_has "$tmux_name"
  mv "$file.aside" "$file"
  return 0
}

# ---------------------------------------------------------------------------
# Writing from a surface that is not the CLI
# ---------------------------------------------------------------------------

# p6_tui_create <pane>: create a session through the TUI's own New Session
# flow. `n`, the repo typed as a path (the flow smart-parses it, so this does
# not depend on what the local scan found), then Launch on the Configure form.
# Sets P6_TUI_ID and P6_TUI_TMUX from the store once the session is there.
p6_tui_create() {
  local pane="$1" repo before after new
  repo="$(p6_repo)"
  before="$(p6_cli_sessions | sort)"
  keys "$pane" Escape
  sleep 0.5
  keys "$pane" n
  if ! wait_screen "$pane" 'Enter=Select|New Session' 20; then
    observe "the TUI's New Session flow did not open in $pane"
    return 1
  fi
  type_text "$pane" "$repo"
  sleep 0.5
  keys "$pane" Enter
  if ! wait_screen "$pane" 'Branch:' 30; then
    observe "the TUI's Configure form did not open in $pane"
    return 1
  fi
  # Launch is the last row, so one shift+tab from the opening focus is on it.
  keys "$pane" BTab
  if ! wait_screen "$pane" '\[ Launch \]' 10; then
    observe "the Configure form in $pane has no Launch row"
    return 1
  fi
  keys "$pane" Enter
  wait_screen "$pane" 'Creating Session|Creating Git worktree' 30 \
    || observe "the TUI did not say it was creating a session in $pane"
  # The store is where the new session lands, whichever source it is on.
  if ! wait_for "$P6_REACH" p6_new_session_landed "$before"; then
    observe "no new session reached the store after the TUI's New Session flow"
    return 1
  fi
  after="$(p6_cli_sessions | sort)"
  new="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -1)"
  [[ -n "$new" ]] || return 1
  P6_TUI_ID="$new"
  P6_TUI_TMUX="$("$AINB_BIN" list --format json 2>/dev/null \
    | jq -r --arg id "$new" '.[] | select(.session_id == $id) | .tmux_session_name')"
  [[ -n "$P6_TUI_TMUX" ]]
}

# p6_new_session_landed <ids before>: the store holds a session it did not.
p6_new_session_landed() {
  local after
  after="$(p6_cli_sessions | sort)"
  [[ -n "$(comm -13 <(printf '%s\n' "$1") <(printf '%s\n' "$after"))" ]]
}

# p6_tui_kill <pane> <needle>: kill the selected session from the TUI's own
# list. `d` on a Claude row opens `[ Stop ] [ Delete ] [ Cancel ]` with Stop
# selected, so one `right` moves to Delete and enter takes it.
p6_tui_kill() {
  local pane="$1" needle="$2" row
  p6_tui_reload "$pane"
  row="$(row_of "$pane" "$needle")"
  if [[ -z "$row" ]]; then
    observe "no row for $needle in $pane to kill"
    return 1
  fi
  click "$pane" 4 "$row"
  keys "$pane" d
  if ! wait_screen "$pane" 'Stop or Delete Session|Delete Session|Kill tmux Session' 20; then
    observe "the TUI in $pane did not ask before deleting"
    return 1
  fi
  if pane_text "$pane" | grep -qF 'Stop or Delete Session'; then
    keys "$pane" Right
  fi
  keys "$pane" Enter
  return 0
}

# ---------------------------------------------------------------------------
# One combination
# ---------------------------------------------------------------------------

# p6_surfaces_down [panes...]: close what a combination started, its web and
# its daemon. Called on every way out of a combination, the early ones too, so
# the next combination starts from the same place.
p6_surfaces_down() {
  local pane
  for pane in "$@"; do
    quit_tui "$pane" 2>/dev/null || ptmux kill-session -t "=$pane:" 2>/dev/null || true
  done
  ptmux kill-session -t "=web:" 2>/dev/null || true
  "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true
}

# p6_daemon_down: the world's daemon is not answering.
p6_daemon_down() { ! daemon_running; }

# p6_combination <name> <tui count> <web:0|1>: one combination, end to end.
p6_combination() {
  local name="$1" tuis="$2" web="$3" i
  local panes=()
  say "combination $name"
  observe "=== $name ==="

  # One daemon for this combination, started before any surface, so {web}
  # (which autostarts nothing) has the same daemon the TUIs would have.
  "$AINB_BIN" hangar daemon start >"$PROOF_WORLD/p6-daemon-$name.txt" 2>&1 || true
  if ! wait_for 45 daemon_running; then
    observe "$name: daemon start said $(tail -2 "$PROOF_WORLD/p6-daemon-$name.txt" | tr '\n' ' ')"
    check "$name: one daemon is up for every surface" false
    p6_surfaces_down
    return 1
  fi
  check "$name: one daemon is up for every surface" true

  for ((i = 1; i <= tuis; i++)); do
    if ! start_tui "tui$i"; then
      check "$name: the TUI in tui$i reaches the home screen" false
      p6_surfaces_down "${panes[@]}"
      return 1
    fi
    panes+=("tui$i")
    open_session_list "tui$i"
  done
  if [[ "$web" == "1" ]] && ! start_web; then
    check "$name: ainb web answers" false
    p6_surfaces_down "${panes[@]}"
    return 1
  fi

  # 0. Every surface is on the daemon's table, checked before anything is
  # created: a harness built without `test-support` stays on the file, and
  # this is where that has to fail rather than pass on the file path.
  check "$name: the CLI resolved the daemon's sessions table" \
    wait_for 30 p6_cli_source daemon
  for i in "${panes[@]}"; do
    check "$name: the TUI in $i resolved the daemon's sessions table" \
      wait_for 60 p6_pane_source "$i" daemon
  done


  # 1. A session created from a surface that is NOT the CLI reaches the
  # others. `ainb web` has no create (its only writes are the answer and the
  # attached pane), so with no TUI up the CLI creates it instead.
  local created tmux_name row from
  if ((tuis > 0)); then
    if ! p6_tui_create "${panes[0]}"; then
      check "$name: a session was created from the TUI" false
      p6_surfaces_down "${panes[@]}"
      return 1
    fi
    created="$P6_TUI_ID"
    tmux_name="$P6_TUI_TMUX"
    from="the TUI in ${panes[0]}"
    check "$name: a session was created from the TUI, not the CLI" true
  else
    if ! fixture_session; then
      check "$name: a session was created" false
      p6_surfaces_down "${panes[@]}"
      return 1
    fi
    created="$FIXTURE_ID"
    tmux_name="$FIXTURE_TMUX"
    from="the CLI"
  fi
  row="$(p6_row_needle "$tmux_name")"
  observe "$name: $from created $tmux_name ($created), listed as $row"
  check "$name: the CLI lists the session $from created" wait_for 30 p6_cli_has "$created"
  check "$name: the session $from created is in the daemon's table" \
    wait_for 30 p6_table_has "$tmux_name"
  for i in "${panes[@]}"; do
    check "$name: the session $from created reached $i" \
      wait_for "$P6_REACH" p6_tui_has "$i" "$row"
    pane_text "$i" >"$NODE_DIR/$name-$i-list.txt" 2>/dev/null || true
    CAPTURES+=("$name-$i-list.txt")
  done
  if [[ "$web" == "1" ]]; then
    check "$name: the session $from created reached ainb web" \
      wait_for "$P6_WEB_REACH" p6_web_has "$tmux_name"
    # And not merely reached it: what the web serves is the table, row for row.
    check "$name: the rows ainb web serves are the daemon's table" \
      wait_for "$P6_WEB_REACH" p6_web_serves_the_table
    observe "$name: ainb web serves: $(p6_web_rows | tr '\n' ' ')"
    # While the two stores agree, equality says nothing about which one a
    # surface read. With the mirror moved away, it does: post-flip the table
    # keeps its rows, and a surface still serving them is reading the table.
    p6_without_the_mirror "$name" "$tmux_name"
  fi

  # 2. An ASK answered on one surface folds on the others.
  if [[ "$web" == "1" ]]; then
    # The ASK is raised through a session's own pane, so it needs a fixture
    # session of this combination's own: the one the TUI made is not the
    # hook's, and a stale one from an earlier combination has no pane left.
    if ! fixture_session; then
      check "$name: a session to raise the ASK on" false
      p6_surfaces_down "${panes[@]}"
      return 1
    fi
    raise_ask "Proof P6e: $name?" "p6-$name" >/dev/null
    if web_card_id "Proof P6e" "p6-card-$name" "$P6_WEB_REACH"; then
      # The card is drawn on the control centre, not on the session list, so
      # each TUI is put there and the card is seen BEFORE it is answered: an
      # absence on a screen the card never reaches proves nothing.
      local saw_card=1
      for i in "${panes[@]}"; do
        if ! open_hangar_screen "$i" control 'Control  ·'; then
          check "$name: the control centre opens on $i" false
          saw_card=0
          continue
        fi
        check "$name: the card is on $i's control centre before it is answered" \
          wait_screen "$i" '1 need you' 30
        wait_screen "$i" '1 need you' 1 || saw_card=0
      done
      web_answer "$WEB_CARD_ID" 1 >"$PROOF_WORLD/p6-answer-$name.json"
      observe "$name: web answered $(tr -d '\n' <"$PROOF_WORLD/p6-answer-$name.json")"
      check "$name: the web answer was delivered" \
        grep -q '"outcome": *"delivered"' "$PROOF_WORLD/p6-answer-$name.json"
      for i in "${panes[@]}"; do
        if ((saw_card)); then
          check "$name: the answered card retired on $i, which is still up" \
            wait_screen "$i" '0 need you' "$P6_REACH"
        fi
        # Back to the list, which the checks after this one read.
        p6_tui_reload "$i" \
          || observe "$name: $i did not come back to the session list"
      done
    else
      check "$name: the web lists the card within ${P6_WEB_REACH} s" false
    fi
  fi

  # 3. The kill, from the TUI where there is one: again a write from a surface
  # that is not the CLI. Every surface drops the row, and so do both stores.
  if ((tuis > 0)); then
    check "$name: the session was killed from the TUI, not the CLI" \
      p6_tui_kill "${panes[0]}" "$row"
  else
    "$AINB_BIN" kill "$created" --force >"$PROOF_WORLD/p6-kill-$name.txt" 2>&1 \
      || observe "$name: ainb kill said $(tail -1 "$PROOF_WORLD/p6-kill-$name.txt")"
  fi
  check "$name: the CLI no longer lists the killed session" \
    wait_for "$P6_REACH" p6_cli_lacks "$created"
  check "$name: the killed session left the daemon's table" \
    wait_for "$P6_REACH" p6_table_lacks "$tmux_name"
  for i in "${panes[@]}"; do
    check "$name: the killed session left $i, which is still up" \
      wait_for "$P6_REACH" p6_tui_lacks "$i" "$row"
  done
  if [[ "$web" == "1" ]]; then
    check "$name: the killed session left ainb web, which is still answering" \
      wait_for "$P6_WEB_REACH" p6_web_lacks "$tmux_name"
  fi

  # 4. A reconcile asked for by hand, then the two stores hold the same
  # sessions: nothing the surfaces did left them disagreeing.
  observe "$name: reconcile said $(p6_reconcile)"
  check "$name: the table and sessions.json hold the same sessions" \
    wait_for 30 p6_stores_agree
  observe "$name: table: $(p6_table_names | tr '\n' ' ')"
  observe "$name: sessions.json: $(p6_file_names | tr '\n' ' ')"
  check "$name: the killed session is in neither store" p6_gone_from_both "$tmux_name"

  p6_surfaces_down "${panes[@]}"
  return 0
}

# ---------------------------------------------------------------------------
# The three rules that stand on their own
# ---------------------------------------------------------------------------

# The file the lock holder watches for: it lets the lock go when this appears.
p6_lock_sentinel() { printf '%s' "$PROOF_WORLD/p6-release-the-lock"; }

# p6_hold_the_lock: take the `sessions.json` lock in another process and keep
# it until [`p6_release_the_lock`], the way the daemon's reconcile pass holds
# it while it reads the file.
p6_hold_the_lock() {
  rm -f "$(p6_lock_sentinel)"
  python3 -c "
import fcntl, os, sys, time
lock, sentinel = sys.argv[1], sys.argv[2]
with open(lock, 'a+') as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    for _ in range(1800):
        if os.path.exists(sentinel):
            break
        time.sleep(0.1)
" "$HOME/.agents-in-a-box/sessions.json.lock" "$(p6_lock_sentinel)" &
  P6_LOCK_PID=$!
  sleep 1
}

# p6_daemon_up: bring the world's daemon up, for a leg that needs one of its
# own. The leg before it may have left none: the degraded leg's daemon goes
# with the TUI that started it.
p6_daemon_up() {
  daemon_running && return 0
  "$AINB_BIN" hangar daemon start >"$PROOF_WORLD/p6-daemon-leg.txt" 2>&1 || true
  wait_for 45 daemon_running
}

# p6_release_the_lock: let it go, and wait until it is gone.
p6_release_the_lock() {
  touch "$(p6_lock_sentinel)"
  [[ -n "${P6_LOCK_PID:-}" ]] && wait "$P6_LOCK_PID" 2>/dev/null
  P6_LOCK_PID=""
  return 0
}

# p6_degraded_and_file_first: no daemon, then one. A create with nothing to
# write a table row to still works and is in the file; a TUI started there
# says degraded, shows the notice and switches; and the session written while
# degraded is in the table after the switch.
p6_degraded_and_file_first() {
  say "degraded, and the file first"
  observe "=== degraded ==="
  "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true
  if ! wait_for 30 p6_daemon_down; then
    check "degraded: the daemon is down before the surfaces start" false
    p6_surfaces_down
    return 1
  fi
  check "degraded: the daemon is down before the surfaces start" true
  check "degraded: the CLI says it is degraded with no daemon up" \
    wait_for 30 p6_cli_source degraded

  # The create: no table to write to, so this proves the row goes to the file
  # first and the session is live either way.
  if ! fixture_session; then
    check "degraded: a session is created with no daemon up" false
    p6_surfaces_down
    return 1
  fi
  local degraded_tmux="$FIXTURE_TMUX" degraded_id="$FIXTURE_ID"
  check "degraded: a session is created with no daemon up" true
  check "degraded: its row is in sessions.json, written before any table row" \
    p6_file_has "$degraded_tmux"

  # The TUI autostarts a daemon of its own, so "no daemon up" is a race it
  # usually wins. The window is held open instead: with the `sessions.json`
  # lock taken, that daemon cannot finish its first pass, so it answers its
  # session reads not-ready and every surface on it is degraded until the
  # lock goes.
  p6_hold_the_lock
  if ! start_tui tuid; then
    p6_release_the_lock
    check "degraded: the TUI reaches the home screen with no daemon up" false
    p6_surfaces_down tuid
    return 1
  fi
  check "degraded: the TUI resolved the file while no daemon could answer" \
    wait_for 60 p6_pane_source tuid degraded
  check "degraded: the TUI said sessions are on the local file for now" \
    wait_for 60 p6_pane_said_degraded tuid
  p6_release_the_lock
  check "degraded: the daemon finished its first pass behind it" \
    wait_for 60 daemon_running
  check "degraded: the TUI switched to the daemon's table" \
    wait_for 120 p6_pane_switched tuid daemon

  observe "degraded: reconcile said $(p6_reconcile)"
  check "degraded: the session written while degraded is in the table" \
    wait_for 60 p6_table_has "$degraded_tmux"
  open_session_list tuid
  check "degraded: the TUI lists it after the switch" \
    wait_for "$P6_REACH" p6_tui_has tuid "$(p6_row_needle "$degraded_tmux")"

  observe "degraded: the TUI's own source lines: $(p6_pane_source_lines tuid)"
  P6_DEGRADED_ID="$degraded_id"
  P6_DEGRADED_TMUX="$degraded_tmux"
  quit_tui tuid 2>/dev/null || ptmux kill-session -t "=tuid:" 2>/dev/null || true
  return 0
}


# p6_flock_is_taken: a write waits for the lock rather than walking over it.
# The session killed here is the one the degraded leg created.
p6_flock_is_taken() {
  say "the sessions.json flock"
  observe "=== flock ==="
  local id="${P6_DEGRADED_ID:-}" tmux_name="${P6_DEGRADED_TMUX:-}" started waited
  if ! p6_daemon_up; then
    check "flock: a daemon is up behind the write" false
    return 1
  fi
  if [[ -z "$id" ]]; then
    check "flock: there is a session to write to" false
    return 1
  fi

  # The control: the same write, with nothing holding the lock. Without it a
  # writer that ignored the lock and merely took its time would pass the
  # measurement below.
  if ! fixture_session; then
    check "flock: a second session for the control run" false
    return 1
  fi
  local control_id="$FIXTURE_ID" control=$SECONDS
  "$AINB_BIN" kill "$control_id" --force >"$PROOF_WORLD/p6-flock-control.txt" 2>&1 \
    || observe "flock: the control kill said $(tail -1 "$PROOF_WORLD/p6-flock-control.txt")"
  control=$((SECONDS - control))
  observe "flock: the same write with no lock held took ${control}s"

  p6_hold_the_lock
  started=$SECONDS
  "$AINB_BIN" kill "$id" --force >"$PROOF_WORLD/p6-flock-kill.txt" 2>&1 &
  local writer=$!
  # The write is in front of a lock it cannot have; it is still waiting when
  # the lock goes, which is what the wait below measures.
  sleep "$P6_LOCK_HOLD"
  p6_release_the_lock
  wait "$writer" 2>/dev/null \
    || observe "flock: ainb kill said $(tail -1 "$PROOF_WORLD/p6-flock-kill.txt")"
  waited=$((SECONDS - started))
  observe "flock: the write took ${waited}s against a lock held for ${P6_LOCK_HOLD}s"
  check "flock: the held write waited about as long as the lock was held" \
    test "$((waited - control))" -ge $((P6_LOCK_HOLD - 2))
  check "flock: the write landed once the lock went" \
    wait_for 60 p6_cli_lacks "$id"
  check "flock: sessions.json is still valid JSON" p6_file_parses
  check "flock: the killed session is in neither store" \
    wait_for 60 p6_gone_from_both "$tmux_name"
  return 0
}

# p6_kill_switch: AINB_SESSION_SOURCE=file, once, end to end. The process
# writes the file and nothing else; the daemon picks the row up on the next
# reconcile, which is what makes the switch safe to reach for.
p6_kill_switch() {
  say "the kill switch"
  observe "=== kill switch ==="
  if ! p6_daemon_up; then
    check "kill switch: a daemon is up to reconcile into" false
    return 1
  fi
  check "kill switch: a process with AINB_SESSION_SOURCE=file says so" \
    p6_cli_source file AINB_SESSION_SOURCE=file

  local repo out id tmux_name
  repo="$(p6_repo)"
  out="$(cd "$repo" && env AINB_SESSION_SOURCE=file "$AINB_BIN" run --repo "$repo" \
    --worktree --format json </dev/null 2>&1)"
  id="$(printf '%s\n' "$out" | sed -n 's/^ *Session ID: *//p' | head -1)"
  tmux_name="$(printf '%s\n' "$out" | sed -n 's/^ *Tmux Session: *//p' | head -1)"
  if [[ -z "$tmux_name" ]]; then
    observe "kill switch: ainb run said $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
    check "kill switch: a session is created with the file forced" false
    return 1
  fi
  check "kill switch: a session is created with the file forced" true
  check "kill switch: its row is in sessions.json" p6_file_has "$tmux_name"
  observe "kill switch: reconcile said $(p6_reconcile)"
  check "kill switch: the daemon's table has it after a reconcile" \
    wait_for 60 p6_table_has "$tmux_name"

  env AINB_SESSION_SOURCE=file "$AINB_BIN" kill "$id" --force \
    >"$PROOF_WORLD/p6-killswitch-kill.txt" 2>&1 \
    || observe "kill switch: ainb kill said $(tail -1 "$PROOF_WORLD/p6-killswitch-kill.txt")"
  check "kill switch: the row leaves sessions.json" wait_for 60 p6_file_lacks "$tmux_name"
  observe "kill switch: reconcile said $(p6_reconcile)"
  check "kill switch: the row leaves the table too" wait_for 60 p6_table_lacks "$tmux_name"
  return 0
}

scenario() {
  p6_env
  p6_binaries_ready || return

  # Each combination brings up one daemon and every surface it names against
  # it, which is the point of the node.
  p6_combination tui 1 0 || return
  p6_combination web 0 1 || return
  p6_combination tui-web 1 1 || return
  p6_combination tui-tui 2 0 || return

  # The three rules, on a daemon of their own. The degraded leg starts with
  # none up, so it must run before the two that need one.
  p6_degraded_and_file_first || { p6_surfaces_down; return; }
  p6_flock_is_taken || true
  p6_kill_switch || true
  p6_surfaces_down

  if [[ -s "$HOME/.agents-in-a-box/sessions.json" ]]; then
    redact_host <"$HOME/.agents-in-a-box/sessions.json" >"$NODE_DIR/sessions-json.txt"
    CAPTURES+=("sessions-json.txt")
  fi
  save_output daemon-status "$AINB_BIN" hangar daemon status
}
