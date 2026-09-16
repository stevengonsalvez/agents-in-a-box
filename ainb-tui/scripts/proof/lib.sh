#!/usr/bin/env bash
# Shared helpers for the proof harness (run.sh sources this, then one scenario).
#
# Every scenario runs in its own world, created fresh and destroyed at the end:
#
#   $PROOF_WORLD/
#     home/      HOME            (~/.agents-in-a-box lives here, never the real one; it is
#                                 also AINB_HANGAR_HOME, the private daemon's home)
#     tmux/      TMUX_TMPDIR     (two private tmux servers, see below)
#     bin/       first on PATH   (the `claude` fixture agent and the `headroom` stub)
#     repo/      the git repository fixture sessions are spawned from
#
# Two tmux servers, both under $TMUX_TMPDIR, never the box's own:
#
#   -L proof   hosts the TUI panes this harness types into and captures
#   default    the one `ainb run` and the TUI itself use for agent sessions
#
# The TUI runs with TMUX unset. Inside a `-L proof` pane it would otherwise
# inherit that server as "its" tmux and list the harness panes instead of the
# fixture sessions.
#
# Nothing here is killed by pattern. Sessions are killed by exact name, and a
# process is only signalled when its environment carries this world's
# AINB_HANGAR_HOME, which no process outside the world can have.

# shellcheck disable=SC2034  # several globals are read by the scenarios

PROOF_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AINB_TUI_DIR="$(cd "$PROOF_LIB_DIR/../.." && pwd)"
AINB_BIN="${AINB_BIN:-${CARGO_TARGET_DIR:-$AINB_TUI_DIR/target}/debug/ainb}"
: "${PROOF_OUT:?run.sh exports PROOF_OUT}"
PROOF_COLS=160
PROOF_ROWS=48

# Scenario state, reset by world_up.
EXPECT=""
NODE=""
NODE_DIR=""
CAPTURES=()
OBSERVED=()
FAILED_CHECKS=()
KNOWN_ISSUE=""
SKIPPED=""
PROOF_STARTED_AT=""

# ---------------------------------------------------------------------------
# Logging and checks
# ---------------------------------------------------------------------------

say() { printf '  [%s] %s\n' "$NODE" "$*" >&2; }

# observe <text>: one line of what the binary actually did.
observe() {
  OBSERVED+=("$(redact_host <<<"$1")")
  say "observed: $1"
}

# check <description> <command...>: run the command; record pass or fail.
check() {
  local what="$1"; shift
  if "$@"; then
    observe "ok: $what"
  else
    observe "FAILED: $what"
    FAILED_CHECKS+=("$what")
  fi
}

# known_issue <number>: this node's failure is a filed product defect.
known_issue() { KNOWN_ISSUE="$1"; }

# skip <reason>: this node cannot run on this machine.
#
# Recorded as skipped rather than failed: a box with no headless X server, say,
# has not falsified anything, so the reason travels with the result and the run
# does not count it against `pass`. A scenario that skips returns straight
# after; anything it checks after this is a check it could not have made.
skip() {
  SKIPPED="$1"
  observe "SKIPPED: $1"
}

# The internal hostname is not something to publish in a PR comment.
PROOF_HOST_FQDN="$(hostname -f 2>/dev/null || hostname)"
PROOF_HOST_SHORT="$(hostname -s 2>/dev/null || hostname)"
redact_host() {
  sed -e "s/${PROOF_HOST_FQDN//./\\.}/<host>/g" -e "s/${PROOF_HOST_SHORT//./\\.}/<host>/g"
}

