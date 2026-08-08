#!/usr/bin/env bash
# ABOUTME: Live end-to-end smoke for the daemon chat bus (tmux leg + ACP leg).
#
# The tmux-verify discipline applied to the chat bus: REAL processes only, one
# scratch world per run, and every journey asserting the EXACT user-visible
# outcome rather than "something appeared".
#
#   ./scripts/chat-bus-smoke.sh            # every journey (CI / release smoke)
#   ./scripts/chat-bus-smoke.sh j2         # one journey, for recording
#   ./scripts/chat-bus-smoke.sh j1 j5c     # a subset
#   ./scripts/chat-bus-smoke.sh --keep j4  # leave the scratch world behind
#
# What it stands up (nothing touches the operator's real hangar or tmux):
#   * a scratch $AINB_HANGAR_HOME and $HOME under one mktemp root
#   * the REAL `ainb-hangar-daemon`, built from this tree, running against it
#   * the REAL `ainb` binary for every CLI verb under test
#   * a PRIVATE tmux server (TMUX_TMPDIR, $TMUX removed) holding 3 sessions
#     that run the fake-agent harness pattern from
#     `crates/ainb-core/tests/tripwire_cli_run_prompt.rs` (a shell that echoes
#     RECEIVED:<line> on submit, so the assertion proves verbatim delivery AND
#     submission, not just "text reached the pane")
#   * ACP adapters: the real ones when they are installed AND credentialled,
#     otherwise the `fake_acp_adapter` fixture from `ainb-acp`. The mode is
#     printed in the banner, never guessed at by the reader.
#
# How the tmux sessions become fleet sessions: they are NOT seeded into the
# store. The daemon's own tmux reconciler (`spawn_tmux_reconciler`, every 3 s)
# discovers any pane whose process tree holds an agent named `claude` or
# `codex` and registers it as a degraded-but-sendable fleet row. That is the
# real path a user's session takes, so the fake agent is a copy of /bin/sh
# NAMED `claude` (the reconciler reads `ps -o comm=`, not the pane title).
#
# Exit code is 0 only when every selected journey PASSed or SKIPped; any FAIL
# exits 1. Each journey prints one machine-readable summary line:
#   SMOKE-RESULT <journey> <PASS|FAIL|SKIP> <reason>
#
# Knobs (all optional):
#   AINB_SMOKE_SKIP_BUILD=1        reuse an existing build instead of running cargo
#   AINB_SMOKE_BIN_SOURCE=<dir>    take the three binaries from <dir>, not target/debug
#                                  (needed when another checkout shares CARGO_TARGET_DIR)
#   AINB_SMOKE_TURN_DEADLINE_MS=N  the daemon's per-turn deadline for this run
#   AINB_SMOKE_JOBS=N              cargo -j for the build step (default 2)
#   AINB_SMOKE_FORCE_FIXTURE=1     use the fixture adapter even where a real one is usable
set -Eeuo pipefail

# --------------------------------------------------------------------- layout

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TUI_DIR="$(dirname -- "$SCRIPT_DIR")"
TARGET_DIR="${CARGO_TARGET_DIR:-$TUI_DIR/target}"
PROFILE_DIR="$TARGET_DIR/debug"

RUN_ID="$$-$(date +%s)"
ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ainb-chat-bus-smoke.XXXXXX")"
BIN_DIR="$ROOT/bin"
LOG_DIR="$ROOT/logs"
SCRATCH_HOME="$ROOT/home"
HANGAR_HOME="$ROOT/hangar-home"
TMUX_DIR="$ROOT/tmux"
DB="$HANGAR_HOME/hangar.db"

# The turn deadline the daemon runs with. 30 minutes (the production default)
# cannot be observed by a smoke run, so `PoolConfig::from_env` reads this.
#
# It is a two-sided constraint, which is why it is not smaller: J5b waits it
# out, so shorter is faster, but J5a has to fill a 32-deep queue with real CLI
# invocations BEFORE the same deadline unwedges the turn it is queueing behind.
TURN_DEADLINE_MS="${AINB_SMOKE_TURN_DEADLINE_MS:-45000}"

KEEP_ROOT=0
DAEMON_PID=""
BG_PIDS=()
TMUX_SESSIONS=()
declare -a RESULT_LINES=()
FAILED=0
SKIP_REASON=""
FAIL_REASON=""

# ---------------------------------------------------------------- diagnostics

c_bold=$'\033[1m'; c_red=$'\033[31m'; c_green=$'\033[32m'; c_yellow=$'\033[33m'; c_off=$'\033[0m'
[[ -t 1 ]] || { c_bold=""; c_red=""; c_green=""; c_yellow=""; c_off=""; }

log()    { printf '%s\n' "$*"; }
banner() { printf '\n%s┌─ %s\n└─ %s%s\n' "$c_bold" "$1" "$2" "$c_off"; }
step()   { printf '   · %s\n' "$*"; }

# Record why a journey failed and return non-zero. Every assertion routes here,
# so a failure always names the EXPECTED and the OBSERVED value.
fail() { FAIL_REASON="$*"; printf '   %sx %s%s\n' "$c_red" "$*" "$c_off" >&2; return 1; }
skip() { SKIP_REASON="$*"; return 77; }

assert_eq() { # expected actual message
  [[ "$1" == "$2" ]] || fail "$3: expected [$1], got [$2]"
}
assert_contains() { # haystack needle message
  case "$1" in *"$2"*) return 0 ;; esac
  fail "$3: [$2] not found in [$(printf '%s' "$1" | head -c 400)]"
}

# Everything a human needs to debug a red journey, printed only when one goes
# red: the panes as they actually look, and the daemon's own log tail.
dump_diagnostics() {
  printf '\n%s--- diagnostics ---%s\n' "$c_yellow" "$c_off"
  for session in "${TMUX_SESSIONS[@]:-}"; do
    [[ -n "$session" ]] || continue
    printf '%s[pane %s]%s\n' "$c_yellow" "$session" "$c_off"
    tmux_cmd capture-pane -p -t "$session" 2>&1 | tail -25 || true
  done
  printf '%s[daemon stdout/stderr tail]%s\n' "$c_yellow" "$c_off"
  tail -40 "$LOG_DIR/daemon.out" 2>/dev/null || true
  local structured
  structured="$(ls -1t "$HANGAR_HOME"/hangar/logs/daemon.* 2>/dev/null | head -1 || true)"
  if [[ -n "$structured" ]]; then
    printf '%s[%s tail]%s\n' "$c_yellow" "$structured" "$c_off"
    tail -40 "$structured" || true
  fi
  if [[ -f "$LOG_DIR/acp-rpc.log" ]]; then
    printf '%s[adapter rpc log]%s\n' "$c_yellow" "$c_off"
    tail -30 "$LOG_DIR/acp-rpc.log" || true
  fi
  printf '%s--- end diagnostics (scratch root: %s) ---%s\n' "$c_yellow" "$ROOT" "$c_off"
}

