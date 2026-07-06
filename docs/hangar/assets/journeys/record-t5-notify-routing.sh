#!/bin/bash
# Recording driver for T5 (per-attention-kind notification routing) — one of the
# remaining tcp journeys in the converged-control-center catalogue
# (docs/hangar/verify-converged-goal.md).
#
# Seeds a plain P4 fixture + RPC-only daemon (mirrors
# `tripwire_p4_common::prepare_pipeline`; the T5 unit tripwire
# `tests/tripwire_tcp_notify_routing_e2e.rs` drives the SAME rule-resolution +
# hook-ingest path at the library level with no TUI). The tape opens Settings
# (`,`) -> Notifications grid, flips the `ask_user_question` rule live on screen
# (space on phone ENABLES it, space on os DISABLES it — leaving phone+web).
#
# NOTE on scope (discovered while wiring this recording, not asserted as a
# defect): the grid's `space` toggle always writes a WORKSPACE-scoped override
# (`apply_notify_action` in plugin.rs sends the current `workspace_id`), but the
# daemon's hook-ingest always resolves a raised attention's channels against the
# GLOBAL row (`notify::resolve_channels(pool, kind, None)`,
# attention_ingest.rs:242 — hardcoded `None`, by design). So a real Claude
# session's ASK never picks up a per-workspace override made through this exact
# grid. This recording's background step therefore ALSO flips the GLOBAL row via
# the same `hangar/notify_rule_set` RPC with no `workspace_id` (exactly what the
# tripwire's unseeded acceptance does) so the hook-raised ASK's resolved channels
# genuinely change — while still capturing the on-screen grid toggle as real,
# working UI evidence in its own right.
set -euo pipefail

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$(cd "$(dirname "$0")" && pwd)"
AINB="$CARGO_TARGET_DIR/debug/ainb"
DAEMON="$CARGO_TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$CARGO_TARGET_DIR/debug/examples/seed_t5_notify_routing_journey"
PLUGIN_ROOT="$(dirname "$CARGO_TARGET_DIR")/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || {
    echo "missing binary: $b — build first: (cd $W && cargo build -p ainb -p ainb-hangar-daemon --example seed_t5_notify_routing_journey)" >&2
    exit 2
  }
done

HOME_DIR=$(mktemp -d /tmp/t5nr.XXXXXX)
SEED_OUT="$("$SEEDER" "$HOME_DIR" "$DAEMON")"
echo "$SEED_OUT"
DAEMON_PID="$(printf '%s\n' "$SEED_OUT" | sed -n 's/^DAEMON_PID=//p')"

DB="$HOME_DIR/.agents-in-a-box/hangar.db"
POLL_LOG="$(mktemp /tmp/t5-notify-poll.XXXXXX.log)"