# write_result: proof-out/<node>/result.json from what the scenario recorded.
# A scenario that recorded no check at all has proven nothing and fails; one
# that skipped says why and is not counted as a failure.
write_result() {
  local pass=true
  if ((${#FAILED_CHECKS[@]} > 0 || ${#OBSERVED[@]} == 0)); then pass=false; fi
  jq -n \
    --arg node "$NODE" \
    --arg expected "$EXPECT" \
    --argjson pass "$pass" \
    --arg issue "$KNOWN_ISSUE" \
    --arg skipped "$SKIPPED" \
    --arg binary "$BINARY_LINE" \
    --arg started_at "$PROOF_STARTED_AT" \
    --args '{
      node: $node,
      expected: $expected,
      observed: $ARGS.positional[0] | fromjson,
      pass: $pass,
      skipped: (if $skipped == "" then null else $skipped end),
      issue: (if $pass or $issue == "" then null else ($issue | tonumber) end),
      capture: $ARGS.positional[1] | fromjson,
      binary: $binary,
      started_at: $started_at
    }' \
    "$(printf '%s\n' "${OBSERVED[@]}" | jq -R . | jq -sc .)" \
    "$(printf '%s\n' "${CAPTURES[@]}" | grep -v '^$' | sort -u | jq -R . | jq -sc .)" \
    >"$NODE_DIR/result.json"
}

# ---------------------------------------------------------------------------
# World lifecycle
# ---------------------------------------------------------------------------

world_up() {
  NODE="$1"
  NODE_DIR="$PROOF_OUT/$NODE"
  rm -rf "$NODE_DIR"
  mkdir -p "$NODE_DIR"
  CAPTURES=(); OBSERVED=(); FAILED_CHECKS=(); KNOWN_ISSUE=""; SKIPPED=""
  PROOF_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  # Without a world every path below would land under / (HOME=/home), so a
  # failed mktemp ends the node before anything is exported. run.sh sets
  # PROOF_TMP_ROOT to a directory of its own so concurrent runs stay apart.
  PROOF_WORLD="$(mktemp -d "${PROOF_TMP_ROOT:-${TMPDIR:-/tmp}}/ainb-proof-$NODE.XXXXXX")" || exit 1
  : "${PROOF_WORLD:?mktemp gave no world directory}"
  export HOME="$PROOF_WORLD/home"
  # The daemon tails $AINB_HANGAR_HOME/events.jsonl while the hook appends to
  # ~/.agents-in-a-box/events.jsonl, so the two must be one directory or no
  # hook-raised card ever reaches the daemon.
  export AINB_HANGAR_HOME="$HOME/.agents-in-a-box"
  export TMUX_TMPDIR="$PROOF_WORLD/tmux"
  unset TMUX TMUX_PANE
  mkdir -p "$HOME/.agents-in-a-box/config" "$TMUX_TMPDIR" "$PROOF_WORLD/bin"
  PATH="$PROOF_WORLD/bin:${AINB_BIN%/*}:$PROOF_BASE_PATH"
  export PATH
  export AINB_PLUGIN_ROOT="$AINB_TUI_DIR/dist/plugins"

  # `ainb init` records onboarding, so the setup wizard never opens.
  "$AINB_BIN" init --format json </dev/null >"$PROOF_WORLD/init.json" 2>&1
  # The first-run hooks prompt on the session list eats keys until Esc.
  printf '{"agents":[],"hook_script":"","prompt_dismissed":true}\n' >"$AINB_HANGAR_HOME/install.json"

  install_fixture_agent
  install_headroom_stub
}

# world_pids: every live process that belongs to this world.
world_pids() {
  local pid
  for pid in $(pgrep -u "$(id -u)"); do
    [[ "$pid" == "$$" || "$pid" == "$BASHPID" ]] && continue
    if { tr '\0' '\n' <"/proc/$pid/environ"; } 2>/dev/null | grep -qxF "AINB_HANGAR_HOME=$AINB_HANGAR_HOME"; then
      printf '%s\n' "$pid"
    fi
  done
}

world_down() {
  local name pid
  # Surface logs travel with the result; the world they lived in does not.
  if [[ -s "$PROOF_WORLD/web.log" ]]; then
    redact_host <"$PROOF_WORLD/web.log" >"$NODE_DIR/web-log.txt"
  fi
  local daemon_log
  daemon_log="$(find "$AINB_HANGAR_HOME/hangar/logs" -type f -name 'daemon.*' 2>/dev/null | sort | tail -1)"
  if [[ -n "$daemon_log" ]]; then
    tail -n 200 "$daemon_log" | redact_host >"$NODE_DIR/daemon-log-tail.txt"
  fi
  local stderr_file
  for stderr_file in "$PROOF_WORLD"/*.stderr; do
    if [[ -s "$stderr_file" ]]; then
      redact_host <"$stderr_file" >"$NODE_DIR/${stderr_file##*/}.txt"
    fi
  done
  # Quit TUIs the way an operator does before anything is signalled.
  for name in $(ptmux list-sessions -F '#{session_name}' 2>/dev/null); do
    ptmux send-keys -t "=$name:" C-c 2>/dev/null || true
  done
  sleep 2
  for name in $(ptmux list-sessions -F '#{session_name}' 2>/dev/null); do
    ptmux kill-session -t "=$name" 2>/dev/null || true
  done
  for name in $(ftmux list-sessions -F '#{session_name}' 2>/dev/null); do
    ftmux kill-session -t "=$name" 2>/dev/null || true
  done
  "$AINB_BIN" hangar daemon stop >/dev/null 2>&1 || true
  for pid in $(world_pids); do kill "$pid" 2>/dev/null || true; done
  sleep 1
  for pid in $(world_pids); do kill -9 "$pid" 2>/dev/null || true; done
  # Captured, not piped: a `| wc` would itself carry this world's environment.
  local left
  left="$(world_pids)"
  if [[ -n "$left" ]]; then
    say "WARNING: process(es) survived teardown: $(tr '\n' ' ' <<<"$left")"
  fi
  # git worktrees point back into the fixture repo; both live in the world.
  rm -rf "$PROOF_WORLD"
}

# ---------------------------------------------------------------------------
# tmux: the harness server and the fixture server
# ---------------------------------------------------------------------------

ptmux() { tmux -L proof "$@"; }
ftmux() { tmux "$@"; }

# pane_text <session> [-e]: the visible screen of a harness pane.
pane_text() { ptmux capture-pane -t "=$1:" -p "${@:2}" 2>/dev/null; }

# capture <session> <name>: save the pane as <name>.txt and <name>.ans.
capture() {
  local session="$1" name="$2"
  pane_text "$session" | redact_host >"$NODE_DIR/$name.txt"
  pane_text "$session" -e | redact_host >"$NODE_DIR/$name.ans"
  CAPTURES+=("$name.txt" "$name.ans")
}

# save_output <name> <command...>: run a CLI command and keep its output.
save_output() {
  local name="$1"; shift
  { "$@" 2>&1 || true; } | redact_host >"$NODE_DIR/$name.txt"
  CAPTURES+=("$name.txt")
}

# keys <session> <key...>: tmux key names, one send per key.
keys() {
  local session="$1" key; shift
  for key in "$@"; do
    ptmux send-keys -t "=$session:" "$key"
    sleep 0.3
  done
}

# type_text <session> <text>: literal characters.
type_text() { ptmux send-keys -t "=$1:" -l "$2"; sleep 0.3; }

# wait_screen <session> <regex> [timeout_s]: poll the pane until it matches.
wait_screen() {
  local session="$1" pattern="$2" deadline=$((SECONDS + ${3:-20}))
  while (( SECONDS < deadline )); do
    pane_text "$session" | grep -qE -- "$pattern" && return 0
    sleep 0.5
  done
  return 1
}

# wait_gone <session> <regex> [timeout_s]
wait_gone() {
  local session="$1" pattern="$2" deadline=$((SECONDS + ${3:-20}))
  while (( SECONDS < deadline )); do
    pane_text "$session" | grep -qE -- "$pattern" || return 0
    sleep 0.5
  done
  return 1
}

# wait_for <timeout_s> <command...>: poll a command until it succeeds.
wait_for() {
  local deadline=$((SECONDS + $1)); shift
  while (( SECONDS < deadline )); do
    "$@" && return 0
    sleep 0.5
  done
  return 1
}

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

# The "agent": a bash loop named `claude`. It prints a numbered tick every
# second and reports SIGINT instead of dying, so a ctrl+c that reaches it is
# visible in its own pane.
install_fixture_agent() {
  cat >"$PROOF_WORLD/bin/claude" <<'AGENT'
#!/usr/bin/env bash
trap 'echo "AGENT GOT SIGINT at $(date +%T)"' INT
n=0
while :; do
  n=$((n + 1))
  echo "agent tick $n $(date +%T)"
  sleep 1
done
AGENT
  chmod +x "$PROOF_WORLD/bin/claude"
}

# A stand-in `headroom`: `headroom proxy --port N` serves GET /health with 200
# on 127.0.0.1:N and logs each request to $PROOF_WORLD/headroom-requests.log.
install_headroom_stub() {
  PROOF_HEADROOM_PORT="$(free_port)"
  export AINB_HEADROOM_PORT="$PROOF_HEADROOM_PORT"
  cat >"$PROOF_WORLD/bin/headroom" <<STUB
#!/usr/bin/env bash
port=8787
while ((\$#)); do
  case "\$1" in --port) port="\$2"; shift ;; esac
  shift
done
exec python3 - "\$port" <<'PY'
import http.server, sys, json
class Health(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"status": "ok", "tokens_saved": 0}).encode()
        self.send_response(200 if self.path.startswith(("/health", "/stats")) else 404)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, fmt, *args):
        with open("$PROOF_WORLD/headroom-requests.log", "a") as log:
            log.write(fmt % args + "\\n")
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Health).serve_forever()
PY
STUB
  chmod +x "$PROOF_WORLD/bin/headroom"
}

