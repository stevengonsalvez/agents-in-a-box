#!/usr/bin/env bash
# C2 web ASK-answer Playwright leg (spec P8 / verify-converged web pillar).
#
# Stands up the REAL converged stack and drives a REAL browser against it to
# prove the web dashboard can answer an AskUserQuestion end to end:
#
#   ┌────────┐   seed ASK    ┌────────┐   attention/*   ┌──────────┐  click ②  ┌─────────┐
#   │ seeder │──(att-ask-1)─▶│ daemon │◀───RPC (D18)────│ ainb web │◀──────────│ chromium│
#   └────────┘  3 options    └───┬────┘                 └──────────┘  browser   └─────────┘
#                    verified send │  (tmux-only)
#                                  ▼
#                          ┌───────────────┐
#                          │ target tmux   │  capture-pane → "2"
#                          │ session (real)│
#                          └───────────────┘
#
# The daemon's answer router resolves a LIVE delivery target before it flips a
# row to `answered`, so the seeded ASK's raising session (`s-deploy`) is wired to
# a REAL tmux session via a fake `ainb list` (AINB_BIN) + tmux-only transport —
# the exact technique from docs/hangar/assets/record-control-center.sh. Pressing
# option ② in the browser therefore genuinely delivers "2" into that pane and the
# row flips to `answered(by=web)`.
#
# Self-contained + idempotent: builds what it needs, provisions a short /tmp HOME
# (unix-socket 104-char limit), installs the Playwright chromium on demand, runs
# headless, and tears everything down by EXACT name / PID only.
#
#   bash scripts/hangar/run_web_e2e.sh
set -o pipefail
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE="$REPO_ROOT/ainb-tui"
E2E_DIR="$WORKSPACE/crates/ainb-web/e2e"

# Shared target dir keeps this in step with the rest of the ccc build (avoids a
# from-scratch rebuild). Override by exporting CARGO_TARGET_DIR before the run.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
TARGET_DIR="$CARGO_TARGET_DIR/debug"

AINB="$TARGET_DIR/ainb"
DAEMON="$TARGET_DIR/ainb-hangar-daemon"
SEEDER="$TARGET_DIR/examples/seed_control_center"

# State populated during provisioning; the trap reads these to tear down exactly
# what we created (never a wildcard / by-name process kill).
HOME_DIR=""
TARGET=""
WEB_PID=""
DAEMON_PID=""

log() { printf '[web-e2e] %s\n' "$*"; }
die() { printf '[web-e2e] ERROR: %s\n' "$*" >&2; exit 2; }