# ------------------------------------------------------------------- teardown

cleanup() {
  local status=$?
  set +e
  for pid in "${BG_PIDS[@]:-}"; do [[ -n "$pid" ]] && kill "$pid" 2>/dev/null; done
  stop_daemon
  kill_fixture_adapters
  # EXACT session names only. Never `kill-server`: this box runs other agents'
  # tmux servers and a stray wildcard kill would take them with it.
  for session in "${TMUX_SESSIONS[@]:-}"; do
    [[ -n "$session" ]] && tmux_cmd kill-session -t "$session" 2>/dev/null
  done
  if [[ "$KEEP_ROOT" == 1 ]]; then
    log "scratch world kept at $ROOT"
  else
    rm -rf "$ROOT"
  fi
  exit "$status"
}
trap cleanup EXIT

# ------------------------------------------------------------------ utilities

tmux_cmd() { env -u TMUX TMUX_TMPDIR="$TMUX_DIR" tmux "$@"; }

# The daemon's environment IS the contract under test: hangar home, private
# tmux server, a PATH whose adapter tokens resolve to this run's binaries, and
# the compressed turn deadline.
#
# An ARRAY, not a wrapper function: `daemon_env … &` would background a
# FUNCTION, so `$!` would be bash's subshell and `kill -9 "$DAEMON_PID"` (J3)
# would leave the real daemon running while a second one booted onto the same
# database. `env` execs its argument, so this way `$!` IS the daemon.
DAEMON_ENV=(
  env -u TMUX
  HOME="$SCRATCH_HOME"
  AINB_HANGAR_HOME="$HANGAR_HOME"
  TMUX_TMPDIR="$TMUX_DIR"
  PATH="$BIN_DIR:$PATH"
  AINB_ACP_TURN_DEADLINE_MS="$TURN_DEADLINE_MS"
  # Nothing here talks to Codex's app-server; left on, the manager retries a
  # missing binary forever and buries the log this smoke asks operators to read.
  AINB_CODEX_MANAGED=0
  RUST_LOG="${RUST_LOG:-info}"
)

ainb_cli() { "${DAEMON_ENV[@]}" "$BIN_DIR/ainb" "$@"; }

# One JSON-RPC round trip on hangar.sock, printing the WHOLE response so the
# caller can read an error code (the Phase 6 probe needs -32601, not a crash).
rpc() { # method [params-json]
  local params="${2:-}"
  [[ -n "$params" ]] || params='{}'
  python3 "$BIN_DIR/rpc.py" "$HANGAR_HOME/hangar.sock" \
    "$(cat "$HANGAR_HOME/hangar/daemon.token")" "$1" "$params"
}

# Receipts (`fleet_message_delivery.detail`) have no wire reader in part 1:
# `fleet/message_list` returns messages, not the delivery join. The plan's
# runbook answers "why did this not deliver" from that column, so the smoke
# reads it directly, READ-ONLY, and says so wherever it does.
db() { sqlite3 -readonly "$DB" "$1"; }

delivery_state()  { db "SELECT state FROM fleet_message_delivery WHERE message_id='$1' AND session_key='$2';"; }
delivery_detail() { db "SELECT COALESCE(detail,'') FROM fleet_message_delivery WHERE message_id='$1' AND session_key='$2';"; }

# Poll `cmd` until it succeeds or `timeout` seconds pass.
wait_until() { # timeout-seconds description cmd...
  local timeout="$1" what="$2"; shift 2
  local deadline=$(( $(date +%s) + timeout ))
  until "$@"; do
    if (( $(date +%s) >= deadline )); then
      fail "timed out after ${timeout}s waiting for: $what"
      return 1
    fi
    sleep 0.25
  done
}

jqr() { jq -r "$1"; }

# ------------------------------------------------------------- scratch world

write_rpc_client() {
  cat >"$BIN_DIR/rpc.py" <<'PY'
"""Minimal hangar.sock JSON-RPC client (Content-Length framing, auth/hello first).

Prints the whole response object, errors included, so callers can branch on
`error.code` instead of on this process's exit status.
"""
import json, socket, sys

sock_path, token, method = sys.argv[1], sys.argv[2], sys.argv[3]
params = json.loads(sys.argv[4]) if len(sys.argv) > 4 else {}


def frame(method, params, ident):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": ident, "method": method, "params": params}
    ).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


def read(stream):
    length = None
    while True:
        line = stream.readline()
        if not line:
            raise SystemExit("daemon closed the connection")
        line = line.strip()
        if not line:
            break
        name, _, value = line.decode().partition(":")
        if name.strip().lower() == "content-length":
            length = int(value.strip())
    if length is None:
        raise SystemExit("no Content-Length in daemon frame")
    return json.loads(stream.read(length))


s = socket.socket(socket.AF_UNIX)
s.settimeout(30)
s.connect(sock_path)
f = s.makefile("rwb")
f.write(frame("auth/hello", {"token": token}, 1))
f.flush()
hello = read(f)
if hello.get("error"):
    print(json.dumps(hello))
    raise SystemExit(3)
f.write(frame(method, params, 2))
f.flush()
# Notifications (no id) can interleave; the reply is the framed object carrying
# our id.
while True:
    response = read(f)
    if response.get("id") is not None:
        break
print(json.dumps(response))
PY
}

# The fake agent, verbatim in spirit from `tripwire_cli_run_prompt.rs`: a shell
# that echoes every SUBMITTED line back as `RECEIVED:<line>`. `read` only
# returns on Enter, so the echo proves submission and not merely typing.
write_fake_agent() {
  cat >"$BIN_DIR/agent.sh" <<'SH'
echo 'fake agent ready. Ctrl+C to exit'
while IFS= read -r line; do
  printf 'RECEIVED:%s\n' "$line"
done
SH
  # NAMED `claude` because the fleet reconciler derives the provider from the
  # pane's process tree via `ps -o comm=` — a `#!/bin/sh` script would report
  # `sh` and the pane would never become a fleet session.
  cp "$(command -v sh)" "$BIN_DIR/claude"
  chmod +x "$BIN_DIR/claude"
}

# One ACP adapter slot. `AdapterConfig` spawns the registry token from PATH and
# hands the child an ALLOWLISTED environment (PATH + HOME only), so fixture
# knobs cannot be exported here: the shim re-exports them itself.
write_adapter_shim() { # token env-lines...
  local token="$1"; shift
  {
    printf '#!/bin/sh\n'
    printf '# Fixture ACP adapter for the chat-bus smoke (NOT a real adapter).\n'
    printf '# The daemon forwards only PATH and HOME to an adapter child, so the\n'
    printf '# fixture knobs are baked in here rather than exported by the caller.\n'
    for line in "$@"; do printf 'export %s\n' "$line"; done
    printf 'exec "%s" "$@"\n' "$BIN_DIR/fake_acp_adapter"
  } >"$BIN_DIR/$token"
  chmod +x "$BIN_DIR/$token"
}