# free_port: an unused loopback TCP port.
free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

# fixture_session [name]: a git repo plus one `ainb run --worktree` session
# whose agent is the fixture loop. Sets FIXTURE_ID, FIXTURE_TMUX, FIXTURE_CWD.
fixture_session() {
  local repo="$PROOF_WORLD/repo"
  if [[ ! -d "$repo/.git" ]]; then
    git init -q -b main "$repo"
    git -C "$repo" -c user.email=proof@localhost -c user.name=proof commit -q --allow-empty -m init
  fi
  local out
  out="$(cd "$repo" && "$AINB_BIN" run --repo "$repo" --worktree --format json </dev/null 2>&1)"
  FIXTURE_ID="$(printf '%s\n' "$out" | sed -n 's/^ *Session ID: *//p' | head -1)"
  FIXTURE_TMUX="$(printf '%s\n' "$out" | sed -n 's/^ *Tmux Session: *//p' | head -1)"
  FIXTURE_CWD="$(printf '%s\n' "$out" | sed -n 's/^ *Working Dir: *//p' | head -1)"
  if [[ -z "$FIXTURE_TMUX" ]]; then
    say "ainb run did not create a session: $out"
    return 1
  fi
  wait_for 15 fixture_says "agent tick"
}

# fixture_text: the fixture agent's own pane, on the fixture tmux server.
fixture_text() { ftmux capture-pane -t "=$FIXTURE_TMUX:" -p 2>/dev/null; }
fixture_says() { fixture_text | grep -qE -- "$1"; }

