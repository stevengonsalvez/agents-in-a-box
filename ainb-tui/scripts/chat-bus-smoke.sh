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
#   ./scripts/chat-bus-smoke.sh j6         # the real TUI, the operating surface
#   ./scripts/chat-bus-smoke.sh j7         # the real TUI's copilot chat screen
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
#   * for J6 and J7, the REAL `ainb tui` in a session of its own on that private
#     server. Every other journey drives the daemon and the CLI; those two drive
#     the OPERATING SURFACE, because part 1 shipped a bus the TUI consumed
#     without anything ever opening it. J6 reads the Fleet panel's roster; J7
#     opens the copilot CHAT on it and operates the conversation.
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
# The three fake-agent panes, and ONLY those: J1 asserts a delivery in every
# member of this array, so J6's TUI pane is tracked separately rather than
# appended here.
TMUX_SESSIONS=()
TUI_SESSION=""
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
  # The whole screen, not a tail: a red J6 is nearly always "what was actually
  # on the operating surface", and the answer is the top of the pane.
  if [[ -n "$TUI_SESSION" ]]; then
    printf '%s[TUI pane %s]%s\n' "$c_yellow" "$TUI_SESSION" "$c_off"
    tmux_cmd capture-pane -p -t "$TUI_SESSION" 2>&1 || true
  fi
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
  [[ -n "$TUI_SESSION" ]] && tmux_cmd kill-session -t "$TUI_SESSION" 2>/dev/null
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

# The ONE writer in this file, and it seeds a PRECONDITION, never an outcome.
#
# J7 needs an open guardrail confirm card on screen. A card is minted by
# `ainb_hangar_daemon::copilot::gate`, called from the copilot's own tool
# bridge — and that bridge is not built yet, so NOTHING on the wire, in the CLI
# or in the TUI can open one. Seeding the row is the only way to reach the
# surface under test at all.
#
# What that costs is scoped and stated: the row is the precondition, and every
# assertion AFTER it is real product behaviour on the real wire — the panel
# decoding the card through `fleet/confirm_list`, rendering it answerable, and
# `y` resolving it through `fleet/confirm_answer`. The day the tool bridge
# lands, this seed is replaced by a copilot tool call and nothing else moves.
db_write() { sqlite3 "$DB" "$1"; }

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

# Every first-run surface, pre-answered. A CLI verb only ever met the
# onboarding wizard, so this used to be one file; the TUI (J6) meets all of
# them, and each one intercepts keystrokes until it is satisfied.
#
# Runs AFTER `build_binaries` because the onboarding record has to name the
# version of the binary under test — see below.
seed_first_run_records() {
  # A completed onboarding record so no first-run wizard can intercept a CLI
  # verb (same trap the tmux tripwires document).
  #
  # The version is READ FROM THE BINARY, never a literal: `needs_onboarding`
  # (ainb-core/src/config/onboarding.rs) re-runs the wizard when the record's
  # MAJOR version differs from the binary's, so a placeholder like `smoke` is
  # indistinguishable from major 0 and the wizard fires anyway — invisible to
  # every CLI journey, and fatal to J6, whose `f` the wizard swallows.
  mkdir -p "$SCRATCH_HOME/.agents-in-a-box/config"
  local version
  version="$("$BIN_DIR/ainb" --version | awk '{print $2}')"
  cat >"$SCRATCH_HOME/.agents-in-a-box/config/onboarding.toml" <<TOML
completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "$version"
skipped_dependencies = []
git_directories = []
TOML

  # …and a DISMISSED notify-install record, for the same reason one screen up.
  #
  # `notifyd::Paths::from_home` (crates/ainb-plugin-notifyd/src/paths.rs)
  # resolves `AINB_HANGAR_HOME` BEFORE `AINB_HOME`, and this world sets both.
  # Seeded under only one base, the "install the notify hooks?" offer fires on
  # first paint and its MODAL swallows the `f` that opens the Fleet panel — J6
  # would then fail on a first-run prompt while looking like a Fleet bug.
  # Written under BOTH bases so the seeding never depends on which var wins.
  local notify_record='{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}'
  printf '%s' "$notify_record" >"$HANGAR_HOME/install.json"
  printf '%s' "$notify_record" >"$SCRATCH_HOME/.agents-in-a-box/install.json"
}

