# shellcheck shell=bash
# #1094: the host reports the tmux session it runs in, and the reducer never
# looks it up. A TUI started inside a tmux session lists that session like any
# other, and selecting it shows a placeholder instead of previewing itself.

# shellcheck disable=SC2034  # read by write_result in lib.sh
EXPECT="a TUI started inside tmux session own-tui on the private -L proof server lists own-tui and a neighbour under Other tmux; selecting own-tui shows 'This is the tmux session ainb is running in' with the session name, opens no preview client on own-tui and keeps the TUI alive; selecting the neighbour previews its live output through a read-only client"

# clients_on <session>: tmux client flags on a session of the harness server.
clients_on() { ptmux list-clients -t "=$1" -F '#{client_flags}' 2>/dev/null; }

scenario() {
  # The one scenario whose TUI keeps TMUX: inside a `-L proof` pane it then
  # treats the harness server as its tmux, which is exactly the setup #1094
  # is about. Every other scenario unsets TMUX (see start_tui).
  ptmux new-session -d -s neighbour -x "$PROOF_COLS" -y "$PROOF_ROWS" \
    "bash -c 'n=0; while :; do n=\$((n + 1)); echo neighbour tick \$n; sleep 1; done'"
  ptmux new-session -d -s own-tui -x "$PROOF_COLS" -y "$PROOF_ROWS" "'$AINB_BIN' 2>>'$PROOF_WORLD/own-tui.stderr'"
  if ! wait_screen own-tui 'Stats +\[i\]' 45; then
    capture own-tui never-home
    check "the TUI inside own-tui reaches the home screen" false
    return
  fi
  sleep 1.5
  local pid
  pid="$(tui_pid own-tui)"
  observe "TUI pid $pid inside tmux session own-tui"

  keys own-tui s
  check "the session list shows both sessions under Other tmux" \
    wait_screen own-tui 'Other tmux \(2\)' 20
  check "own-tui is listed as a row" wait_screen own-tui '─ ○ own-tui' 5
  capture own-tui session-list

  # Put the cursor on neighbour first, wherever the list starts it.
  local i
  for i in 1 2 3; do
    pane_text own-tui | grep -qE '▶ [0-9]+ ☐ [├└]─ ○ neighbour' && break
    keys own-tui Up
    sleep 1
  done
  check "the neighbour previews its live output" wait_screen own-tui 'neighbour tick [0-9]+' 10
  observe "clients with neighbour selected: own-tui [$(clients_on own-tui | paste -sd';' -)], neighbour [$(clients_on neighbour | paste -sd';' -)]"
  check "the neighbour preview is a read-only client" bash -c "tmux -L proof list-clients -t '=neighbour' -F '#{client_flags}' | grep -q read-only"
  capture own-tui neighbour-selected

  for i in 1 2 3; do
    pane_text own-tui | grep -qE '▶ [0-9]+ ☐ [├└]─ ○ own-tui' && break
    keys own-tui Down
    sleep 1
  done
  check "the own session shows the placeholder" \
    wait_screen own-tui 'This is the tmux session ainb is running in' 10
  check "the placeholder names the session" \
    bash -c "tmux -L proof capture-pane -t '=own-tui:' -p | cut -c $((PROOF_COLS / 2))- | grep -A3 'This is the tmux session ainb is running in' | grep -q 'own-tui'"
  check "the placeholder says why there is no preview" \
    wait_screen own-tui 'A live preview would show this screen inside itself' 5
  capture own-tui own-session-selected
  sleep 2
  observe "clients with own-tui selected: own-tui [$(clients_on own-tui | paste -sd';' -)], neighbour [$(clients_on neighbour | paste -sd';' -)]"
  check "no preview client is opened on the TUI's own session" test -z "$(clients_on own-tui)"
  check "the neighbour's preview client is released" test -z "$(clients_on neighbour)"
  check "the TUI survives selecting its own session (same pid $pid)" kill -0 "$pid"
}