# start_tui <session>: the TUI in a harness pane, waited on until home renders.
start_tui() {
  local session="$1"
  ptmux new-session -d -s "$session" -x "$PROOF_COLS" -y "$PROOF_ROWS" \
    "env -u TMUX -u TMUX_PANE '$AINB_BIN' 2>>'$PROOF_WORLD/$session.stderr'"
  if ! wait_screen "$session" 'Stats +\[i\]' 45; then
    say "the TUI in $session never reached the home screen"
    return 1
  fi
  # The home screen drops the first key pressed right after its first render
  # (#1029); give it a beat so every scenario's first key lands.
  sleep 1.5
  # The TUI autostarts its private daemon; a scenario that reads the daemon
  # before it is up would blame the wrong thing, so wait for it here.
  if ! wait_for 30 daemon_running; then
    say "the TUI in $session did not bring its daemon up within 30 s"
    observe "the daemon the TUI autostarts was not running 30 s after the home screen: $("$AINB_BIN" hangar daemon status 2>&1 | head -1)"
  fi
}

# daemon_running: the world's private daemon answers on its socket.
daemon_running() { "$AINB_BIN" hangar daemon status 2>/dev/null | grep -q 'daemon: running'; }

# tui_pid <session>: pid of the ainb process in a harness pane.
tui_pid() { ptmux display-message -p -t "=$1:" '#{pane_pid}' 2>/dev/null; }