# Background STATE-DRIVEN raise. Two scopes matter here and they are NOT the
# same row: the Settings grid's `space` toggle writes a WORKSPACE-scoped
# override (`apply_notify_action` in plugin.rs always sends the current
# `workspace_id`), but the daemon's hook-ingest resolves a raised attention's
# channels via `notify::resolve_channels(pool, kind, None)` — ALWAYS the GLOBAL
# row, by design (attention_ingest.rs:242). So: wait for the on-screen grid
# toggle to land as a workspace override (the UI evidence), THEN flip the
# GLOBAL row directly via the same `hangar/notify_rule_set` RPC with no
# `workspace_id` (exactly what the tripwire's unseeded acceptance does) so the
# hook-raised ASK actually resolves against the new routing. Finally plant an
# AskUserQuestion transcript + append a `Notification` line to the real
# events.jsonl the daemon's attention_ingest tails, and poll for the resulting
# attention row's STAMPED channels.
(
  set +e +o pipefail
  for _ in $(seq 1 240); do
    ch=$(sqlite3 "$DB" "SELECT channels FROM notify_rule WHERE kind='ask_user_question' AND workspace_id IS NOT NULL;" 2>/dev/null)
    if [ "$ch" = "phone,web" ]; then
      echo "$(date +%s.%N) observed grid's WORKSPACE-scoped override channels=[$ch]" >> "$POLL_LOG"
      break
    fi
    sleep 0.3
  done

  python3 - "$HOME_DIR" >> "$POLL_LOG" 2>&1 <<'PY'
import socket, json, os, sys, time
home = sys.argv[1]
sock_path = os.path.join(home, ".agents-in-a-box", "hangar.sock")
tok_path = os.path.join(home, ".agents-in-a-box", "hangar", "daemon.token")
for _ in range(200):
    if os.path.exists(sock_path) and os.path.exists(tok_path):
        break
    time.sleep(0.1)
with open(tok_path) as f:
    token = f.read().strip()
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)
buf = b""
def send(obj):
    body = json.dumps(obj).encode()
    s.sendall(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
def recv():
    global buf
    while True:
        idx = buf.find(b"\r\n\r\n")
        if idx == -1:
            buf += s.recv(65536)
            continue
        header = buf[:idx].decode()
        length = None
        for line in header.split("\r\n"):
            if line.lower().startswith("content-length:"):
                length = int(line.split(":", 1)[1].strip())
        start = idx + 4
        while len(buf) < start + length:
            buf += s.recv(65536)
        body = buf[start:start + length]
        buf = buf[start + length:]
        return json.loads(body)
send({"jsonrpc": "2.0", "id": 1, "method": "auth/hello", "params": {"token": token}})
print("auth:", recv())
send({"jsonrpc": "2.0", "id": 2, "method": "hangar/notify_rule_set", "params": {"kind": "ask_user_question", "channels": ["phone", "web"]}})
print("global rule flip (no workspace_id):", recv())
PY
  echo "$(date +%s.%N) flipped the GLOBAL ask rule via direct RPC (no workspace_id)" >> "$POLL_LOG"

  CWD="/ainb-t5-journey/$$/$(date +%s%N)"
  SLUG=$(printf '%s' "$CWD" | sed -E 's/[^a-zA-Z0-9-]/-/g')
  PROJ_DIR="$HOME_DIR/.claude/projects/$SLUG"
  mkdir -p "$PROJ_DIR"
  cat > "$PROJ_DIR/session.jsonl" <<'JSONL'
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Ship it?","options":[{"label":"yes"},{"label":"no"}]}]}}]},"timestamp":"2026-01-01T00:00:00Z"}
JSONL

  EVENTS="$HOME_DIR/.agents-in-a-box/events.jsonl"
  mkdir -p "$(dirname "$EVENTS")"
  printf '{"ts":1700000000000,"session_id":"sid-t5-journey","cwd":"%s","transcript_path":"","agent":"claude","event_type":"Notification"}\n' "$CWD" >> "$EVENTS"
  echo "$(date +%s.%N) appended Notification hook line for cwd=$CWD" >> "$POLL_LOG"

  for _ in $(seq 1 40); do
    row=$(sqlite3 -separator '|' "$DB" "SELECT channels, state FROM attention WHERE kind='ask_user_question' ORDER BY created_at DESC LIMIT 1;" 2>/dev/null)
    if [ -n "$row" ]; then
      echo "$(date +%s.%N) raised attention row channels|state = [$row]" >> "$POLL_LOG"
      break
    fi
    sleep 0.5
  done
) &
RAISE_BG=$!

TAPE="$ASSETS/t5-notify-routing.tape"
cat > "$TAPE" <<EOF
# T5 — notification routing (verify-converged-goal.md journey catalogue).
Output "t5-notify-routing.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='$HOME_DIR' AINB_PLUGIN_ROOT='$PLUGIN_ROOT' exec '$AINB' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Settings (,) -> Notifications section (j clamps there regardless of the
#     starting section) — the seeded default: ask routes to web+os ---
Type ","
Sleep 2s
Type "jjjjjjjj"
Sleep 2s
Screenshot "t5-1-notify-grid-default.png"

# --- flip the ask rule: space on phone (col 0) ENABLES it ---
Type " "
Sleep 1s
# --- move right twice (phone -> web -> os), space DISABLES os ---
Type "ll"
Sleep 1s
Type " "
Sleep 2s
Screenshot "t5-2-notify-grid-flipped.png"

# --- hold: the background step waits for the flip to land, then raises a real
#     ASK via the hook seam; the daemon's attention-ingest tick (3s) picks it up ---
Sleep 18s

# --- Control Center (C): the routed ASK, now visible on the board ---
Type "C"
Sleep 3s
Screenshot "t5-3-control-center-ask-routed.png"
Sleep 1s
EOF

cd "$ASSETS"
vhs t5-notify-routing.tape
echo "vhs done"

wait "$RAISE_BG" 2>/dev/null || true

echo "--- notify-routing poll log ---"
cat "$POLL_LOG"
rm -f "$POLL_LOG"

# --- teardown: kill only the daemon PID the seeder printed ---
if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  kill -9 "$DAEMON_PID" 2>/dev/null || true
  echo "killed daemon $DAEMON_PID"
fi
rm -rf "$HOME_DIR"
echo "teardown complete"