# The turn the fixture plays back. Kinds ALTERNATE on purpose: the reducer
# coalesces contiguous same-kind text and only flushes on a kind boundary (or
# 4 KiB), so a turn made of six identical message chunks commits as ONE row at
# turn end and could never prove the live-streaming half of I12. Alternating
# thought/message forces a commit per boundary, mid-turn.
write_turn_script() {
  cat >"$ROOT/acp-turn.ndjson" <<'NDJSON'
{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"reading the room"}}
{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first part of the answer. "}}
{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"checking one more thing"}}
{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second part of the answer. "}}
{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"almost done"}}
NDJSON
}

kill_fixture_adapters() {
  # Scoped to THIS run's binary path: never a bare `pkill fake_acp_adapter`.
  pkill -f "$BIN_DIR/fake_acp_adapter" 2>/dev/null || true
}

build_binaries() {
  if [[ "${AINB_SMOKE_SKIP_BUILD:-0}" == 1 ]]; then
    step "build skipped (AINB_SMOKE_SKIP_BUILD=1)"
  else
    step "building ainb, ainb-hangar-daemon, fake_acp_adapter"
    ( cd "$TUI_DIR" && CARGO_PROFILE_TEST_STRIP=debuginfo CARGO_INCREMENTAL=0 \
        cargo build -j"${AINB_SMOKE_JOBS:-2}" -p ainb -p ainb-hangar-daemon -p ainb-acp --bins ) \
      >"$LOG_DIR/build.log" 2>&1 || { tail -30 "$LOG_DIR/build.log"; return 1; }
  fi
  # `AINB_SMOKE_BIN_SOURCE` points at an already-built bin dir. It exists for
  # the shared-target-dir case: when another checkout builds into the same
  # `CARGO_TARGET_DIR`, `target/debug/<bin>` is whoever built last, and a smoke
  # run would silently assert against a different tree's daemon.
  local source_dir="${AINB_SMOKE_BIN_SOURCE:-$PROFILE_DIR}"
  local missing=0
  for bin in ainb ainb-hangar-daemon fake_acp_adapter; do
    if [[ ! -x "$source_dir/$bin" ]]; then
      log "missing build artifact: $source_dir/$bin"; missing=1; continue
    fi
    # Hardlink (falling back to a copy): pins the exact inode under test, so a
    # concurrent `cargo build` in the same target dir cannot swap the binary
    # out from under a running journey.
    ln -f "$source_dir/$bin" "$BIN_DIR/$bin" 2>/dev/null || cp "$source_dir/$bin" "$BIN_DIR/$bin"
  done
  return "$missing"
}