# quit_tui <session>: ctrl+c quits from home; wait until the process is gone.
quit_tui() {
  local session="$1" pid
  pid="$(tui_pid "$session")"
  keys "$session" Escape Escape C-c
  wait_for 15 bash -c "! kill -0 $pid 2>/dev/null"
}

# open_hangar_screen <session> <palette word> <marker regex>: `g` for the
# Hangar, then ctrl+p <word> enter, until <marker> is on screen. The palette
# takes a moment to own the keyboard; a word typed before then lands as tab
# hotkeys, so each attempt is verified and retried from the Issues tab.
open_hangar_screen() {
  local session="$1" word="$2" marker="$3" attempt i
  for attempt in 1 2 3 4; do
    # Every attempt starts from the TUI home screen, so the palette always
    # opens over the Hangar's first screen rather than whatever is left over.
    for i in 1 2 3 4 5; do
      pane_text "$session" | grep -qE 'Stats +\[i\]' && break
      keys "$session" Escape
      sleep 0.7
      pane_text "$session" | grep -qE 'Stats +\[i\]' && break
      keys "$session" q
      sleep 0.7
    done
    keys "$session" g
    wait_screen "$session" '\[1\]Issues|danger-full-access' 30 || continue
    if pane_text "$session" | grep -q 'danger-full-access'; then keys "$session" y; fi
    wait_screen "$session" '\[1\]Issues' 20 || continue
    sleep 2
    keys "$session" C-p
    sleep 1
    type_text "$session" "$word"
    sleep 0.5
    keys "$session" Enter
    if wait_screen "$session" "$marker" 10; then
      # The hooks install prompt covers the first plugin screen until dismissed.
      if pane_text "$session" | grep -q 'Get notified when a session needs you'; then
        keys "$session" Escape
      fi
      return 0
    fi
    say "palette attempt $attempt did not reach $word; retrying from home"
    capture "$session" "palette-miss-$word-$attempt"
  done
  return 1
}

# raise_ask <question> [session-id]: an AskUserQuestion card through the real
# hook path, for the fixture session's cwd and tmux pane.
raise_ask() {
  local question="$1" sid="${2:-proof-$NODE}" pane
  pane="$(ftmux display-message -p -t "=$FIXTURE_TMUX:" '#{pane_id}')"
  jq -n --arg sid "$sid" --arg cwd "$FIXTURE_CWD" --arg q "$question" '{
    session_id: $sid, cwd: $cwd, hook_event_name: "PreToolUse",
    tool_name: "AskUserQuestion", tool_use_id: ("toolu_" + $sid),
    tool_input: {questions: [{question: $q, header: "proof", multiSelect: false,
      options: [{label: "alpha", description: "first"}, {label: "beta", description: "second"}]}]}
  }' | (cd "$FIXTURE_CWD" && TMUX_PANE="$pane" "$AINB_BIN" fleet atc hook \
      --event PreToolUse --matcher AskUserQuestion --session-id "$sid" --cwd "$FIXTURE_CWD")
}

# start_web: `ainb web` on a free port in a harness pane. Sets WEB_URL.
start_web() {
  local port
  port="$(free_port)"
  WEB_URL="http://127.0.0.1:$port"
  ptmux new-session -d -s web -x "$PROOF_COLS" -y 20 \
    "env -u TMUX -u TMUX_PANE '$AINB_BIN' web --listen 127.0.0.1:$port 2>&1 | tee '$PROOF_WORLD/web.log'"
  # Readiness on the static page, not /api/snapshot: a cold snapshot request
  # races the poller's first tick, and two concurrent `ainb fleet cost` runs
  # each take 120 s (#1055), which freezes the snapshot for that long.
  wait_for 30 curl -fsS -o /dev/null "$WEB_URL/" 2>/dev/null
}

