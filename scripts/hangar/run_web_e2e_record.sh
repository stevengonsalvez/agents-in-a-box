#!/usr/bin/env bash
# S1 (web ASK-answer, recorded) — verify-converged-goal.md journey catalogue.
#
# A headed + video-recorded variant of scripts/hangar/run_web_e2e.sh (the
# CI-gated CC18 leg, which proves this exact fixture headless). This script
# stands up the identical real stack — real daemon, real tmux delivery target,
# real `ainb web` server — then drives a HEADED chromium (video "on") through
# `playwright.config.record.ts` + `tests/ask-answer.record.spec.ts` so the
# browser answering the seeded ASK is captured on film, not just asserted.
#
# Produces, directly into docs/hangar/assets/journeys/:
#   - s1-web-answer.gif           (ffmpeg webm→gif, gifsicle -O3 --colors 64)
#   - s1-1-ask-card-rendered.png
#   - s1-2-card-flipped-gone.png
#   - s1-3-store-answered.png
#
# Self-contained + idempotent, same posture as run_web_e2e.sh: builds what it
# needs, provisions a short /tmp HOME, installs the Playwright chromium on
# demand, and tears everything down by EXACT name / PID only.
#
#   bash scripts/hangar/run_web_e2e_record.sh
set -o pipefail
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE="$REPO_ROOT/ainb-tui"
E2E_DIR="$WORKSPACE/crates/ainb-web/e2e"
JOURNEYS_DIR="$REPO_ROOT/docs/hangar/assets/journeys"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
TARGET_DIR="$CARGO_TARGET_DIR/debug"

AINB="$TARGET_DIR/ainb"
DAEMON="$TARGET_DIR/ainb-hangar-daemon"
SEEDER="$TARGET_DIR/examples/seed_control_center"

HOME_DIR=""
TARGET=""
WEB_PID=""
DAEMON_PID=""
SCREENSHOT_DIR=""

log() { printf '[web-e2e-record] %s\n' "$*"; }
die() { printf '[web-e2e-record] ERROR: %s\n' "$*" >&2; exit 2; }

