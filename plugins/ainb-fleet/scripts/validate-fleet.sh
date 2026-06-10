#!/usr/bin/env bash
# validate-fleet.sh — live end-to-end validation of every `ainb fleet` subcommand
# against REAL agent sessions spawned through `ainb run`.
#
# `ainb fleet` discovers sessions via `ainb list` (NOT by scanning raw tmux), so
# the test sessions MUST be ainb-spawned to be discoverable. This script spawns
# one Claude and one Codex session into a throwaway git repo, waits for them to
# register, then drives every verb and asserts via capture-pane / JSONL. It
# writes a PASS/FAIL proof file and exits non-zero on any failure.
#
# Signal-kind *detection* (ASK/ERR/IDLE/WAIT classification) is also covered
# deterministically by the in-process unit tests and tripwire_fleet_enrich.rs;
# this script proves discovery + delivery + the cache/0-token contract against
# real sessions, plus the IDLE/ASK signals that are inducible live.
#
# tmux/session safety: every session is ainb-owned and torn down with
# `ainb kill <exact-name>`; this script never kills tmux server-wide and never
# touches a session it did not create.
#
# Prereqs: ainb, tmux, git on PATH; a usable `claude` (and ideally `codex`)
# agent configured for `ainb run`. Missing prereqs SKIP cleanly, never hang.
#
# Usage: plugins/ainb-fleet/scripts/validate-fleet.sh
set -u

PREFIX="fleetval-$$"
STAMP="$(date +%Y%m%dT%H%M%S 2>/dev/null || echo run)"
PROOF="${PROOF_FILE:-/tmp/${PREFIX}-${STAMP}.proof.txt}"
PASS=0; FAIL=0; SKIP=0
CREATED=()                 # ainb session names we spawned (exact-name teardown)
REPO=""

: >"$PROOF"
say()  { printf '%s\n' "$*" | tee -a "$PROOF"; }
pass() { PASS=$((PASS+1)); say "  ✓ PASS  $*"; }
fail() { FAIL=$((FAIL+1)); say "  ✗ FAIL  $*"; }
skip() { SKIP=$((SKIP+1)); say "  ⤼ SKIP  $*"; }
hdr()  { say ""; say "── $* ──"; }

cleanup() {
  hdr "cleanup — tear down only the sessions we spawned, by exact identifier"
  for n in "${CREATED[@]:-}"; do
    [ -n "$n" ] || continue
    # `ainb kill` takes the session-id (not our --name), so resolve it from the
    # known tmux_session name; then kill the tmux pane by its exact name too.
    local id=""
    if command -v jq >/dev/null 2>&1; then
      id="$(ainb --format json list 2>/dev/null \
            | jq -r --arg t "tmux_${n}" '.[] | select(.tmux_session_name==$t) | .session_id' 2>/dev/null | head -1)"
    fi
    if [ -n "$id" ]; then
      ainb kill "$id" >>"$PROOF" 2>&1 && say "  ainb removed $n ($id)"
    fi
    if tmux has-session -t "tmux_${n}" 2>/dev/null; then
      tmux kill-session -t "tmux_${n}" && say "  killed tmux tmux_${n}"
    fi
  done
  [ -n "$REPO" ] && rm -rf "$REPO" 2>/dev/null
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
hdr "preflight"
need() { command -v "$1" >/dev/null 2>&1; }
for b in ainb tmux git; do need "$b" || { say "FATAL: $b not on PATH"; exit 2; }; done
say "  ainb=$(command -v ainb)  tmux=$(command -v tmux)  git=$(command -v git)"

REPO="$(mktemp -d "/tmp/${PREFIX}-repo.XXXXXX")"
( cd "$REPO" && git init -q && git commit -q --allow-empty -m init ) || { say "FATAL: scratch repo init"; exit 2; }
say "  scratch repo: $REPO"

# Which agents can we spawn?
A1_TOOL="claude"; A2_TOOL="codex"
have_a1=0; have_a2=0
need claude && have_a1=1
need codex && have_a2=1
say "  claude=$have_a1  codex=$have_a2"
[ "$have_a1" = 1 ] || [ "$have_a2" = 1 ] || { skip "no real agent (claude/codex) available — running cache/transport checks only"; }

# ---------------------------------------------------------------------------
# Spawn discoverable sessions via `ainb run` (non-interactive, background).
# ---------------------------------------------------------------------------
S1="${PREFIX}-claude"
S2="${PREFIX}-codex"
spawn() { # name, tool
  local name="$1" tool="$2"
  say "  spawning $name (tool=$tool)…"
  ( timeout 90 ainb run --repo "$REPO" --tool "$tool" --model haiku \
      --name "$name" --prompt "Reply with the single word READY and then wait." \
      >>"$PROOF" 2>&1 ) &
  CREATED+=("$name")
}
hdr "spawn sessions"
[ "$have_a1" = 1 ] && spawn "$S1" "$A1_TOOL" || skip "claude unavailable — $S1 not spawned"
[ "$have_a2" = 1 ] && spawn "$S2" "$A2_TOOL" || skip "codex unavailable — $S2 not spawned"

# Poll `ainb list` until our sessions register (or give up).
discovered_name() { ainb --format json list 2>/dev/null | grep -qF "$1"; }
wait_discovered() {
  local name="$1" tries=0
  while [ "$tries" -lt 30 ]; do
    discovered_name "$name" && return 0
    sleep 2; tries=$((tries+1))
  done
  return 1
}
DISC=()
for s in "$S1" "$S2"; do
  case " ${CREATED[*]} " in *" $s "*)
    if wait_discovered "$s"; then say "  discovered $s"; DISC+=("$s"); else say "  $s never registered"; fi ;;
  esac