# web_attention_id <question regex>: the attentionId the web snapshot lists.
web_attention_id() {
  curl -fsS "$WEB_URL/api/snapshot" \
    | jq -r --arg q "$1" '.needs[]? | select((.payload | tostring) | test($q)) | .attentionId' | head -1
}

# web_answer <attention id> <answer>: the exact body frontend/app.js posts.
web_answer() {
  curl -sS -X POST "$WEB_URL/api/answer" -H 'content-type: application/json' \
    -d "$(jq -nc --arg id "$1" --arg a "$2" '{attentionId: $id, answer: $a}')"
}

# web_card_id <question regex> <capture name> <timeout_s>: wait until the web
# snapshot lists a card whose payload matches, and set WEB_CARD_ID to its
# attentionId. Returns 1 on timeout with WEB_CARD_ID empty, so a caller stops
# before posting an answer with no id. Called directly, not in $(...): its
# observed lines and captures must reach the result. The web's poller can sit
# behind `ainb fleet cost` runs (#1055); past 15 s it records which cost runs
# the world has, and the observed line keeps the measured lag visible.
WEB_CARD_ID=""
web_card_id() {
  local question="$1" name="$2" timeout="${3:?web_card_id needs a timeout in seconds}"
  local start=$SECONDS noted=0 p cmd
  WEB_CARD_ID=""
  while (( SECONDS - start < timeout )); do
    WEB_CARD_ID="$(web_attention_id "$question")"
    [[ -n "$WEB_CARD_ID" ]] && break
    if (( !noted && SECONDS - start >= 15 )); then
      noted=1
      for p in $(world_pids); do
        cmd="$( { tr '\0' ' ' <"/proc/$p/cmdline"; } 2>/dev/null)"
        if grep -F 'fleet cost' <<<"$cmd" >/dev/null; then printf '%s %s\n' "$p" "$cmd"; fi
      done >"$NODE_DIR/$name-cost-runs.txt"
      CAPTURES+=("$name-cost-runs.txt")
      observe "web had no card after 15 s; ainb fleet cost runs in this world: $(wc -l <"$NODE_DIR/$name-cost-runs.txt")"
    fi
    sleep 1
  done
  if [[ -z "$WEB_CARD_ID" ]]; then
    curl -sS "$WEB_URL/api/snapshot" 2>/dev/null | redact_host >"$NODE_DIR/$name-snapshot-at-timeout.json"
    CAPTURES+=("$name-snapshot-at-timeout.json")
    observe "web snapshot listed no card for '$question' within ${timeout}s (#1055 web lag)"
    return 1
  fi
  observe "web snapshot card for '$question' after $((SECONDS - start))s (#1055 web lag): $WEB_CARD_ID"
}

# connections_json: the daemon's connection registry as JSON.
connections_json() { "$AINB_BIN" hangar connections list --format json 2>/dev/null; }

# count_kind <kind> [pid]: rows of that surface kind (at that pid).
count_kind() {
  connections_json | jq --arg k "$1" --arg p "${2:-}" \
    '[.connections[].surface | select(.kind == $k and ($p == "" or (.pid | tostring) == $p))] | length'
}

# rows_is <kind> <pid or ""> <op> <n>: `test`-style check on count_kind, so it
# can be handed to check and wait_for directly.
rows_is() { test "$(count_kind "$1" "$2")" "-$3" "$4"; }