cleanup() {
  local code=$?
  if [ "$code" -ne 0 ] && [ -n "$HOME_DIR" ] && [ -f "$HOME_DIR/web.log" ]; then
    printf '[web-e2e-record] --- last 20 lines of ainb web log ---\n' >&2
    tail -20 "$HOME_DIR/web.log" >&2
  fi
  [ -n "$WEB_PID" ] && kill "$WEB_PID" 2>/dev/null
  [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
  [ -n "$TARGET" ] && tmux kill-session -t "$TARGET" 2>/dev/null
  [ -n "$HOME_DIR" ] && rm -rf "$HOME_DIR"
  [ -n "$SCREENSHOT_DIR" ] && rm -rf "$SCREENSHOT_DIR"
  exit "$code"
}
trap cleanup EXIT INT TERM

command -v tmux    >/dev/null 2>&1 || die "tmux not found (required for the delivery target)"
command -v sqlite3 >/dev/null 2>&1 || die "sqlite3 not found (required to assert the answered row)"
command -v node    >/dev/null 2>&1 || die "node not found (required for Playwright)"
command -v npm     >/dev/null 2>&1 || die "npm not found (required for Playwright)"
command -v ffmpeg  >/dev/null 2>&1 || die "ffmpeg not found (required to convert the recorded video to a GIF)"
command -v gifsicle >/dev/null 2>&1 || die "gifsicle not found (required to compress the GIF)"

log "building ainb + daemon + seed_control_center (shared target: $CARGO_TARGET_DIR)"
( cd "$WORKSPACE" && cargo build -p ainb -p ainb-hangar-daemon --example seed_control_center ) \
  || die "cargo build failed"
for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || die "expected binary missing after build: $b"
done

log "installing e2e npm deps"
( cd "$E2E_DIR" && npm install --no-audit --no-fund ) || die "npm install failed"
log "ensuring the Playwright chromium is installed"
( cd "$E2E_DIR" && npx playwright install chromium ) || die "playwright install chromium failed"

HOME_DIR="$(mktemp -d /tmp/ccwr.XXXXXX)" || die "mktemp home failed"
TARGET="web-e2e-rec-target-$$-$(date +%s%N)"
FAKE_AINB="$HOME_DIR/fake-ainb.sh"
SCREENSHOT_DIR="$(mktemp -d /tmp/ccwr-shots.XXXXXX)" || die "mktemp screenshot dir failed"

cat > "$FAKE_AINB" <<EOF
#!/bin/sh
if [ "\$1" = "list" ] || [ "\$2" = "list" ] || [ "\$3" = "list" ]; then
  printf '%s' '[{"session_id":"s-deploy","tmux_session_name":"$TARGET","workspace_name":"deploy","worktree_path":"/work/deploy","created_at":"2026-07-01T00:00:00+00:00","is_running":true,"claude_active":true}]'
  exit 0
fi
printf '%s' '[]'
EOF
chmod +x "$FAKE_AINB"

tmux new-session -d -s "$TARGET" -x 80 -y 24 || die "could not create target tmux session"

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

PORT="$(( (RANDOM % 4000) + 4500 ))"
BEARER="e2e-rec-$(date +%s)-$RANDOM"
WEB_URL="http://127.0.0.1:$PORT"
log "starting ainb web on $WEB_URL"
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

log "running Playwright (HEADED chromium, video on)"
WEB_URL="$WEB_URL" \
WEB_TOKEN="$BEARER" \
TARGET_SESSION="$TARGET" \
HANGAR_HOME="$HOME_DIR" \
SCREENSHOT_DIR="$SCREENSHOT_DIR" \
  bash -c "cd '$E2E_DIR' && npx playwright test --config=playwright.config.record.ts"
RESULT=$?

if [ "$RESULT" -ne 0 ]; then
  log "FAILED — Playwright exited $RESULT"
  exit "$RESULT"
fi
log "GREEN — recorded web ASK-answer journey passed"

# ── locate the recorded video + assemble the GIF ─────────────────────────────
VIDEO="$(find "$E2E_DIR/test-results" -name '*.webm' -print -quit 2>/dev/null)"
[ -n "$VIDEO" ] && [ -f "$VIDEO" ] || die "no recorded .webm found under $E2E_DIR/test-results"
log "recorded video: $VIDEO"

mkdir -p "$JOURNEYS_DIR"
PALETTE="$SCREENSHOT_DIR/palette.png"
RAW_GIF="$SCREENSHOT_DIR/s1-web-answer.raw.gif"

ffmpeg -y -i "$VIDEO" -vf "fps=8,scale=1280:-1:flags=lanczos,palettegen" "$PALETTE" \
  || die "ffmpeg palettegen failed"
ffmpeg -y -i "$VIDEO" -i "$PALETTE" \
  -lavfi "fps=8,scale=1280:-1:flags=lanczos [x]; [x][1:v] paletteuse" \
  "$RAW_GIF" \
  || die "ffmpeg gif assembly failed"

gifsicle -O3 --colors 64 "$RAW_GIF" -o "$JOURNEYS_DIR/s1-web-answer.gif" \
  || die "gifsicle compression failed"
log "wrote $JOURNEYS_DIR/s1-web-answer.gif"

for shot in s1-1-ask-card-rendered.png s1-2-card-flipped-gone.png s1-3-store-answered.png; do
  [ -f "$SCREENSHOT_DIR/$shot" ] || die "expected still missing: $SCREENSHOT_DIR/$shot"
  cp "$SCREENSHOT_DIR/$shot" "$JOURNEYS_DIR/$shot"
  log "wrote $JOURNEYS_DIR/$shot"
done

# Clean up the raw Playwright test-results tree (video + trace scratch — the
# GIF + stills are the durable artifacts).
rm -rf "$E2E_DIR/test-results"

exit 0