# Which adapter answers which registry token. Real adapters win when they are
# installed AND credentialled; the fixture is the fallback, and the choice is
# printed, never implied.
resolve_adapters() {
  ACP_MODE=fixture
  ACP_PROVIDER=claude-agent-acp        # the "normal conversation" slot (J2, J3, J4)
  PERMISSION_PROVIDER=codex-acp        # the "raises a permission per turn" slot (J5d)
  REAL_ADAPTER_NOTE=""

  local have_real=0
  if command -v claude-agent-acp >/dev/null 2>&1; then
    if [[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}${ANTHROPIC_API_KEY:-}" ]]; then
      have_real=1
    else
      REAL_ADAPTER_NOTE="claude-agent-acp is installed but no CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY is set"
    fi
  else
    REAL_ADAPTER_NOTE="no real ACP adapter on PATH"
  fi

  if [[ "$have_real" == 1 && "${AINB_SMOKE_FORCE_FIXTURE:-0}" != 1 ]]; then
    ACP_MODE=real
    REAL_ADAPTER_NOTE="claude-agent-acp $(claude-agent-acp --version 2>/dev/null | head -1)"
    # The real adapter keeps its own name; only the fixture slot is shimmed.
    write_adapter_shim codex-acp \
      "FAKE_ACP_SESSION_PREFIX=smoke-$RUN_ID" \
      "FAKE_ACP_RPC_LOG=$LOG_DIR/acp-rpc.log" \
      "FAKE_ACP_SCRIPT=$ROOT/acp-turn.ndjson" \
      "FAKE_ACP_CHUNK_DELAY_MS=400" \
      "FAKE_ACP_ECHO_PROMPT=1" \
      "FAKE_ACP_HANG_PROMPTS=$HANG_TEXT"
    # Fault journeys need the fixture, so they move to the fixture slot; the
    # permission fixture would need a THIRD registry token, which part 1 does
    # not have.
    FIXTURE_PROVIDER=codex-acp
    PERMISSION_PROVIDER=""
  else
    FIXTURE_PROVIDER=claude-agent-acp
    write_adapter_shim claude-agent-acp \
      "FAKE_ACP_SESSION_PREFIX=smoke-$RUN_ID" \
      "FAKE_ACP_RPC_LOG=$LOG_DIR/acp-rpc.log" \
      "FAKE_ACP_SCRIPT=$ROOT/acp-turn.ndjson" \
      "FAKE_ACP_CHUNK_DELAY_MS=400" \
      "FAKE_ACP_ECHO_PROMPT=1" \
      "FAKE_ACP_HANG_PROMPTS=$HANG_TEXT"
    write_adapter_shim codex-acp \
      "FAKE_ACP_SESSION_PREFIX=smokeperm-$RUN_ID" \
      "FAKE_ACP_RPC_LOG=$LOG_DIR/acp-rpc.log" \
      "FAKE_ACP_CHUNKS=2" \
      "FAKE_ACP_ECHO_PROMPT=1" \
      "FAKE_ACP_PERMISSION_SESSIONS=*"
  fi
}

start_daemon() {
  "${DAEMON_ENV[@]}" "$BIN_DIR/ainb-hangar-daemon" >>"$LOG_DIR/daemon.out" 2>&1 &
  DAEMON_PID=$!
  daemon_ready
}

daemon_ready() {
  local deadline=$(( $(date +%s) + 60 ))
  while (( $(date +%s) < deadline )); do
    if [[ -S "$HANGAR_HOME/hangar.sock" && -f "$HANGAR_HOME/hangar/daemon.token" ]] &&
       rpc fleet/snapshot >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      tail -30 "$LOG_DIR/daemon.out"
      fail "the daemon exited during boot"
      return 1
    fi
    sleep 0.25
  done
  tail -30 "$LOG_DIR/daemon.out"
  fail "the daemon never answered fleet/snapshot"
}

stop_daemon() {
  [[ -n "$DAEMON_PID" ]] || return 0
  kill "$DAEMON_PID" 2>/dev/null || true
  for _ in $(seq 1 40); do
    kill -0 "$DAEMON_PID" 2>/dev/null || { DAEMON_PID=""; return 0; }
    sleep 0.25
  done
  kill -9 "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
}

start_tmux_sessions() {
  for index in 1 2 3; do
    local session="ainb-smoke-$RUN_ID-$index"
    tmux_cmd new-session -d -s "$session" "$BIN_DIR/claude $BIN_DIR/agent.sh"
    TMUX_SESSIONS+=("$session")
  done
}

# The session keys the daemon MINTED for our panes, from the wire snapshot.
# Filtered by our own session-name prefix, so a stray pane on this box could
# never become a target of this run.
resolve_tmux_targets() {
  local snapshot
  snapshot="$(rpc fleet/snapshot)"
  TMUX_KEYS=()
  local key
  while IFS= read -r key; do
    [[ -n "$key" ]] && TMUX_KEYS+=("$key")
  done < <(printf '%s' "$snapshot" |
    jq -r --arg prefix "ainb-smoke-$RUN_ID-" \
      '.result.sessions[] | select(.tmux_target != null and (.tmux_target | startswith($prefix))) | .session_key' |
    sort)
  (( ${#TMUX_KEYS[@]} == 3 ))
}

setup_world() {
  mkdir -p "$BIN_DIR" "$LOG_DIR" "$SCRATCH_HOME" "$HANGAR_HOME/hangar" "$TMUX_DIR"
  # A completed onboarding record so no first-run wizard can intercept a CLI
  # verb (same trap the tmux tripwires document).
  mkdir -p "$SCRATCH_HOME/.agents-in-a-box/config"
  cat >"$SCRATCH_HOME/.agents-in-a-box/config/onboarding.toml" <<'TOML'
completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "smoke"
skipped_dependencies = []
git_directories = []
TOML
  write_rpc_client
  write_fake_agent
  write_turn_script
  build_binaries || return 1
  resolve_adapters
  start_tmux_sessions
  start_daemon || return 1
  # Discovery is a 3 s reconciler tick, so the roster is not instant.
  wait_until 45 "the daemon to discover all 3 tmux sessions" resolve_tmux_targets || return 1
}

# ------------------------------------------------------------------ journeys

# The exact prompt text the fixture adapter never answers (its
# FAKE_ACP_HANG_PROMPTS entry). Journeys that need an open turn send this.
HANG_TEXT="SMOKE_HANG_PROMPT"

new_acp_session() { # provider -> prints "<session_key> <scope_key>"
  ainb_cli --format json fleet acp create --provider "$1" --cwd "$ROOT" |
    jqr '"\(.session_key) \(.scope_key)"'
}

send_msg() { # request-id text target... -> prints the send result JSON
  local request_id="$1" text="$2"; shift 2
  local args=()
  for target in "$@"; do args+=(--target "$target"); done
  ainb_cli --format json fleet msg send "${args[@]}" --text "$text" --request-id "$request_id"
}

# Same, with an EXPLICIT scope. `msg list` pages the whole log oldest-first
# under a 100-row cap, so a journey that runs after a few hundred messages
# cannot find its own row without one.
send_msg_scoped() { # scope request-id text target... -> prints the send result JSON
  local scope="$1" request_id="$2" text="$3"; shift 3
  local args=()
  for target in "$@"; do args+=(--target "$target"); done
  ainb_cli --format json fleet msg send "${args[@]}" --scope "$scope" --text "$text" --request-id "$request_id"
}

pane_hits() { # session text -> count of exact RECEIVED lines
  tmux_cmd capture-pane -p -t "$1" | grep -cxF "RECEIVED:$2" || true
}

# ---- J1: the bus, live on tmux -------------------------------------------

journey_j1() {
  banner "J1 · chat bus on tmux" \
    "one send to 3 real panes: verbatim delivery, 3 DELIVERED receipts, the row in msg list, and a live follower that saw it"

  # The body LEADS with a dash on purpose. Phase 0 exists because a bare
  # send-keys parses a dash-prefixed payload as a tmux flag and corrupts it;
  # asserting the flags structurally elsewhere is not the same as putting the
  # symptom on the path a user actually takes.
  local text="-y [j1 $RUN_ID] hello fleet, deliver me verbatim"
  local request_id="j1-$RUN_ID"
  # An explicit broadcast scope, minted here rather than by the daemon, so the
  # `msg list` assertion reads THIS journey's scope instead of paging a shared
  # log whose head is capped at 100 rows.
  local scope="broadcast:smoke-j1-$RUN_ID"

  step "attaching a background \`fleet msg follow\` BEFORE the send"
  ainb_cli --format json fleet msg follow >"$LOG_DIR/j1-follow.ndjson" 2>"$LOG_DIR/j1-follow.err" &
  local follower=$!
  BG_PIDS+=("$follower")
  wait_until 15 "the follower to ack its head cursor" \
    bash -c "test -s '$LOG_DIR/j1-follow.ndjson'" || return 1

  step "sending to ${#TMUX_KEYS[@]} tmux sessions"
  local result
  result="$(send_msg_scoped "$scope" "$request_id" "$text" "${TMUX_KEYS[@]}")" ||
    { fail "msg send exited non-zero"; return 1; }
  local message_id
  message_id="$(printf '%s' "$result" | jqr '.message_id')"
  [[ -n "$message_id" && "$message_id" != null ]] || { fail "no message_id in send result: $result"; return 1; }

  local states
  states="$(printf '%s' "$result" | jqr '[.deliveries[].state] | join(",")')"
  assert_eq "DELIVERED,DELIVERED,DELIVERED" "$states" "every leg must be DELIVERED" || return 1

  step "asserting the payload reached every pane VERBATIM (capture-pane)"
  local session
  for session in "${TMUX_SESSIONS[@]}"; do
    local hits; hits="$(pane_hits "$session" "$text")"
    assert_eq 1 "$hits" "pane $session must show exactly one RECEIVED:<payload> line" || return 1
  done
  # Echo the pane's own line to stdout: the assertion above is a `grep -cxF`
  # count, and a recording of this run should SHOW the delivered text, not just
  # a tick next to it.
  step "pane echo, verbatim: $(tmux_cmd capture-pane -p -t "${TMUX_SESSIONS[0]}" |
    grep -m1 -xF "RECEIVED:$text" || true)"

  step "asserting the row is in \`msg list\`"
  local listed
  listed="$(ainb_cli --format json fleet msg list --scope "$scope" --limit 20 |
    jq -r --arg id "$message_id" '.messages[] | select(.id == $id) | .body')"
  assert_eq "$text" "$listed" "msg list must carry the message verbatim" || return 1

  step "asserting the background follower observed the event"
  wait_until 20 "the follower to stream the committed message" \
    grep -q "$message_id" "$LOG_DIR/j1-follow.ndjson" || return 1
  local followed
  followed="$(grep -h "$message_id" "$LOG_DIR/j1-follow.ndjson" | head -1 | jqr '.body')"
  assert_eq "$text" "$followed" "the streamed row must carry the same body" || return 1

  kill "$follower" 2>/dev/null || true
  log "   message_id=$message_id · 3/3 DELIVERED · 3/3 panes verbatim · follower saw it"
}

# ---- J2: the ACP leg ------------------------------------------------------

journey_j2() {
  banner "J2 · ACP leg" \
    "acp create + msg send: transcript chunks stream BEFORE turn end, and the timeline gets EXACTLY the final message"
  step "adapter mode: $ACP_MODE ($REAL_ADAPTER_NOTE)"

  local created session_key scope_key
  created="$(new_acp_session "$ACP_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key scope_key <<<"$created"
  [[ "$session_key" == acp:* ]] || { fail "expected an acp: session key, got [$session_key]"; return 1; }
  step "session_key=$session_key scope=$scope_key"

  step "attaching \`transcript --follow\` BEFORE the prompt (the I12 live leg)"
  ainb_cli --format json fleet transcript "$session_key" --follow \
    >"$LOG_DIR/j2-transcript.ndjson" 2>"$LOG_DIR/j2-transcript.err" &
  local follower=$!
  BG_PIDS+=("$follower")
  wait_until 15 "the transcript follower to ack" \
    bash -c "test -s '$LOG_DIR/j2-transcript.ndjson'" || return 1

  local text="[j2 $RUN_ID] what are you working on?"
  local result message_id
  result="$(send_msg "j2-$RUN_ID" "$text" "$session_key")" || { fail "msg send failed"; return 1; }
  message_id="$(printf '%s' "$result" | jqr '.message_id')"
  # An ACP leg is PENDING at accept and resolves at TURN END: that is the
  # contract, so asserting it here is asserting the design, not tolerating slop.
  assert_eq "PENDING" "$(printf '%s' "$result" | jqr '.deliveries[0].state')" \
    "an ACP leg must be accepted PENDING, not resolved at send time" || return 1

  # The wall-clock half of I12: a content chunk is READABLE by the follower
  # while the leg is still PENDING. Line ordering alone would also be satisfied
  # by a stream that only flushed at turn end.
  step "asserting a chunk is readable while the leg is still PENDING"
  wait_until 60 "the first streamed content chunk" \
    grep -q '"acp\.\(message\|thought\|tool_call\|user_message\)"' "$LOG_DIR/j2-transcript.ndjson" || return 1
  local live_state
  live_state="$(delivery_state "$message_id" "$session_key")"
  assert_eq "PENDING" "$live_state" "the turn must still be open when the first chunk is readable" || return 1

  step "waiting for the turn to end"
  wait_until 90 "the delivery to resolve" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$message_id'\")\" == DELIVERED ]]" || return 1
  kill "$follower" 2>/dev/null || true

  step "asserting chunks arrived DURING the turn, not after it"
  local first_chunk_line turn_end_line
  first_chunk_line="$(grep -n '"acp\.\(message\|thought\|tool_call\|user_message\)"' "$LOG_DIR/j2-transcript.ndjson" | head -1 | cut -d: -f1)"
  turn_end_line="$(grep -n '"acp\.turn_completed"' "$LOG_DIR/j2-transcript.ndjson" | head -1 | cut -d: -f1)"
  [[ -n "$first_chunk_line" ]] || { fail "the follower streamed no content chunk at all"; return 1; }
  [[ -n "$turn_end_line" ]] || { fail "the follower never streamed acp.turn_completed"; return 1; }
  (( first_chunk_line < turn_end_line )) ||
    { fail "a chunk must arrive before turn end: first chunk on line $first_chunk_line, turn_completed on line $turn_end_line"; return 1; }

  step "asserting the timeline got EXACTLY the final message (I4)"
  local agent_rows chunk_count
  agent_rows="$(ainb_cli --format json fleet msg list --scope "$scope_key" --limit 100 |
    jq '[.messages[] | select(.kind == "agent")] | length')"
  assert_eq 1 "$agent_rows" "the timeline must hold exactly one agent message" || return 1
  chunk_count="$(ainb_cli --format json fleet transcript "$session_key" --limit 200 | jq '.chunks | length')"
  (( chunk_count > agent_rows )) ||
    { fail "the transcript ($chunk_count rows) must be richer than the timeline ($agent_rows row)"; return 1; }

  log "   $chunk_count transcript chunks · 1 timeline reply · first chunk line $first_chunk_line < turn end line $turn_end_line"
}

# ---- J3: resume across a daemon SIGKILL -----------------------------------

# The re-prime prelude's end marker (`ainb-acp/src/reprime.rs`), which is the
# one string that reaches the DAEMON binary only when the pool actually calls
# the resume renderer.
PHASE6_MARKER='=== end ainb chat context ==='

phase6_present() {
  # Does this daemon carry the Phase 6 RESUME routine?
  #
  # Not a wire probe: nothing on the v2 surface advertises resume, and
  # `fleet/transcript_prune` (Phase 6's other half) can merge ahead of it, so
  # the method's presence proves nothing. `ainb-acp::reprime` ships from Phase
  # 4, but nothing in the daemon CALLS it until the resume routine lands, so
  # its prelude marker is dead-stripped from a pre-Phase-6 daemon and linked
  # into a post-Phase-6 one. Absence is the safe default: J3 skips with a
  # reason rather than failing on an unimplemented phase, and the probe flips
  # itself the day the routine lands, with no edit here.
  grep -qa "$PHASE6_MARKER" "$BIN_DIR/ainb-hangar-daemon"
}

journey_j3() {
  banner "J3 · resume" \
    "SIGKILL the daemon mid-turn, restart, continue the SAME conversation with no ghost attention rows"

  if ! phase6_present; then
    skip "this daemon has no Phase 6 resume routine (the re-prime prelude marker is absent from the binary)"
    return 77
  fi

  local created session_key scope_key
  created="$(new_acp_session "$FIXTURE_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key scope_key <<<"$created"

  step "establishing the conversation"
  local first_id
  first_id="$(send_msg "j3a-$RUN_ID" "[j3 $RUN_ID] remember the secret word BANANA" "$session_key" | jqr '.message_id')"
  wait_until 90 "the first turn to complete" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$first_id'\")\" == DELIVERED ]]" || return 1

  step "opening a turn that never ends, then SIGKILLing the daemon"
  local stuck_id
  stuck_id="$(send_msg "j3b-$RUN_ID" "$HANG_TEXT" "$session_key" | jqr '.message_id')"
  wait_until 30 "the turn to be recorded as open" \
    bash -c "[[ -n \"\$(sqlite3 -readonly '$DB' \"SELECT open_turn_id FROM fleet_acp_session WHERE session_key='$session_key' AND open_turn_id IS NOT NULL\")\" ]]" || return 1
  kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true; DAEMON_PID=""
  kill_fixture_adapters

  step "restarting the daemon"
  start_daemon || return 1

  step "asserting the boot scan converged the interrupted leg"
  wait_until 60 "the stuck delivery to reach a terminal state" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$stuck_id'\")\" != PENDING ]]" || return 1
  local state detail
  state="$(delivery_state "$stuck_id" "$session_key")"
  detail="$(delivery_detail "$stuck_id" "$session_key")"
  assert_eq "UNKNOWN" "$state" "an interrupted leg converges UNKNOWN" || return 1
  assert_contains "$detail" "daemon_restart" "the receipt detail must be enumerated" || return 1

  step "asserting no ghost attention rows survived the restart"
  local open_rows
  open_rows="$(rpc attention/list '{"fleet":true}' |
    jq --arg key "$session_key" '[.result.attention[] | select(.session_id == $key)] | length')"
  assert_eq 0 "$open_rows" "a dead session must leave no open attention row" || return 1

  step "continuing the conversation on the SAME session_key"
  local resumed_id
  resumed_id="$(send_msg "j3c-$RUN_ID" "[j3 $RUN_ID] what was the secret word?" "$session_key" | jqr '.message_id')"
  wait_until 90 "the resumed turn to deliver" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$resumed_id'\")\" == DELIVERED ]]" || return 1

  local transcript
  transcript="$(ainb_cli --format json fleet transcript "$session_key" --limit 400)"
  local rebuilt
  rebuilt="$(printf '%s' "$transcript" | jq -r '[.chunks[] | select(.event_type == "acp.context_rebuilt")] | length')"
  (( rebuilt >= 1 )) || { fail "no acp.context_rebuilt marker: the resume path left no evidence of which leg ran"; return 1; }
  local mode
  mode="$(printf '%s' "$transcript" | jq -r 'last(.chunks[] | select(.event_type == "acp.context_rebuilt")) | .payload.mode // .payload.detail // "unknown"')"
  local replies
  replies="$(ainb_cli --format json fleet msg list --scope "$scope_key" --limit 100 |
    jq '[.messages[] | select(.kind == "agent")] | length')"
  (( replies >= 2 )) || { fail "the conversation did not continue: only $replies agent replies"; return 1; }
  # The resume path is ASSERTED, not merely printed. A daemon whose every
  # session/load fails re-primes forever and still delivers, so a journey that
  # only counts replies passes while the fast path is silently dead. That is
  # exactly the degradation the 2026-08-06 gate caught by hand.
  [[ "$mode" == loaded ]] || {
    fail "resume degraded to [$mode]: the adapter's own session/load did not carry the conversation"
    return 1
  }
  # Not asserted here, deliberately: that the agent RECALLS the secret word.
  # The fixture echoes its prompt, so only a real adapter can answer that, and
  # the check lives in the real-adapter probes (acp_resume_real.rs) which run
  # where an adapter is installed. This journey proves the transport path.

  log "   resumed via [$mode] on the same session_key · $replies agent replies · 0 ghost attention rows"
}