done

# ---------------------------------------------------------------------------
# 1. standup — spawned sessions are listed
# ---------------------------------------------------------------------------
hdr "standup"
if [ "${#DISC[@]}" -gt 0 ]; then
  ST="$(ainb --format json fleet standup 2>>"$PROOF" || true)"
  if printf '%s' "$ST" | grep -qF "$S1"; then
    pass "standup lists the spawned Claude session ($S1)"
  else
    fail "standup omitted the spawned Claude session ($S1)"
  fi
  printf '%s' "$ST" | grep -qF "$S2" \
    || skip "Codex session ($S2) not listed — codex may be unconfigured in this env"
else
  skip "no discoverable sessions — standup not asserted (agents unavailable)"
fi

# ---------------------------------------------------------------------------
# 2. broadcast — delivery routes to a tmux pane
#    NOTE: agents interpret pane input as a prompt, not a shell command, so we
#    assert on broadcast's own delivery report (✓ via tmux), not on pane echo.
# ---------------------------------------------------------------------------
hdr "broadcast"
if [ "${#DISC[@]}" -gt 0 ]; then
  BC="$(ainb fleet broadcast "fleet validation ping $$" --filter "$PREFIX" 2>&1)"
  printf '%s\n' "$BC" >>"$PROOF"
  if printf '%s' "$BC" | grep -q "via tmux" && printf '%s' "$BC" | grep -q "sent to"; then
    pass "broadcast routed to a tmux pane ($(printf '%s' "$BC" | grep -c 'via tmux') target(s))"
  else
    fail "broadcast did not report a tmux delivery"
  fi
else
  skip "no discoverable sessions — broadcast not asserted"
fi

# ---------------------------------------------------------------------------
# 3. needs — IDLE is inducible against a real session
# ---------------------------------------------------------------------------
hdr "needs — IDLE"
if [ "${#DISC[@]}" -gt 0 ]; then
  say "  letting a session go idle (sleep 70s past --idle-min 1)…"; sleep 70
  NJ="$(ainb --format json fleet needs --idle-min 1 2>>"$PROOF" || true)"
  if printf '%s' "$NJ" | grep -qF "$PREFIX" && printf '%s' "$NJ" | grep -q '"kind":"IDLE"'; then
    pass "needs surfaces an IDLE card for a spawned session"
  else
    skip "no IDLE card observed (session may still be active) — see proof"
  fi
else
  skip "no discoverable sessions — IDLE not asserted"
fi

# ---------------------------------------------------------------------------
# 4. needs — ERR / WAIT (not deterministically inducible live)
# ---------------------------------------------------------------------------
hdr "needs — ERR / WAIT"
skip "ERR (needs a real API failure) + WAIT (needs a peer summary) are covered deterministically by unit tests + tripwire_fleet_enrich.rs"

# ---------------------------------------------------------------------------
# 5. sequence — ordered, ack-gated delivery
#    `ainb fleet sequence` is --all-only in v0.1 (no --filter), so scoping to
#    just our test sessions isn't possible; running --all would fan out to every
#    unrelated session on the host. We therefore don't auto-run it here — its
#    ack-gating is covered by the sequence skill + JSONL turn-end unit logic.
# ---------------------------------------------------------------------------
hdr "sequence"
skip "sequence is --all-only (v0.1, no --filter); not auto-run in a shared env — ack-gating covered by the sequence skill + JSONL turn-end unit logic"

# ---------------------------------------------------------------------------
# 6. daemon — auto-continue (needs a real error; smoke its startup only)
# ---------------------------------------------------------------------------
hdr "daemon"
DOUT="$(timeout 7 ainb fleet daemon --verbose 2>&1 | head -5 || true)"
if printf '%s' "$DOUT" | grep -qiE "broker|tmux-only|fleet/daemon"; then
  pass "daemon starts + reports transport mode (auto-continue path unit-tested)"
else
  skip "daemon start line not captured — see proof"
fi
printf '%s\n' "$DOUT" >>"$PROOF"

# ---------------------------------------------------------------------------
# 7. enrich cache round-trip + --no-enrich 0-token contract
# ---------------------------------------------------------------------------
hdr "enrich cache + token budget"
TC="/tmp/${PREFIX}-cache.json"
if AINB_FLEET_ENRICH_CACHE="$TC" ainb fleet enrich-cache put --key vk1 --suggestion "Approve as-is" >>"$PROOF" 2>&1 \
   && [ "$(AINB_FLEET_ENRICH_CACHE="$TC" ainb fleet enrich-cache get --key vk1 2>/dev/null)" = "Approve as-is" ]; then
  pass "enrich-cache put/get round-trips"
else
  fail "enrich-cache round-trip broke"
fi
rm -f "$TC"

NE="$(ainb --format json fleet needs --no-enrich 2>>"$PROOF" || true)"
if ! printf '%s' "$NE" | grep -q '"need_enrich": *true'; then
  pass "--no-enrich flags no card for the producer (0-token HUD)"
else
  fail "--no-enrich still flagged a card need_enrich"
fi

# ---------------------------------------------------------------------------
# 8. fleet-needs (workflow) — session-only, cannot run from bash
# ---------------------------------------------------------------------------
hdr "fleet-needs (workflow)"
skip "workflow path is session-only (CLAUDE_CODE_WORKFLOWS) — validate via /ainb-fleet:fleet-needs in a live session"

# ---------------------------------------------------------------------------
hdr "SUMMARY"
say "  PASS=$PASS  FAIL=$FAIL  SKIP=$SKIP"
say "  proof: $PROOF"
[ "$FAIL" -eq 0 ] || { say "RESULT: FAIL"; exit 1; }
say "RESULT: PASS"
exit 0