setup_world() {
  mkdir -p "$BIN_DIR" "$LOG_DIR" "$SCRATCH_HOME" "$HANGAR_HOME/hangar" "$TMUX_DIR"
  write_rpc_client
  write_fake_agent
  write_turn_script
  build_binaries || return 1
  seed_first_run_records
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

# The cwd defaults to the scratch root. J6 overrides it because the Fleet card's
# identity line is the cwd's basename, and that is what makes its assertion
# specific to its own row.
new_acp_session() { # provider [cwd] -> prints "<session_key> <scope_key>"
  ainb_cli --format json fleet acp create --provider "$1" --cwd "${2:-$ROOT}" |
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

# ---- J6: the operating surface --------------------------------------------

# Chrome the panel itself paints (`ainb-core/src/components/fleet_panel.rs`),
# so a match means the Fleet panel is ON SCREEN, not merely that the TUI booted.
FLEET_PANEL_MARKER='Fleet ·'
# The cold-launch home screen's own banner (`components/home_screen_v2.rs`).
# Waiting for it before the keypress is what makes "one press of f" an honest
# claim: a key sent into a terminal that has not painted yet is a key nobody can
# prove was seen. Spaced letters ON PURPOSE — the setup wizard's own splash says
# "Welcome to Agents in a Box!", so the unspaced name would match a screen that
# eats `f` rather than the screen that acts on it.
HOME_SCREEN_MARKER='A I N B'

tui_pane()     { tmux_cmd capture-pane -p -t "$TUI_SESSION"; }
tui_pane_has() { tui_pane | grep -qF -- "$1"; }

journey_j6() {
  banner "J6 · the operating surface" \
    "a chat-bus ACP session is VISIBLE in the real Fleet panel of a cold-launched \`ainb tui\`, opened by ONE press of \`f\`"

  "$BIN_DIR/ainb" tui --help >/dev/null 2>&1 || {
    skip "this ainb has no \`tui\` subcommand: there is no operating surface to drive"
    return 77
  }

  # Deliberately NOT setting `AINB_FLEET_DISABLE_TMUX_DISCOVERY`: it gates the
  # DAEMON's reconciler (`ainb-hangar-daemon/src/fleet.rs`), and this world runs
  # ONE daemon that J1/J5c/J5e need discovery from. Setting it on the TUI child
  # would be a no-op anyway — the TUI is a snapshot reader, not a discoverer. So
  # the three fake `claude` panes DO share the roster, and J6 earns its
  # specificity by anchoring on its own card instead of on an empty panel.
  local project="$ROOT/j6-project"
  local identity="j6-project"
  mkdir -p "$project"

  local created session_key
  created="$(new_acp_session "$ACP_PROVIDER" "$project")" || { fail "acp create failed"; return 1; }
  read -r session_key _ <<<"$created"
  [[ "$session_key" == acp:* ]] || { fail "expected an acp: session key, got [$session_key]"; return 1; }
  step "session_key=$session_key cwd=$project"

  step "cold-launching the REAL \`ainb tui\` on the private tmux server"
  TUI_SESSION="ainb-smoke-tui-$RUN_ID"
  # NOT the `ainb-smoke-$RUN_ID-` prefix the fake agents use: `resolve_tmux_targets`
  # selects on that prefix, and a TUI pane joining the roster would break its
  # exactly-3 check. 200x50 so the panel renders its wide two-column form.
  tmux_cmd new-session -d -s "$TUI_SESSION" -x 200 -y 50 \
    "env -u TMUX HOME=$SCRATCH_HOME AINB_HOME=$SCRATCH_HOME/.agents-in-a-box \
       AINB_HANGAR_HOME=$HANGAR_HOME TMUX_TMPDIR=$TMUX_DIR PATH=$BIN_DIR:$PATH \
       AINB_DISABLE_PLUGINS=1 AINB_CODEX_MANAGED=0 \
       CLAUDE_PEERS_DB=$ROOT/j6-peers.db AINB_FLEET_JOBS_DIR=$ROOT/j6-jobs \
       $BIN_DIR/ainb tui"
  wait_until 60 "the TUI to paint its home screen" tui_pane_has "$HOME_SCREEN_MARKER" || return 1

  step "pressing \`f\` ONCE, the way a user opens Fleet"
  tmux_cmd send-keys -t "$TUI_SESSION" f
  # A retry loop here would be a lie: pressing `f` until something happens is
  # exactly how a modal eating the first one goes unnoticed.
  wait_until 30 "the Fleet panel to open on the FIRST \`f\` (a modal in the way would eat it)" \
    tui_pane_has "$FLEET_PANEL_MARKER" || return 1

  # The panel opens on the `Needs input` lens, and a freshly minted ACP session
  # needs nothing, so its card is filtered out until the operator widens the
  # lens. `5` is not a workaround: it is the key the panel's OWN empty state
  # prints ("Press 5 for All"), so this is still the path a user walks.
  step "pressing \`5\` for the All lens, the key the panel's empty state names"
  tmux_cmd send-keys -t "$TUI_SESSION" 5
  wait_until 45 "the card for $session_key ($identity) to appear in the panel" \
    tui_pane_has "$identity" || return 1

  # `render_session_card` (ainb-plugin-hangar/src/screen/fleet.rs) paints each
  # roster entry as four lines: a top border, status + age, the identity, then
  # `<branch>  ·  <provider>  ·  <attachment>`. The provider cell is read from
  # the line AFTER this journey's own identity line, which is what stops the
  # assertion passing on an incidental `acp` elsewhere in the pane — the session
  # key, the scratch paths and any other journey's row all contain one.
  #
  # The identity match is ANCHORED at the card's left border (`│` at column 0,
  # the `▶ `/`  ` marker at column 2) because `capture-pane` returns the whole
  # screen on one physical line per row: the roster header and the detail pane
  # to the right of the divider both repeat this identity, and an unanchored
  # match lands on one of those and reads a provider cell from the wrong column.
  local pane card identity_line provider_line provider_cell provider_token
  pane="$(tui_pane)"
  # `|| true` because this grep legitimately finds nothing when the card is
  # rendered in a shape this anchor does not know about, and `set -e` would turn
  # that into a bare abort with no reason instead of the `fail` two lines down.
  card="$(printf '%s\n' "$pane" | grep -m1 -A1 -E "^│ +(▶ )?$identity( |$)" || true)"
  identity_line="$(printf '%s\n' "$card" | head -1)"
  provider_line="$(printf '%s\n' "$card" | tail -1)"
  [[ -n "$identity_line" && "$provider_line" != "$identity_line" ]] ||
    { fail "no roster card for $identity in the Fleet panel: [$identity_line]"; return 1; }
  provider_cell="$(printf '%s' "$provider_line" | awk -F'  ·  ' '{print $2}' | tr -d '[:space:]')"
  provider_token="$(printf '%s' "$provider_cell" | tr '[:upper:]' '[:lower:]')"

  # Echoed, not just asserted: a recording of this run should SHOW the operator's
  # own two lines, the same way J1 echoes the delivered pane text. Trimmed at the
  # `││` seam between the roster column and the detail pane, and squeezed, so the
  # card reads as one line instead of 200 columns of card padding.
  step "card identity, as the operator sees it:$(printf '%s' "$identity_line" | awk -F'││' '{print $1}' | tr -s ' ')"
  step "card provider row, as the operator sees it:$(printf '%s' "$provider_line" | awk -F'││' '{print $1}' | tr -s ' ')"

  # `unknown` first: it is the SILENT degradation this journey exists for. The
  # panel maps the wire token in one place and falls back rather than failing,
  # so an unmapped provider looks like a live session with a shrugging label.
  [[ "$provider_token" != unknown ]] ||
    { fail "the Fleet card labelled $session_key [$provider_cell]: an unmapped wire token degraded to the silent fallback"; return 1; }
  assert_eq "acp" "$provider_token" "the Fleet card must name the ACP session's provider" || return 1

  local roster
  roster="$(printf '%s\n' "$pane" | grep -m1 -F -- "$FLEET_PANEL_MARKER" | tr -s ' ')"
  tmux_cmd send-keys -t "$TUI_SESSION" Escape
  tmux_cmd kill-session -t "$TUI_SESSION" 2>/dev/null || true
  TUI_SESSION=""

  log "   Fleet opened on the first \`f\` ·$roster· card [$identity] labelled [$provider_cell]"
}

# ---- J7: the copilot chat, on the operating surface -----------------------

# Chrome the chat screen itself paints (`ainb-plugin-hangar/src/screen/fleet_chat.rs`),
# so a match means the copilot CHAT is on screen, not merely that Fleet opened.
CHAT_SCREEN_MARKER='Fleet chat · #copilot'
# The composer's own help line. The header above paints while the screen is
# still LOADING, so waiting for this instead is what makes the keys that follow
# land on a surface that can receive them.
CHAT_COMPOSER_MARKER='Enter sends · Tab confirm cards'
# The cards help line, which the pane prints only once focus IS on the cards.
CHAT_CARDS_MARKER='↑↓ card · y approve'

# The newest copilot channel AS THE DAEMON REPORTS IT, through the CLI's own
# `fleet/channel_list` round trip.
#
# Deliberately NOT read off the pane: the thing under test is whether the screen
# shows the scope the daemon minted, and a reader that took its expected value
# from the screen would agree with any string the screen chose to print. The
# first version of this surface hardcoded `channel:copilot` and read an empty
# timeline forever while every one of its unit tests stayed green.
copilot_scope() {
  ainb_cli --format json fleet channel list |
    jq -r '[.channels[] | select(.kind == "copilot")] | last | .scope_key // empty'
}
have_copilot_channel() { [[ -n "$(copilot_scope)" ]]; }

# One timeline row, ANCHORED on the attribution column.
#
# `render_timeline` paints a fixed-width 12-column author label, then `│ `, then
# the body, precisely so a reader can name the row it means. A pane-wide grep
# for the body would match the composer still echoing what the operator typed,
# and — the failure this journey exists for — would pass even if both authors
# rendered under the same name.
chat_row() { # label body -> the whole row, or empty
  tui_pane | grep -m1 -E "^ *$1 +│ $2" || true
}
chat_row_present() { [[ -n "$(chat_row "$1" "$2")" ]]; }

# The open confirm card's own row: `render_cards` paints
# `<cursor> [<state>] <tool>  <arguments>  <hint>`.
chat_card_row() { tui_pane | grep -m1 -E '^ *(▶ )?\[OPEN\] +kill ' || true; }
chat_card_present() { [[ -n "$(chat_card_row)" ]]; }

journey_j7() {
  banner "J7 · the copilot chat" \
    "the real chat screen on a cold-launched \`ainb tui\`: the DAEMON's channel scope on screen, the operator's own message in the conversation, two authors on DISTINCT rows, and an open confirm card answered from the pane"

  "$BIN_DIR/ainb" tui --help >/dev/null 2>&1 || {
    skip "this ainb has no \`tui\` subcommand: there is no operating surface to drive"
    return 77
  }
  # Part 2's dispatch arms, probed on the WIRE. A part 1 daemon answers -32601
  # here, and failing on that would report an unimplemented phase as a
  # regression. Same posture as J3's Phase 6 probe: absence skips with a reason,
  # and the probe flips itself the day the arms land, with no edit here.
  local probe
  probe="$(rpc fleet/channel_list)"
  if [[ "$(printf '%s' "$probe" | jqr '.error.code // empty')" == "-32601" ]]; then
    skip "this daemon has no part 2 chat dispatch (fleet/channel_list is not a method)"
    return 77
  fi

  local project="$ROOT/j7-project"
  mkdir -p "$project"

  step "cold-launching the REAL \`ainb tui\` on the private tmux server"
  TUI_SESSION="ainb-smoke-chat-$RUN_ID"
  # `-c "$project"`: the chat screen creates the copilot's ACP session against
  # the TUI's OWN cwd, and the daemon pins a scope to the cwd that first claimed
  # it. A scratch dir keeps that fact inside this run's world. NOT the
  # `ainb-smoke-$RUN_ID-` prefix the fake agents use — `resolve_tmux_targets`
  # selects on that, and a TUI pane joining the roster breaks its exactly-3
  # check. 200x50 so the chat renders its full-height form.
  tmux_cmd new-session -d -s "$TUI_SESSION" -x 200 -y 50 -c "$project" \
    "env -u TMUX HOME=$SCRATCH_HOME AINB_HOME=$SCRATCH_HOME/.agents-in-a-box \
       AINB_HANGAR_HOME=$HANGAR_HOME TMUX_TMPDIR=$TMUX_DIR PATH=$BIN_DIR:$PATH \
       AINB_DISABLE_PLUGINS=1 AINB_CODEX_MANAGED=0 \
       CLAUDE_PEERS_DB=$ROOT/j7-peers.db AINB_FLEET_JOBS_DIR=$ROOT/j7-jobs \
       $BIN_DIR/ainb tui"
  wait_until 60 "the TUI to paint its home screen" tui_pane_has "$HOME_SCREEN_MARKER" || return 1

  # Each key pressed ONCE, after waiting for the screen that receives it. A
  # retry loop is how a modal swallowing the first press goes unnoticed, which
  # is a real bug this suite has already caught.
  step "pressing \`f\` ONCE, the way a user opens Fleet"
  tmux_cmd send-keys -t "$TUI_SESSION" f
  wait_until 30 "the Fleet panel to open on the FIRST \`f\`" \
    tui_pane_has "$FLEET_PANEL_MARKER" || return 1
  step "pressing \`m\` ONCE, the way a user opens the copilot chat"
  tmux_cmd send-keys -t "$TUI_SESSION" m
  wait_until 30 "the chat to open on the FIRST \`m\` (a modal in the way would eat it)" \
    tui_pane_has "$CHAT_SCREEN_MARKER" || return 1
  wait_until 60 "the chat to finish loading (its composer help line)" \
    tui_pane_has "$CHAT_COMPOSER_MARKER" || return 1

  step "reading the channel scope back from the DAEMON, independently of the screen"
  wait_until 45 "the daemon to report the copilot channel the screen asked it to mint" \
    have_copilot_channel || return 1
  local scope
  scope="$(copilot_scope)"
  case "$scope" in
    channel:copilot)
      fail "the chat is bound to a hardcoded channel:copilot; the daemon mints channel:<ulid>"
      return 1 ;;
    channel:?*) ;;
    *) fail "expected a channel:<ulid> scope from fleet/channel_list, got [$scope]"; return 1 ;;
  esac
  # The header CELL, not the header ROW. `capture-pane` returns one physical
  # line per screen row, and anything floating to the right of the chat (a
  # "Workspaces loaded" toast at column 150, say) lands on the same line. So the
  # comparison is anchored at the START of the row and bounded by a space: the
  # header must read EXACTLY this and nothing else, which a `contains` on the
  # scope would not prove — the scope string appears in the status line too.
  local expected_header="$CHAT_SCREEN_MARKER · $scope"
  local header
  header="$(tui_pane | grep -m1 -E "^ *$CHAT_SCREEN_MARKER · " | sed 's/^ *//')"
  step "chat header, as the operator sees it: ${header:0:${#expected_header}}"
  assert_eq "$expected_header" "${header:0:${#expected_header}}" \
    "the chat must name the scope the DAEMON minted, not one of its own" || return 1
  case "${header:${#expected_header}:1}" in
    ''|' ') ;;
    *) fail "the chat header does not end at the daemon's scope: [$header]"; return 1 ;;
  esac

  # The session the channel's membership actually resolves to
  # (`FleetAcpSessionRepo::get_live_by_scope`), read READ-ONLY from the store:
  # nothing on the v2 wire projects a scope onto its ACP session, and the
  # screen's own status line is part of what is under test, so it cannot be the
  # source of the target either.
  step "resolving the copilot session the chat created on that scope"
  local live_session_sql="SELECT session_key FROM fleet_acp_session WHERE scope_key='$scope' AND state IN ('ACTIVE','IDLE');"
  wait_until 60 "the chat to create the copilot's ACP session" \
    bash -c "[[ -n \"\$(sqlite3 -readonly '$DB' \"$live_session_sql\")\" ]]" || return 1
  local target
  target="$(db "$live_session_sql")"

  step "typing in the composer and pressing Enter ONCE, the way an operator asks"
  local operator_text="what is blocked right now"
  tmux_cmd send-keys -t "$TUI_SESSION" -l "$operator_text"
  tmux_cmd send-keys -t "$TUI_SESSION" Enter
  wait_until 60 "the operator's message to reach the conversation, attributed to the operator" \
    chat_row_present YOU "$operator_text" || return 1

  # A COPILOT-authored line, minted by the DAEMON from the `actor` the wire
  # carries — the same field `copilot::post_channel_message` sets. Sent over
  # `fleet/message_send` directly because part 2 ships no CLI verb that writes
  # as the copilot, exactly as J5d speaks `fleet/action` for the same reason.
  step "posting a COPILOT-authored line on the wire the daemon's own copilot writes on"
  local copilot_text="session one is waiting on an approval"
  local posted
  posted="$(rpc fleet/message_send "$(jq -nc --arg scope "$scope" --arg target "$target" \
    --arg text "$copilot_text" --arg request "j7-copilot-$RUN_ID" \
    '{scope_key:$scope, actor:"copilot", targets:[$target], text:$text, request_id:$request}')")"
  [[ -n "$(printf '%s' "$posted" | jqr '.result.message_id // empty')" ]] ||
    { fail "the copilot line was not accepted by the daemon: $posted"; return 1; }
  wait_until 60 "the copilot's line to reach the conversation, attributed to the copilot" \
    chat_row_present COPILOT "$copilot_text" || return 1

  local operator_row copilot_row
  operator_row="$(chat_row YOU "$operator_text")"
  copilot_row="$(chat_row COPILOT "$copilot_text")"
  # Echoed, not merely asserted: a recording of this run should SHOW the two
  # attributed rows, the way J1 echoes the delivered pane text.
  step "operator row, as the operator sees it:$(printf '%s' "$operator_row" | tr -s ' ')"
  step "copilot row, as the operator sees it:$(printf '%s' "$copilot_row" | tr -s ' ')"
  [[ "$operator_row" != "$copilot_row" ]] ||
    { fail "the two authors rendered as the same row"; return 1; }
  # The masquerade check, BOTH ways. The wire carries the author precisely so a
  # copilot write cannot wear a human's name, and that guarantee dies at the
  # last inch if the panel paints either row under the other's label.
  [[ -z "$(chat_row YOU "$copilot_text")" ]] ||
    { fail "the copilot's line is also attributed to the operator: a copilot write can wear a human's name"; return 1; }
  [[ -z "$(chat_row COPILOT "$operator_text")" ]] ||
    { fail "the operator's line is also attributed to the copilot"; return 1; }

  step "seeding ONE open confirm card (a precondition the copilot's tool bridge will mint; see db_write)"
  local confirm_id="j7-card-$RUN_ID"
  # `expires_at` far in the future on purpose: `list_open` and `resolve` BOTH
  # carry an `expires_at > now` term, so a card whose TTL has lapsed is
  # invisible and unanswerable by design, and a short one here would test the
  # clock rather than the pane.
  db_write "INSERT INTO fleet_confirm
      (confirm_id, scope_key, tool, arguments, target_session_key, state,
       edited_arguments, created_at, expires_at, answered_at)
    VALUES
      ('$confirm_id', '$scope', 'kill', '{\"session\":\"${TMUX_KEYS[0]}\"}',
       '${TMUX_KEYS[0]}', 'open', NULL, $(date +%s)000, 4000000000000, NULL);" ||
    { fail "could not seed the confirm card"; return 1; }
  wait_until 45 "the card to reach the pane" chat_card_present || return 1
  local card_row
  card_row="$(chat_card_row)"
  step "confirm card, as the operator sees it:$(printf '%s' "$card_row" | tr -s ' ')"
  assert_contains "$card_row" "y approve" \
    "an OPEN card must render as ANSWERABLE, not as a card the operator can only read" || return 1

  step "pressing \`Tab\` ONCE to focus the cards"
  tmux_cmd send-keys -t "$TUI_SESSION" Tab
  wait_until 20 "focus to reach the cards (their own help line)" \
    tui_pane_has "$CHAT_CARDS_MARKER" || return 1
  # Focus does NOT arm a card. Nothing is selected until the operator picks one,
  # which the surface says itself ("pick a card with up/down before answering").
  # That is deliberate: a background poll used to adopt the first card, so `y`
  # could approve a destructive call nobody had read. Approving now takes two
  # keys and this journey presses both.
  step "pressing \`Down\` ONCE to pick the card, which focus deliberately does not do"
  tmux_cmd send-keys -t "$TUI_SESSION" Down
  step "pressing \`y\` ONCE to approve, from the pane"
  tmux_cmd send-keys -t "$TUI_SESSION" y
  wait_until 45 "the daemon to record the answer" \
    bash -c "[[ \"\$(sqlite3 -readonly '$DB' \"SELECT state FROM fleet_confirm WHERE confirm_id='$confirm_id'\")\" != open ]]" || return 1
  local card_state
  card_state="$(db "SELECT state FROM fleet_confirm WHERE confirm_id='$confirm_id';")"
  assert_eq "approved" "$card_state" \
    "\`y\` from the pane must approve the card through fleet/confirm_answer" || return 1
  wait_until 30 "the answered card to LEAVE the pane" \
    tui_pane_has "CONFIRM CARDS · none open" || return 1

  tmux_cmd send-keys -t "$TUI_SESSION" Escape
  tmux_cmd kill-session -t "$TUI_SESSION" 2>/dev/null || true
  TUI_SESSION=""

  log "   chat opened on one \`f\` + one \`m\` · scope [$scope] on screen · YOU and COPILOT on distinct rows · card [$confirm_id] answered [$card_state]"
}

# --------------------------------------------------------------------- driver

ALL_JOURNEYS=(j1 j2 j3 j4 j5a j5b j5c j5d j5e j6 j7)

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
      -h|--help) sed -n '2,50p' "${BASH_SOURCE[0]}" | cut -c3-; exit 0 ;;
      all) selected+=("${ALL_JOURNEYS[@]}") ;;
      j5) selected+=(j5a j5b j5c j5d j5e) ;;
      j1|j2|j3|j4|j5a|j5b|j5c|j5d|j5e|j6|j7) selected+=("$1") ;;
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