# ---- J4: runtime convergence (adapter dies, daemon lives) -----------------

journey_j4() {
  banner "J4 · convergence" \
    "SIGKILL only the ADAPTER: the scope converges and accepts the next message with no daemon restart"

  local created session_key scope_key
  created="$(new_acp_session "$FIXTURE_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key scope_key <<<"$created"
  local daemon_before="$DAEMON_PID"

  step "opening a turn that never ends"
  local stuck_id
  stuck_id="$(send_msg "j4a-$RUN_ID" "$HANG_TEXT" "$session_key" | jqr '.message_id')"
  wait_until 30 "the adapter process to exist and the turn to open" \
    bash -c "pgrep -f '$BIN_DIR/fake_acp_adapter' >/dev/null && [[ -n \"\$(sqlite3 -readonly '$DB' \"SELECT open_turn_id FROM fleet_acp_session WHERE session_key='$session_key' AND open_turn_id IS NOT NULL\")\" ]]" || return 1

  step "SIGKILLing the adapter process (scoped to this run's binary)"
  pkill -9 -f "$BIN_DIR/fake_acp_adapter" || { fail "no adapter process to kill"; return 1; }

  step "asserting runtime convergence, with NO daemon restart"
  wait_until 60 "the in-flight delivery to converge" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$stuck_id'\")\" != PENDING ]]" || return 1
  local state detail
  state="$(delivery_state "$stuck_id" "$session_key")"
  detail="$(delivery_detail "$stuck_id" "$session_key")"
  assert_eq "UNKNOWN" "$state" "a prompt the adapter swallowed converges UNKNOWN, never a blind resend" || return 1
  assert_contains "$detail" "adapter_exit" "the receipt detail must name the enumerated cause" || return 1
  assert_eq "$daemon_before" "$DAEMON_PID" "the daemon must not have been restarted" || return 1
  kill -0 "$DAEMON_PID" || { fail "the daemon died with the adapter"; return 1; }

  step "asserting the scope accepts the NEXT message with no restart"
  local next_id
  next_id="$(send_msg "j4b-$RUN_ID" "[j4 $RUN_ID] are you back?" "$session_key" | jqr '.message_id')"
  wait_until 90 "the next turn to deliver" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$next_id'\")\" == DELIVERED ]]" || return 1

  log "   converged UNKNOWN/$detail · same daemon pid $DAEMON_PID · next message DELIVERED"
}

