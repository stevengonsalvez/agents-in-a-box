#!/usr/bin/env bash
# Self-contained test for toolkit/copilot/statusline.sh.
#
# Feeds a SYNTHETIC copilot-shaped session JSON (the live `copilot` binary is
# org-policy-blocked, so we never invoke it) and asserts the rendered line:
#   1. is exactly one line
#   2. contains the model short name        (proves model remap)
#   3. contains a context-% indicator        (proves ctx-window remap)
#   4. contains the current git branch        (proves field-agnostic git block)
#   5. does NOT contain a "$" USD cost segment (proves copilot-specific omission:
#      duration is rendered from total_duration_ms, never a dollar cost)
#
# Run from anywhere:  bash toolkit/copilot/test_statusline.sh
# Forced bash 3.2:    /bin/bash toolkit/copilot/test_statusline.sh   (macOS)

set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
STATUSLINE="$SCRIPT_DIR/statusline.sh"

# Resolve the repo's git branch from the statusline script's own location so the
# branch assertion is meaningful regardless of the caller's cwd.
EXPECTED_BRANCH=$(git -C "$SCRIPT_DIR" symbolic-ref --short HEAD 2>/dev/null \
  || git -C "$SCRIPT_DIR" rev-parse --short HEAD 2>/dev/null \
  || echo "")

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$1"; }

[[ -x "$STATUSLINE" ]] || fail "statusline.sh not executable at $STATUSLINE"

# ── Synthetic copilot session JSON ────────────────────────────────────────────
# Realistic values for every confirmed Copilot field. cwd points at the repo
# root so the git branch renders. Deliberately OMITS cost.total_cost_usd and
# rate_limits (no copilot equivalent) so the "$"-absence assertion is meaningful.
SYNTHETIC_JSON=$(cat <<JSON
{
  "cwd": "$SCRIPT_DIR",
  "session_id": "11111111-2222-3333-4444-555555555555",
  "version": "1.0.62",
  "model": {
    "id": "claude-sonnet-4.6",
    "display_name": "Claude Sonnet 4.6"
  },
  "context_window": {
    "current_context_tokens": 84210,
    "current_context_used_percentage": 42.1,
    "context_window_size": 200000
  },
  "cost": {
    "total_duration_ms": 754000,
    "total_api_duration_ms": 612000
  },
  "remote": {
    "connected": true,
    "indicator": "remote↗"
  }
}
JSON
)

# ── Render ────────────────────────────────────────────────────────────────────
RAW=$(printf '%s' "$SYNTHETIC_JSON" | "$STATUSLINE")
# Strip ANSI / powerline glyphs so visible-text assertions are robust.
PLAIN=$(printf '%s' "$RAW" | sed $'s/\033\\[[0-9;]*m//g')

printf '\n--- rendered line (raw, with colour) ---\n%s\n' "$RAW"
printf '\n--- rendered line (plain text) ---\n%s\n\n' "$PLAIN"

# 1. exactly one line.
# The script emits one trailing-newline-terminated line; command substitution
# above strips that single trailing newline, so a correct single-line render
# leaves RAW with ZERO embedded newlines. Assert RAW is non-empty and contains
# no embedded newline (i.e. the statusline never spilled onto a second line).
[[ -n "$RAW" ]] || fail "output is empty"
EMBEDDED_NL=$(printf '%s' "$RAW" | wc -l | tr -d ' ')
[[ "$EMBEDDED_NL" == "0" ]] || fail "expected exactly 1 line, found $((EMBEDDED_NL + 1)) lines"
pass "output is exactly one line"

# 2. contains the model short name (Claude Sonnet 4.6 → sonnet-4.6)
case "$PLAIN" in
  *"sonnet-4.6"*) pass "contains model short name (sonnet-4.6)" ;;
  *) fail "model short name 'sonnet-4.6' not in output" ;;
esac

# 3. contains a context-% indicator (the bar renders "NN%")
case "$PLAIN" in
  *"ctx "*"%"*) pass "contains context-% indicator (ctx ... %)" ;;
  *) fail "context-% indicator not in output" ;;
esac
# And specifically the rounded 42% from current_context_used_percentage=42.1
case "$PLAIN" in
  *"42%"*) pass "context % is the remapped 42% (from current_context_used_percentage)" ;;
  *) fail "expected '42%' from current_context_used_percentage=42.1" ;;
esac

# 4. contains the current git branch (this is a real git worktree)
if [[ -n "$EXPECTED_BRANCH" ]]; then
  case "$PLAIN" in
    *"$EXPECTED_BRANCH"*) pass "contains git branch ($EXPECTED_BRANCH)" ;;
    *) fail "git branch '$EXPECTED_BRANCH' not in output" ;;
  esac
else
  printf 'SKIP: not in a git repo, branch assertion skipped\n'
fi

# 5. does NOT contain a "$" USD cost segment (copilot-specific omission)
case "$RAW" in
  *'$'*) fail "output contains '\$' — USD cost segment should be omitted for copilot" ;;
  *) pass "no '\$' USD cost segment (copilot omission proven)" ;;
esac

# Bonus: duration rendered from total_duration_ms (754000ms → 12m 34s)
case "$PLAIN" in
  *"12m 34s"*) pass "duration rendered from total_duration_ms (12m 34s)" ;;
  *) fail "expected duration '12m 34s' from total_duration_ms=754000" ;;
esac

# Bonus: remote indicator rendered
case "$PLAIN" in
  *"remote↗"*) pass "remote indicator rendered (remote↗)" ;;
  *) fail "expected remote indicator 'remote↗'" ;;
esac

printf '\nALL ASSERTIONS PASSED\n'