cleanup() {
  local code=$?
  # On a failing run, surface the server log before we delete the home dir.
  if [ "$code" -ne 0 ] && [ -n "$HOME_DIR" ] && [ -f "$HOME_DIR/web.log" ]; then
    printf '[web-e2e] --- last 20 lines of ainb web log ---\n' >&2
    tail -20 "$HOME_DIR/web.log" >&2
  fi
  [ -n "$WEB_PID" ] && kill "$WEB_PID" 2>/dev/null
  [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
  [ -n "$TARGET" ] && tmux kill-session -t "$TARGET" 2>/dev/null
  [ -n "$HOME_DIR" ] && rm -rf "$HOME_DIR"
  exit "$code"
}
trap cleanup EXIT INT TERM

# ── prerequisites ────────────────────────────────────────────────────────────
command -v tmux    >/dev/null 2>&1 || die "tmux not found (required for the delivery target)"
command -v sqlite3 >/dev/null 2>&1 || die "sqlite3 not found (required to assert the answered row)"
command -v node    >/dev/null 2>&1 || die "node not found (required for Playwright)"
command -v npm     >/dev/null 2>&1 || die "npm not found (required for Playwright)"

# ── build ────────────────────────────────────────────────────────────────────
log "building ainb + daemon + seed_control_center (shared target: $CARGO_TARGET_DIR)"
( cd "$WORKSPACE" && cargo build -p ainb -p ainb-hangar-daemon --example seed_control_center ) \
  || die "cargo build failed"
for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || die "expected binary missing after build: $b"
done

# ── Playwright deps (idempotent) ─────────────────────────────────────────────
log "installing e2e npm deps"
( cd "$E2E_DIR" && npm install --no-audit --no-fund ) || die "npm install failed"
log "ensuring the Playwright chromium is installed"
( cd "$E2E_DIR" && npx playwright install chromium ) || die "playwright install chromium failed"

# ── provision the isolated stack ─────────────────────────────────────────────
# Short HOME so {home}/.agents-in-a-box/hangar.sock stays under the 104-char
# unix-socket path limit.
HOME_DIR="$(mktemp -d /tmp/ccw.XXXXXX)" || die "mktemp home failed"
TARGET="web-e2e-target-$$-$(date +%s%N)"
FAKE_AINB="$HOME_DIR/fake-ainb.sh"

# Fake `ainb list --format json`: reports the seeded ASK's raising session
# (`s-deploy`) as a live session bound to the REAL target tmux session, so the
# daemon's C1 target resolve finds an exact-id match and can deliver.
cat > "$FAKE_AINB" <<EOF
#!/bin/sh
if [ "\$1" = "list" ] || [ "\$2" = "list" ] || [ "\$3" = "list" ]; then
  printf '%s' '[{"session_id":"s-deploy","tmux_session_name":"$TARGET","workspace_name":"deploy","worktree_path":"/work/deploy","created_at":"2026-07-01T00:00:00+00:00","is_running":true,"claude_active":true}]'
  exit 0
fi
printf '%s' '[]'
EOF
chmod +x "$FAKE_AINB"

# Real delivery-target tmux session (a plain shell pane accepts the send-keys).
tmux new-session -d -s "$TARGET" -x 80 -y 24 || die "could not create target tmux session"

# Seed the DB (P4 fixture + WAIT + 3-option ASK) and spawn the daemon detached.
# The seeder prints HOME=… and DAEMON_PID=… on success and exits leaving the
# daemon alive.
log "seeding daemon (3-option ASK) under $HOME_DIR"
SEED_OUT="$("$SEEDER" "$HOME_DIR" "$DAEMON" "$FAKE_AINB" 2>&1)" || {
  printf '%s\n' "$SEED_OUT" >&2
  die "seeder failed"
}
DAEMON_PID="$(printf '%s\n' "$SEED_OUT" | sed -n 's/^DAEMON_PID=//p' | head -1)"
[ -n "$DAEMON_PID" ] || die "seeder did not report a DAEMON_PID:\n$SEED_OUT"
log "daemon pid $DAEMON_PID"

SOCK="$HOME_DIR/.agents-in-a-box/hangar.sock"
TOKEN_FILE="$HOME_DIR/.agents-in-a-box/hangar/daemon.token"
for _ in $(seq 1 60); do
  [ -S "$SOCK" ] && [ -f "$TOKEN_FILE" ] && break
  sleep 0.25
done
[ -S "$SOCK" ] || die "daemon socket never appeared at $SOCK"
[ -f "$TOKEN_FILE" ] || die "daemon token never written at $TOKEN_FILE"

# ── start the web dashboard against the seeded daemon ────────────────────────
PORT="$(( (RANDOM % 4000) + 4500 ))"
BEARER="e2e-$(date +%s)-$RANDOM"
WEB_URL="http://127.0.0.1:$PORT"
log "starting ainb web on $WEB_URL"
# HOME points the web server's daemon client at the seeded socket; AINB_BIN
# feeds the sessions panel from the same fake so nothing re-shells a real ainb.
# AINB_HANGAR_HOME is forced empty so the shared resolver falls back to HOME.
AINB_HANGAR_HOME= HOME="$HOME_DIR" AINB_BIN="$FAKE_AINB" \
  nohup "$AINB" web --listen "127.0.0.1:$PORT" --token "$BEARER" \
  >"$HOME_DIR/web.log" 2>&1 &
WEB_PID=$!

for _ in $(seq 1 60); do
  if curl -sf -o /dev/null "$WEB_URL/"; then break; fi
  kill -0 "$WEB_PID" 2>/dev/null || die "ainb web exited during startup (see log above)"
  sleep 0.25
done
curl -sf -o /dev/null "$WEB_URL/" || die "ainb web never became reachable on $WEB_URL"
log "web dashboard is up"

# ── run the browser journey ──────────────────────────────────────────────────
log "running Playwright (headless chromium)"
WEB_URL="$WEB_URL" \
WEB_TOKEN="$BEARER" \
TARGET_SESSION="$TARGET" \
HANGAR_HOME="$HOME_DIR" \
  bash -c "cd '$E2E_DIR' && npx playwright test"
RESULT=$?

if [ "$RESULT" -eq 0 ]; then
  log "GREEN — web ASK-answer journey passed"
else
  log "FAILED — Playwright exited $RESULT"
fi
exit "$RESULT"