# ---- J5a: bounded queue ---------------------------------------------------

journey_j5a() {
  banner "J5a · fault: queue overflow" \
    "prompts behind a wedged turn fill the bounded queue; the overflowing leg is REJECTED with detail queue_full"

  local created session_key
  created="$(new_acp_session "$FIXTURE_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key _ <<<"$created"

  step "wedging the scope's one in-flight turn"
  send_msg "j5a-hang-$RUN_ID" "$HANG_TEXT" "$session_key" >/dev/null || { fail "the wedging send failed"; return 1; }

  step "filling the queue until a leg is REJECTED"
  local index=0 rejected_id="" state
  while (( index < 80 )); do
    index=$(( index + 1 ))
    local result
    result="$(send_msg "j5a-$index-$RUN_ID" "[j5a $RUN_ID] queued prompt $index" "$session_key")" ||
      { fail "send $index exited non-zero"; return 1; }
    state="$(printf '%s' "$result" | jqr '.deliveries[0].state')"
    if [[ "$state" == "REJECTED" ]]; then
      rejected_id="$(printf '%s' "$result" | jqr '.message_id')"
      break
    fi
    assert_eq "PENDING" "$state" "send $index should have queued" || return 1
  done
  [[ -n "$rejected_id" ]] || { fail "the queue never rejected a prompt in $index sends: it is not bounded"; return 1; }
  (( index > 1 )) || { fail "the FIRST queued prompt was rejected: the queue has no depth at all"; return 1; }

  local detail
  detail="$(delivery_detail "$rejected_id" "$session_key")"
  assert_contains "$detail" "queue_full" "the rejection must carry the enumerated queue_full detail" || return 1

  log "   queue accepted $(( index - 1 )) prompts, then REJECTED/$detail"
}