# rpc_call <min> <max> [method] [params-json]: a raw auth/hello on the private
# daemon's socket, framed as the wire contract frames it (Content-Length header,
# JSON-RPC body), then optionally one call. Prints the hello reply (capability
# list reduced to a count) and, when a method is given, that call's reply.
rpc_call() {
  python3 - "$AINB_HANGAR_HOME" "$@" <<'PY'
import json, socket, sys
home, lo, hi = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
method = sys.argv[4] if len(sys.argv) > 4 else None
params = json.loads(sys.argv[5]) if len(sys.argv) > 5 else {}
token = open(f"{home}/hangar/daemon.token").read().strip()
sock = socket.socket(socket.AF_UNIX)
sock.settimeout(10)
sock.connect(f"{home}/hangar.sock")
stream = sock.makefile("rb")

def send(req_id, name, body):
    raw = json.dumps({"jsonrpc": "2.0", "id": req_id, "method": name, "params": body}).encode()
    sock.sendall(b"Content-Length: %d\r\n\r\n" % len(raw) + raw)

def receive(req_id):
    while True:
        length = None
        while True:
            line = stream.readline()
            if not line:
                return {"closed": True}
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        frame = json.loads(stream.read(length))
        if frame.get("id") == req_id:
            return frame

send(1, "auth/hello", {"token": token, "protocol": {"min": lo, "max": hi}, "capabilities": []})
hello = receive(1)
result = hello.get("result") or {}
if isinstance(result.get("capabilities"), list):
    result["capabilities"] = f"{len(result['capabilities'])} strings"
print(json.dumps(hello))
if method and "result" in hello:
    send(2, method, params)
    print(json.dumps(receive(2)))
PY
}

# hello_probe <min> <max>: the hello reply alone.
hello_probe() { rpc_call "$1" "$2"; }

# fleet_status_rows: the daemon's own fleet/status rows, one JSON object each.
fleet_status_rows() { rpc_call 1 1 fleet/status '{}' | tail -1 | jq -c '.result.rows[]?'; }

# panel_lens_count <session> <label>: the count after a Fleet panel lens label
# such as "needs input", "idle", "running" or "all".
panel_lens_count() {
  pane_text "$1" | grep -oE "[0-9] $2 [0-9]+" | head -1 | awk '{print $NF}'
}

# panel_status_line <session>: the Fleet detail line `state · provenance · tier N · age`.
panel_status_line() {
  pane_text "$1" | grep -oE '(waiting|working|idle|exited|unverifiable) · [a-z]+ · tier [0-9]+ · [^ │]+' | head -1
}

# ask_session <question> <session id>: a fresh fixture session holding one
# hook-raised ASK. Asks on one pane collapse into one card, so each gets its own.
ask_session() {
  fixture_session || return 1
  raise_ask "$1" "$2" >/dev/null
}

# web_sync_sessions [timeout_s]: wait until the web snapshot lists as many
# sessions as `ainb list`. Prints the seconds it took; fails on timeout.
web_sync_sessions() {
  local deadline=$((SECONDS + ${1:-180})) start=$SECONDS want got
  want="$("$AINB_BIN" list --format json 2>/dev/null | jq 'length')"
  while (( SECONDS < deadline )); do
    got="$(curl -sS "$WEB_URL/api/snapshot" 2>/dev/null | jq '.sessions | length' 2>/dev/null)"
    if [[ -n "$got" && "$got" -ge "$want" ]]; then
      printf '%s' "$((SECONDS - start))"
      return 0
    fi
    sleep 2
  done
  printf '%s' "$((SECONDS - start))"
  return 1
}

# click <session> <col> <row>: one left click, as SGR mouse bytes on the pane.
click() {
  ptmux send-keys -t "=$1:" -l "$(printf '\033[<0;%d;%dM\033[<0;%d;%dm' "$2" "$3" "$2" "$3")"
  sleep 0.4
}

# double_click <session> <col> <row>: two clicks well inside 300 ms.
double_click() {
  ptmux send-keys -t "=$1:" -l "$(printf '\033[<0;%d;%dM\033[<0;%d;%dm\033[<0;%d;%dM\033[<0;%d;%dm' \
    "$2" "$3" "$2" "$3" "$2" "$3" "$2" "$3")"
  sleep 0.4
}

# row_of <session> <regex>: the 1-based screen row of the first matching line.
row_of() { pane_text "$1" | grep -nE -- "$2" | head -1 | cut -d: -f1; }

# open_session_list <session>: `s` from home, waited on until the fixture's
# preview is live.
open_session_list() {
  keys "$1" s
  wait_screen "$1" 'agent tick' 20
}