# ---- J5b: turn deadline ---------------------------------------------------

journey_j5b() {
  banner "J5b · fault: turn deadline" \
    "an adapter that never ends its turn is converged by the deadline: UNKNOWN with detail turn_deadline"

  local created session_key
  created="$(new_acp_session "$FIXTURE_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key _ <<<"$created"

  step "sending the prompt the fixture never answers (deadline ${TURN_DEADLINE_MS}ms)"
  local stuck_id
  stuck_id="$(send_msg "j5b-$RUN_ID" "$HANG_TEXT" "$session_key" | jqr '.message_id')"

  local budget=$(( TURN_DEADLINE_MS / 1000 + 45 ))
  wait_until "$budget" "the deadline sweep to converge the turn" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$stuck_id'\")\" != PENDING ]]" || return 1

  local state detail
  state="$(delivery_state "$stuck_id" "$session_key")"
  detail="$(delivery_detail "$stuck_id" "$session_key")"
  assert_eq "UNKNOWN" "$state" "a turn killed by its deadline is UNKNOWN, never FAILED" || return 1
  assert_contains "$detail" "turn_deadline" "the receipt detail must name the deadline" || return 1

  # Only the adapter can say WHICH session was cancelled; the store cannot. The
  # grep is pinned to THIS session's adapter id, so another journey's wedged
  # scope timing out cannot satisfy it.
  local acp_id
  acp_id="$(db "SELECT COALESCE(acp_session_id,'') FROM fleet_acp_session WHERE session_key='$session_key';")"
  if [[ -n "$acp_id" && -f "$LOG_DIR/acp-rpc.log" ]]; then
    grep -qx "cancel:$acp_id" "$LOG_DIR/acp-rpc.log" ||
      { fail "the adapter recorded no session/cancel for $acp_id: the deadline never reached it"; return 1; }
  fi

  step "asserting the scope is reusable without a daemon restart"
  local next_id
  next_id="$(send_msg "j5b-next-$RUN_ID" "[j5b $RUN_ID] still there?" "$session_key" | jqr '.message_id')"
  wait_until 90 "the next turn to deliver" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$next_id'\")\" == DELIVERED ]]" || return 1

  log "   converged UNKNOWN/$detail after the deadline · scope reusable"
}

# ---- J5c: idempotency -----------------------------------------------------

journey_j5c() {
  banner "J5c · fault: idempotency replay" \
    "a replayed request_id delivers ONCE; the same id with different text exits 5"

  local target="${TMUX_KEYS[0]}" session="${TMUX_SESSIONS[0]}"
  local text="[j5c $RUN_ID] exactly once please"
  local request_id="j5c-$RUN_ID"

  local first second
  first="$(send_msg "$request_id" "$text" "$target")" || { fail "first send failed"; return 1; }
  assert_eq "DELIVERED" "$(printf '%s' "$first" | jqr '.deliveries[0].state')" "first send must deliver" || return 1
  assert_eq 1 "$(pane_hits "$session" "$text")" "the pane must show the payload once" || return 1

  step "replaying the SAME request_id with the SAME text"
  second="$(send_msg "$request_id" "$text" "$target")" || { fail "the replay exited non-zero"; return 1; }
  assert_eq "$(printf '%s' "$first" | jqr '.message_id')" "$(printf '%s' "$second" | jqr '.message_id')" \
    "a replay must return the original message_id" || return 1
  sleep 1
  assert_eq 1 "$(pane_hits "$session" "$text")" "the replay must NOT have submitted a second prompt" || return 1

  step "reusing the request_id with DIFFERENT text"
  local conflict_out conflict_code=0
  conflict_out="$(send_msg "$request_id" "$text and something else" "$target" 2>"$LOG_DIR/j5c.err")" || conflict_code=$?
  assert_eq 5 "$conflict_code" "an idempotency conflict must exit 5" || return 1
  assert_contains "$(cat "$LOG_DIR/j5c.err")" "idempotency_conflict" "the error JSON must name the kind" || return 1
  assert_eq 0 "$(pane_hits "$session" "$text and something else")" "the refused send must not reach the pane" || return 1

  log "   one delivery for two identical sends · exit 5 on the conflicting third"
}

# ---- J5d: permission round trip -------------------------------------------

journey_j5d() {
  banner "J5d · fault: permission round trip" \
    "an ACP permission raises an attention row, an operator approve reaches the adapter, and the turn finishes"

  if [[ -z "$PERMISSION_PROVIDER" ]]; then
    skip "real-adapter mode occupies one of the two registry tokens; the permission fixture has no slot"
    return 77
  fi

  local created session_key
  created="$(new_acp_session "$PERMISSION_PROVIDER")" || { fail "acp create failed"; return 1; }
  read -r session_key _ <<<"$created"

  local message_id
  message_id="$(send_msg "j5d-$RUN_ID" "[j5d $RUN_ID] rm -rf /tmp/fixture" "$session_key" | jqr '.message_id')"

  step "waiting for the attention row R8 exists for"
  local row=""
  local deadline=$(( $(date +%s) + 60 ))
  while (( $(date +%s) < deadline )); do
    row="$(rpc attention/list '{"fleet":true}' |
      jq -c --arg key "$session_key" 'first(.result.attention[]? | select(.session_id == $key))')"
    [[ -n "$row" && "$row" != null ]] && break
    row=""
    sleep 0.5
  done
  [[ -n "$row" ]] || { fail "no attention row was ever raised for $session_key"; return 1; }
  assert_eq "approval" "$(printf '%s' "$row" | jqr '.kind')" "the row must be an approval" || return 1
  local fingerprint
  fingerprint="$(printf '%s' "$row" | jq -r '.payload | fromjson | .requestFingerprint')"
  [[ -n "$fingerprint" && "$fingerprint" != null ]] || { fail "the row carries no requestFingerprint: $row"; return 1; }

  # NOTE: part 1 ships no CLI verb for answering an ACP permission (`ainb fleet
  # approve` is the notifyd broker path for Claude hooks, a different
  # transport). The answer rides `fleet/action`, which is exactly what the TUI
  # and the macOS app send, so this journey speaks that wire directly.
  step "approving over fleet/action (the same wire the TUI uses)"
  local version approved
  version="$(rpc fleet/snapshot | jq --arg key "$session_key" '.result.sessions[] | select(.session_key == $key) | .version')"
  approved="$(rpc fleet/action "$(jq -nc --arg key "$session_key" --argjson version "$version" \
    --arg request "j5d-answer-$RUN_ID" --arg fingerprint "$fingerprint" \
    '{session_key:$key, expected_version:$version, request_id:$request,
      action:{action:"approve", request_fingerprint:$fingerprint}}')")"
  assert_eq "DELIVERED" "$(printf '%s' "$approved" | jqr '.result.receipt.status')" \
    "the answer must reach the adapter, not the \"transport is not active\" Unknown" || return 1
  assert_contains "$(printf '%s' "$approved" | jqr '.result.receipt.detail')" "allow" \
    "the receipt must name the option that was taken" || return 1

  step "asserting the approved turn actually finished"
  wait_until 90 "the delivery to resolve after the approval" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_message_delivery WHERE message_id='$message_id'\")\" == DELIVERED ]]" || return 1
  local still_open
  still_open="$(rpc attention/list '{"fleet":true}' |
    jq --arg key "$session_key" '[.result.attention[]? | select(.session_id == $key)] | length')"
  assert_eq 0 "$still_open" "an answered permission must close its attention row" || return 1

  log "   attention row → fleet/action approve → DELIVERED, row closed"
}

# ---- J5e: unknown target ---------------------------------------------------

journey_j5e() {
  banner "J5e · fault: unknown target" \
    "one bad recipient is REJECTED per-delivery with detail target_unknown; the request itself still succeeds"

  local text="[j5e $RUN_ID] one good target, one ghost"
  local ghost="acp:NO-SUCH-SESSION-$RUN_ID"
  local scope="broadcast:smoke-j5e-$RUN_ID"
  local result code=0
  result="$(send_msg_scoped "$scope" "j5e-$RUN_ID" "$text" "${TMUX_KEYS[0]}" "$ghost")" || code=$?
  assert_eq 0 "$code" "a request naming an unknown target must still succeed" || return 1

  local message_id good bad
  message_id="$(printf '%s' "$result" | jqr '.message_id')"
  good="$(printf '%s' "$result" | jq -r --arg key "${TMUX_KEYS[0]}" '.deliveries[] | select(.session_key == $key) | .state')"
  bad="$(printf '%s' "$result" | jq -r --arg key "$ghost" '.deliveries[] | select(.session_key == $key) | .state')"
  assert_eq "DELIVERED" "$good" "the real recipient must still receive it" || return 1
  assert_eq "REJECTED" "$bad" "the ghost recipient must be REJECTED" || return 1
  assert_contains "$(delivery_detail "$message_id" "$ghost")" "target_unknown" \
    "the rejection must carry the enumerated target_unknown detail" || return 1
  assert_eq 1 "$(pane_hits "${TMUX_SESSIONS[0]}" "$text")" "the good pane must still show the payload once" || return 1

  local persisted
  persisted="$(ainb_cli --format json fleet msg list --scope "$scope" --limit 20 |
    jq -r --arg id "$message_id" '[.messages[] | select(.id == $id)] | length')"
  assert_eq 1 "$persisted" "the message must be persisted despite the bad leg" || return 1

  log "   DELIVERED + REJECTED/target_unknown in one request, message persisted"
}

# --------------------------------------------------------------------- driver

ALL_JOURNEYS=(j1 j2 j3 j4 j5a j5b j5c j5d j5e)

run_journey() {
  local name="$1"
  SKIP_REASON=""; FAIL_REASON=""
  local status=0
  "journey_$name" || status=$?
  if (( status == 0 )); then
    RESULT_LINES+=("SMOKE-RESULT $name PASS -")
    printf '   %s✓ %s PASS%s\n' "$c_green" "$name" "$c_off"
  elif (( status == 77 )); then
    RESULT_LINES+=("SMOKE-RESULT $name SKIP ${SKIP_REASON:-no reason given}")
    printf '   %s~ %s SKIP (%s)%s\n' "$c_yellow" "$name" "$SKIP_REASON" "$c_off"
  else
    FAILED=1
    RESULT_LINES+=("SMOKE-RESULT $name FAIL ${FAIL_REASON:-see diagnostics}")
    printf '   %s✗ %s FAIL%s\n' "$c_red" "$name" "$c_off"
    dump_diagnostics
  fi
}

main() {
  local selected=()
  while (( $# )); do
    case "$1" in
      --keep) KEEP_ROOT=1 ;;
      -h|--help) sed -n '2,45p' "${BASH_SOURCE[0]}" | cut -c3-; exit 0 ;;
      all) selected+=("${ALL_JOURNEYS[@]}") ;;
      j5) selected+=(j5a j5b j5c j5d j5e) ;;
      j1|j2|j3|j4|j5a|j5b|j5c|j5d|j5e) selected+=("$1") ;;
      *) log "unknown journey: $1 (try: ${ALL_JOURNEYS[*]}, j5, all)"; exit 2 ;;
    esac
    shift
  done
  (( ${#selected[@]} )) || selected=("${ALL_JOURNEYS[@]}")

  local tools=(tmux jq python3 sqlite3)
  [[ "${AINB_SMOKE_SKIP_BUILD:-0}" == 1 ]] || tools+=(cargo)
  for tool in "${tools[@]}"; do
    command -v "$tool" >/dev/null || { log "missing required tool: $tool"; exit 2; }
  done

  banner "chat-bus smoke · run $RUN_ID" "scratch world $ROOT · journeys: ${selected[*]}"
  setup_world || { dump_diagnostics; log "SMOKE-RESULT setup FAIL world did not come up"; exit 1; }
  log "   adapter mode: ${c_bold}$ACP_MODE${c_off} ($REAL_ADAPTER_NOTE)"
  log "   conversation provider: $ACP_PROVIDER · fixture provider: $FIXTURE_PROVIDER · permission provider: ${PERMISSION_PROVIDER:-none}"
  log "   tmux sessions: ${TMUX_SESSIONS[*]}"
  log "   daemon pid: $DAEMON_PID · turn deadline: ${TURN_DEADLINE_MS}ms"

  for name in "${selected[@]}"; do run_journey "$name"; done

  printf '\n%s── summary ──%s\n' "$c_bold" "$c_off"
  printf '%s\n' "${RESULT_LINES[@]}"
  if (( FAILED )); then
    printf '%sSMOKE-RESULT overall FAIL%s\n' "$c_red" "$c_off"
    exit 1
  fi
  printf '%sSMOKE-RESULT overall PASS%s\n' "$c_green" "$c_off"
}

main "$@"
