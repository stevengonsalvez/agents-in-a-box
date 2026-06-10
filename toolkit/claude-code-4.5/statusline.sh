#!/usr/bin/env bash
# ~/.claude/statusline.sh — Claude Code rich two-line statusline
# Line 1: model · cwd · git branch+changes · session health · beads
# Line 2: ctx bar · 5h bar · wk bar · cost
#
# Reads JSON session state from stdin (Claude Code schema).
# All slow external commands are timeout-guarded (500ms) with silent fallbacks.
# External results cached 10s in /tmp to keep render fast.

set -o pipefail

# ── ANSI colour codes ────────────────────────────────────────────────────────
RESET='\033[0m'
BOLD='\033[1m'

FG_GREEN='\033[32m'
FG_YELLOW='\033[33m'
FG_RED='\033[31m'
FG_CYAN='\033[36m'
FG_BLUE='\033[34m'
FG_MAGENTA='\033[35m'
FG_GREY='\033[90m'

SEP="${FG_GREY}·${RESET}"

# ── 10-second cache for slow commands ────────────────────────────────────────
CACHE_FILE="/tmp/claude-statusline-cache-${USER}.json"
CACHE_TTL=10

_cache_age() {
  [[ ! -f "$CACHE_FILE" ]] && echo 9999 && return
  local now mtime
  now=$(date +%s)
  mtime=$(stat -f %m "$CACHE_FILE" 2>/dev/null || stat -c %Y "$CACHE_FILE" 2>/dev/null || echo 0)
  echo $(( now - mtime ))
}

# Read a top-level key (string) from cache JSON
_cache_get() {
  [[ ! -f "$CACHE_FILE" ]] && return
  jq -r --arg k "$1" '.[$k] // empty' "$CACHE_FILE" 2>/dev/null
}

# Write a top-level key into cache JSON (value as raw string)
_cache_set() {
  local key=$1 val=$2 tmp existing
  tmp=$(mktemp)
  existing="{}"
  [[ -f "$CACHE_FILE" ]] && existing=$(cat "$CACHE_FILE" 2>/dev/null || echo "{}")
  printf '%s' "$existing" | jq --arg k "$key" --arg v "$val" '.[$k] = $v' > "$tmp" 2>/dev/null \
    && mv "$tmp" "$CACHE_FILE" || rm -f "$tmp"
}

# ── Unicode progress bar ──────────────────────────────────────────────────────
# Usage: _bar <percent 0-100> [amber_at=60] [red_at=85]
# RAG: green < amber_at, amber ≤ pct < red_at, red ≥ red_at (6 blocks wide)
_bar() {
  local pct=${1:-0}
  local amber=${2:-60}
  local red=${3:-85}
  local total=6
  local filled=$(( pct * total / 100 ))
  (( filled > total )) && filled=$total
  local empty=$(( total - filled ))
  local col
  if   (( pct < amber )); then col=$FG_GREEN
  elif (( pct < red   )); then col=$FG_YELLOW
  else                         col=$FG_RED
  fi
  local bar="" i
  for (( i=0; i<filled; i++ )); do bar+="█"; done
  for (( i=0; i<empty;  i++ )); do bar+="░"; done
  printf "${col}[${bar}] ${pct}%%${RESET}"
}

# ── Format a Unix epoch as local time ─────────────────────────────────────────
# Usage: _fmt_epoch <epoch-seconds> <strftime-fmt>. Empty on bad input.
# Handles BSD `date -r` (macOS) and GNU `date -d @` (Linux).
_fmt_epoch() {
  [[ "$1" =~ ^[0-9]+$ ]] || return
  date -r "$1" +"$2" 2>/dev/null || date -d "@$1" +"$2" 2>/dev/null || true
}

# ── timeout wrapper ───────────────────────────────────────────────────────────
# Prefer `timeout`, fall back to coreutils' `gtimeout`, else run with no limit.
# macOS Homebrew installs coreutils' timeout as `gtimeout` and doesn't symlink
# `timeout` unless gnubin is on PATH — without this the guarded segments below
# would silently blank on a stock mac.
_to() {
  if command -v timeout >/dev/null 2>&1; then timeout "$@"
  elif command -v gtimeout >/dev/null 2>&1; then gtimeout "$@"
  else shift; "$@"; fi
}

# ── Count un-acked reflect errors ─────────────────────────────────────────────
# Ladder, fastest → installed CLI. Replaces the old bare
# `python3 -m reflect_kb.errors count`, which only worked when reflect_kb was
# importable by *system* python3 — it usually isn't (it lives in the uv tool
# venv from `uv tool install reflect-kb`), so the badge was dead on every box.
#   1. jq-direct read of the sink       — no python, no reflect_kb needed (hot path)
#   2. installed `reflect errors count`  — from `uv tool install reflect-kb`
#   3. give up → 0
_reflect_err_count() {
  local f="$HOME/.reflect/errors.json"
  if [[ -f "$f" ]] && command -v jq >/dev/null 2>&1; then
    jq -r '[.errors[]? | select(.acked != true)] | length' "$f" 2>/dev/null && return
  fi
  if command -v reflect >/dev/null 2>&1; then
    reflect errors count 2>/dev/null && return
  fi
  echo 0
}

# ── Reflect error indicator ───────────────────────────────────────────────────
# Output: empty when 0, "⚠N" in red when >0. Cached 10s.
_reflect_errors() {
  local cached_val cached_age
  cached_val=$(_cache_get reflect_err)
  cached_age=$(_cache_age)
  if [[ -n "$cached_val" ]] && [[ "$cached_age" -lt "$CACHE_TTL" ]]; then
    printf '%s' "$cached_val"
    return
  fi
  local count
  count=$(_reflect_err_count)
  count=${count:-0}
  local out=""
  if [[ "$count" =~ ^[0-9]+$ ]] && [[ "$count" -gt 0 ]]; then
    out=$(printf "${FG_RED}${BOLD}⚠%s${RESET}" "$count")
  fi
  _cache_set reflect_err "$out"
  printf '%s' "$out"
}

# ── Read JSON from stdin once ─────────────────────────────────────────────────
INPUT=$(cat)
_jq() { printf '%s' "$INPUT" | jq -r "${1} // empty" 2>/dev/null; }

# Side-channel: feed ainb-tui's Live Window cache so it can surface the
# OAuth-grade 5h/7d rate-limit window. Background, silent, never blocks render.
if command -v ainb >/dev/null 2>&1; then
  printf '%s' "$INPUT" | ainb claudecode statusline --cache-only >/dev/null 2>&1 &
  disown 2>/dev/null || true
fi

CACHE_AGE=$(_cache_age)

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 1 — Model short name
# ════════════════════════════════════════════════════════════════════════════
MODEL_DISPLAY=$(_jq '.model.display_name')
MODEL_ID=$(_jq '.model.id')

if [[ -n "$MODEL_DISPLAY" ]]; then
  # "Claude Opus 4.7" → "opus-4.7",  "Claude Sonnet 4.6" → "sonnet-4.6"
  MODEL_SHORT=$(printf '%s' "$MODEL_DISPLAY" | awk '{
    n = split(tolower($0), a, " ")
    tier = ""; ver = ""
    for (i=1; i<=n; i++) {
      if (a[i] ~ /haiku|sonnet|opus/) tier = a[i]
      if (a[i] ~ /^[0-9]/)           ver  = a[i]
    }
    if (tier != "" && ver != "") print tier "-" ver
    else print tolower($0)
  }')
elif [[ -n "$MODEL_ID" ]]; then
  MODEL_SHORT=$(printf '%s' "$MODEL_ID" | sed 's/^claude-//' | sed 's/\[.*\]$//')
else
  MODEL_SHORT="${CLAUDE_MODEL:-unknown}"
fi

# Context window variant tag (1M). Check id "[1m]" suffix OR display "(1M context)".
CTX_VARIANT=""
if printf '%s' "$MODEL_ID" | grep -qi '\[1m\]'; then
  CTX_VARIANT="·1m"
elif printf '%s' "$MODEL_DISPLAY" | grep -qi '1m'; then
  CTX_VARIANT="·1m"
fi
MODEL_SHORT="${MODEL_SHORT}${CTX_VARIANT}"

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 1b — Reasoning effort + fast mode (native Claude Code stdin fields)
#   .effort.level ∈ low|medium|high|xhigh   ·   .fast_mode bool   ·   .thinking.enabled bool
# Rendered compactly on Line 2 next to the model. Empty if field absent.
# ════════════════════════════════════════════════════════════════════════════
EFFORT_DISPLAY=""
EFFORT_LEVEL=$(_jq '.effort.level')
if [[ -n "$EFFORT_LEVEL" ]]; then
  case "$EFFORT_LEVEL" in
    low)    EFFORT_TAG="lo"   ; EFFORT_FG=$FG_GREY   ;;
    medium) EFFORT_TAG="med"  ; EFFORT_FG=$FG_GREEN  ;;
    high)   EFFORT_TAG="high" ; EFFORT_FG=$FG_YELLOW ;;
    xhigh)  EFFORT_TAG="xhi"  ; EFFORT_FG=$FG_MAGENTA;;
    *)      EFFORT_TAG="$EFFORT_LEVEL" ; EFFORT_FG=$FG_CYAN ;;
  esac
  # ⚡ appended when fast-mode is on (Opus faster-output toggle).
  FAST_FLAG=""
  [[ "$(_jq '.fast_mode')" == "true" ]] && FAST_FLAG="⚡"
  EFFORT_DISPLAY="eff ${EFFORT_FG}${BOLD}${EFFORT_TAG}${RESET}${FAST_FLAG}"
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 2 — Working directory (~ collapsed)
# ════════════════════════════════════════════════════════════════════════════
CWD=$(_jq '.workspace.current_dir')
[[ -z "$CWD" ]] && CWD=$(_jq '.cwd')
[[ -z "$CWD" ]] && CWD=$(pwd 2>/dev/null || echo "?")
CWD_DISPLAY="${CWD/#$HOME/\~}"

# Adaptive shortening: narrow terminals wrap long cwd and eat line 2.
# Detect terminal width (Claude Code stdin may expose it; fallback to tput/stty).
TERM_COLS=$(_jq '.terminal.width')
[[ -z "$TERM_COLS" || "$TERM_COLS" -lt 20 ]] && TERM_COLS=$(tput cols 2>/dev/null || echo 120)

_shorten_path() {
  local p=$1
  local keep=$2   # number of trailing segments to keep
  # Split on /, keep last N
  local IFS='/'; read -r -a parts <<< "$p"
  local n=${#parts[@]}
  (( n <= keep )) && { printf '%s' "$p"; return; }
  local tail="" i
  for (( i=n-keep; i<n; i++ )); do tail+="/${parts[i]}"; done
  printf '…%s' "$tail"
}

# Shorten cwd adaptively by terminal width. Tighter limits at narrower widths.
CWD_LEN=${#CWD_DISPLAY}
if   (( TERM_COLS < 80 ))  && (( CWD_LEN > 25 )); then
  CWD_DISPLAY=$(_shorten_path "$CWD_DISPLAY" 1)
elif (( TERM_COLS < 120 )) && (( CWD_LEN > 40 )); then
  CWD_DISPLAY=$(_shorten_path "$CWD_DISPLAY" 2)
elif                            (( CWD_LEN > 80 )); then
  CWD_DISPLAY=$(_shorten_path "$CWD_DISPLAY" 2)
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 3 — Git: branch, ahead/behind, staged+unstaged, untracked
# ════════════════════════════════════════════════════════════════════════════
GIT_BRANCH=""
GIT_AHEAD_BEHIND=""
GIT_CHANGES=""

if git -C "$CWD" rev-parse --git-dir &>/dev/null 2>&1; then
  GIT_BRANCH=$(_to 0.5 git -C "$CWD" symbolic-ref --short HEAD 2>/dev/null \
    || _to 0.5 git -C "$CWD" rev-parse --short HEAD 2>/dev/null \
    || true)

  if [[ -n "$GIT_BRANCH" ]]; then
    REMOTE_INFO=$(_to 0.5 git -C "$CWD" rev-list --left-right --count \
      "@{upstream}...HEAD" 2>/dev/null || true)
    if [[ -n "$REMOTE_INFO" ]]; then
      BEHIND=$(awk '{print $1}' <<< "$REMOTE_INFO")
      AHEAD=$(awk '{print $2}'  <<< "$REMOTE_INFO")
      (( AHEAD > 0 || BEHIND > 0 )) && GIT_AHEAD_BEHIND=" ↑${AHEAD}↓${BEHIND}"
    fi
  fi

  STATUS_OUTPUT=$(_to 0.5 git -C "$CWD" status --porcelain 2>/dev/null || true)
  if [[ -n "$STATUS_OUTPUT" ]]; then
    # Staged: lines where col1 is not space or '?'
    STAGED=$(printf '%s\n' "$STATUS_OUTPUT" | awk '$0 !~ /^[ ?]/ {n++} END {print n+0}')
    # Unstaged: lines where col2 is not space or '?'  (col2 = char at index 2)
    UNSTAGED=$(printf '%s\n' "$STATUS_OUTPUT" | awk 'substr($0,2,1) !~ /[ ?]/ {n++} END {print n+0}')
    UNTRACKED=$(printf '%s\n' "$STATUS_OUTPUT" | grep -c '^\?\?' 2>/dev/null || echo 0)
    DELTA=$(( STAGED + UNSTAGED ))
    [[ $DELTA    -gt 0 ]] && GIT_CHANGES="±${DELTA}"
    [[ $UNTRACKED -gt 0 ]] && GIT_CHANGES="${GIT_CHANGES:+${GIT_CHANGES} }?${UNTRACKED}"
  fi
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 4 — Session health from transcript message count
# ════════════════════════════════════════════════════════════════════════════
TRANSCRIPT_PATH=$(_jq '.transcript_path')
MSG_COUNT=0
if [[ -n "$TRANSCRIPT_PATH" && -f "$TRANSCRIPT_PATH" ]]; then
  # Count real user turns: role=user AND content is not a tool_result.
  # Falls back to grep if jq fails.
  MSG_COUNT=$(_to 0.5 jq -r --slurp '
    [.[] | select(
      (.message.role // .role) == "user"
      and ((.message.content // .content | tostring) | contains("tool_result") | not)
    )] | length
  ' "$TRANSCRIPT_PATH" 2>/dev/null) || MSG_COUNT=0
  [[ -z "$MSG_COUNT" || "$MSG_COUNT" == "null" ]] && MSG_COUNT=0
fi

if   (( MSG_COUNT <= 30 )); then HEALTH_EMOJI="🟢"
elif (( MSG_COUNT <= 45 )); then HEALTH_EMOJI="🟡"
else                              HEALTH_EMOJI="🔴"
fi
HEALTH_DISPLAY="${HEALTH_EMOJI} ${MSG_COUNT}/50"

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 5 — Beads ready count (bd) — cached
# ════════════════════════════════════════════════════════════════════════════
BD_DISPLAY=""
if (( CACHE_AGE < CACHE_TTL )); then
  BD_COUNT=$(_cache_get "bd")
else
  BD_COUNT=$(_to 0.5 bash -c 'bd ready --json 2>/dev/null | jq "length" 2>/dev/null' || true)
  [[ -n "$BD_COUNT" ]] && _cache_set "bd" "$BD_COUNT"
fi
if [[ "$BD_COUNT" =~ ^[0-9]+$ ]] && (( BD_COUNT > 0 )); then
  BD_DISPLAY="${FG_CYAN}bd:${BD_COUNT}▸${RESET}"
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 6 — Context window used %
# ════════════════════════════════════════════════════════════════════════════
CTX_PCT=0
CTX_USED_PCT=$(_jq '.context_window.used_percentage')

if [[ -n "$CTX_USED_PCT" ]]; then
  CTX_PCT=$(printf "%.0f" "$CTX_USED_PCT" 2>/dev/null || echo 0)
else
  # Fallback via raw token counts
  INPUT_TOKENS=$(_jq '.context_window.current_usage.input_tokens')
  CTX_WINDOW_SIZE=$(_jq '.context_window.context_window_size')
  [[ -z "$CTX_WINDOW_SIZE" || "$CTX_WINDOW_SIZE" == "0" ]] && CTX_WINDOW_SIZE=200000
  EXCEEDS=$(_jq '.exceeds_200k_tokens')
  if [[ "$EXCEEDS" == "true" ]]; then
    CTX_PCT=95
  elif [[ -n "$INPUT_TOKENS" && "$INPUT_TOKENS" =~ ^[0-9]+$ && "$INPUT_TOKENS" -gt 0 ]]; then
    CTX_PCT=$(( INPUT_TOKENS * 100 / CTX_WINDOW_SIZE ))
    (( CTX_PCT > 100 )) && CTX_PCT=100
  fi
fi
# Context window: green <50, amber 50-80, red ≥80 (compaction hits near 80%)
CTX_BAR=$(_bar "$CTX_PCT" 50 80)

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 7 — Cost: session + today's total
# ════════════════════════════════════════════════════════════════════════════
SESSION_COST=$(_jq '.cost.total_cost_usd')
COST_DISPLAY=""

if [[ -n "$SESSION_COST" && "$SESSION_COST" =~ ^[0-9] ]]; then
  SESSION_COST_FMT=$(printf "\$%.2f" "$SESSION_COST" 2>/dev/null || echo "")
  TODAY_COST_FMT=""
  COSTS_FILE="$HOME/.claude/metrics/costs.jsonl"
  if [[ -f "$COSTS_FILE" ]]; then
    TODAY=$(date +%Y-%m-%d)
    TODAY_COST=$(grep "\"${TODAY}" "$COSTS_FILE" 2>/dev/null \
      | jq -s '[.[].estimated_cost_usd] | add // 0' 2>/dev/null || true)
    if [[ -n "$TODAY_COST" && "$TODAY_COST" =~ ^[0-9] ]]; then
      TODAY_COST_FMT=$(printf "\$%.2f today" "$TODAY_COST" 2>/dev/null || true)
    fi
  fi
  if [[ -n "$TODAY_COST_FMT" ]]; then
    COST_DISPLAY="${SESSION_COST_FMT} / ${TODAY_COST_FMT}"
  elif [[ -n "$SESSION_COST_FMT" ]]; then
    COST_DISPLAY="$SESSION_COST_FMT"
  fi
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 8 — 5-hour block + weekly usage % (bars)
# Source priority: stdin rate_limits → ccusage (cached)
# ════════════════════════════════════════════════════════════════════════════
FIVE_HR_PCT=""
WEEK_PCT=""

# Try stdin first (Claude.ai subscriber fields)
FIVE_HR_JSON=$(_jq '.rate_limits.five_hour.used_percentage')
WEEK_JSON=$(_jq '.rate_limits.seven_day.used_percentage')
[[ -n "$FIVE_HR_JSON" ]] && FIVE_HR_PCT=$(printf "%.0f" "$FIVE_HR_JSON" 2>/dev/null || true)
[[ -n "$WEEK_JSON"    ]] && WEEK_PCT=$(printf "%.0f" "$WEEK_JSON"    2>/dev/null || true)

# Quota reset instants — Claude Code sends resets_at as a Unix epoch.
# 5h → HH:MM (always same/next day); weekly → "Mon D HH:MM" (days out).
FIVE_HR_RESET_FMT=$(_fmt_epoch "$(_jq '.rate_limits.five_hour.resets_at')" '%H:%M')
WEEK_RESET_FMT=$(_fmt_epoch "$(_jq '.rate_limits.seven_day.resets_at')" '%b %e %H:%M' | tr -s ' ')

# Fallback: ccusage, cached separately as "five_hr" and "wk" keys
if [[ -z "$FIVE_HR_PCT" ]]; then
  if (( CACHE_AGE < CACHE_TTL )); then
    FIVE_HR_PCT=$(_cache_get "five_hr")
  else
    BLOCKS_JSON=$(_to 0.5 bash -c 'ccusage blocks --active --json 2>/dev/null' || true)
    if [[ -n "$BLOCKS_JSON" ]]; then
      FIVE_HR_PCT=$(printf '%s' "$BLOCKS_JSON" \
        | jq -r '.[0].burnPercent // empty' 2>/dev/null || true)
    fi
    _cache_set "five_hr" "${FIVE_HR_PCT:-}"
  fi
fi

if [[ -z "$WEEK_PCT" ]]; then
  if (( CACHE_AGE < CACHE_TTL )); then
    WEEK_PCT=$(_cache_get "wk")
  else
    WEEK_PCT=""  # ccusage doesn't surface a clean weekly % — leave blank unless rate_limits present
    _cache_set "wk" ""
  fi
fi

FIVE_HR_BAR=""
WEEK_BAR=""
# 5h rate block: green <60, amber 60-85, red ≥85 (default)
[[ "$FIVE_HR_PCT" =~ ^[0-9]+$ ]] && FIVE_HR_BAR=$(_bar "$FIVE_HR_PCT" 60 85)
# Weekly budget: green <70, amber 70-90, red ≥90 (more forgiving, marathon not sprint)
[[ "$WEEK_PCT"    =~ ^[0-9]+$ ]] && WEEK_BAR=$(_bar "$WEEK_PCT" 70 90)

# ════════════════════════════════════════════════════════════════════════════
# POWERLINE RENDER
# Requires a Nerd Font (iTerm2: Profile → Text → "Use built-in Powerline glyphs"
# OR install MesloLGS NF / JetBrainsMono NF / Hack NF and select it).
# ════════════════════════════════════════════════════════════════════════════

# Powerline separator glyphs (Nerd Font)
PL_SEP=$'\ue0b0'       # right triangle
PL_SEP_THIN=$'\ue0b1'  # right chevron

# True-color palette (24-bit). Each segment has bg + fg pair.
_fg() { printf '\033[38;2;%d;%d;%dm' "$1" "$2" "$3"; }
_bg() { printf '\033[48;2;%d;%d;%dm' "$1" "$2" "$3"; }

# Dracula-ish palette
C_WHITE="248;248;242"
C_BLACK="40;42;54"
C_PURPLE="189;147;249"
C_BLUE="98;114;164"
C_CYAN="139;233;253"
C_GREEN="80;250;123"
C_YELLOW="241;250;140"
C_ORANGE="255;184;108"
C_PINK="255;121;198"
C_RED="255;85;85"
C_GREY="68;71;90"

# Build one powerline segment. Handles bg→bg transition.
# Args: <bg r;g;b> <fg r;g;b> <text> <prev_bg r;g;b or empty>
_seg() {
  local bg=$1 fg=$2 text=$3 prev_bg=$4
  local out=""
  if [[ -n "$prev_bg" ]]; then
    if [[ "$prev_bg" == "$bg" ]]; then
      # Same bg: use thin chevron with contrasting fg (use segment's own fg)
      out+=$(_bg ${bg//;/ }; _fg ${fg//;/ })" ${PL_SEP_THIN} "
    else
      # Different bg: full triangle separator
      out+=$(_fg ${prev_bg//;/ }; _bg ${bg//;/ })"$PL_SEP"
    fi
  fi
  # (First segment has no leading separator — clean left edge)
  out+=$(_bg ${bg//;/ }; _fg ${fg//;/ })" ${text} "
  printf '%s' "$out"
}

# Close the powerline (last segment tail): draw final separator in last bg on default.
_seg_end() {
  local last_bg=$1
  printf '%b' "\033[0m$(_fg ${last_bg//;/ })${PL_SEP}\033[0m"
}

# Powerline bar (no brackets, fills background with block intensity)
# Usage: _pbar <pct>
_pbar() {
  local pct=${1:-0}
  local total=6
  local filled=$(( pct * total / 100 ))
  (( filled > total )) && filled=$total
  local empty=$(( total - filled ))
  local bg fg
  if   (( pct < 60 )); then bg=$C_GREEN;  fg=$C_BLACK
  elif (( pct < 85 )); then bg=$C_YELLOW; fg=$C_BLACK
  else                       bg=$C_RED;    fg=$C_WHITE
  fi
  local bar="" i
  for (( i=0; i<filled; i++ )); do bar+="█"; done
  for (( i=0; i<empty;  i++ )); do bar+="░"; done
  printf '%s %s %3d%%' "$bg" "$fg" "$pct"  # placeholders; real use via _seg
  # Actually return just the text; caller wraps in _seg
}

# Pack bar into segment-ready text
_bar_text() {
  local pct=${1:-0}
  local total=6
  local filled=$(( pct * total / 100 ))
  (( filled > total )) && filled=$total
  local empty=$(( total - filled ))
  local bar="" i
  for (( i=0; i<filled; i++ )); do bar+="█"; done
  for (( i=0; i<empty;  i++ )); do bar+="░"; done
  printf '%s %3d%%' "$bar" "$pct"
}
_bar_bg() {
  local pct=${1:-0}
  if   (( pct < 60 )); then echo "$C_GREEN"
  elif (( pct < 85 )); then echo "$C_YELLOW"
  else                       echo "$C_RED"
  fi
}
_bar_fg() {
  local pct=${1:-0}
  if (( pct >= 85 )); then echo "$C_WHITE"; else echo "$C_BLACK"; fi
}

# ── Build Line 1 (powerline: cwd · git · beads) ──────────────────────────────
L1=""
prev=""

# CWD segment (blue)
L1+=$(_seg "$C_BLUE" "$C_WHITE" "$CWD_DISPLAY" "$prev")
prev="$C_BLUE"

# Git segment (cyan if clean, orange if dirty)
if [[ -n "$GIT_BRANCH" ]]; then
  git_text="${GIT_BRANCH}${GIT_AHEAD_BEHIND}"
  [[ -n "$GIT_CHANGES" ]] && git_text+=" ${GIT_CHANGES}"
  if [[ -n "$GIT_CHANGES" ]]; then
    L1+=$(_seg "$C_ORANGE" "$C_BLACK" "$git_text" "$prev")
    prev="$C_ORANGE"
  else
    L1+=$(_seg "$C_CYAN" "$C_BLACK" "$git_text" "$prev")
    prev="$C_CYAN"
  fi
fi

# Reflect error badge (red, only when unacked errors exist) — cached 10s
if (( CACHE_AGE < CACHE_TTL )); then
  REFLECT_ERR_COUNT=$(_cache_get "reflect_err_count")
else
  REFLECT_ERR_COUNT=$(_reflect_err_count)
  _cache_set "reflect_err_count" "${REFLECT_ERR_COUNT:-0}"
fi
REFLECT_ERR_COUNT=${REFLECT_ERR_COUNT:-0}
if [[ "$REFLECT_ERR_COUNT" =~ ^[0-9]+$ ]] && (( REFLECT_ERR_COUNT > 0 )); then
  L1+=$(_seg "$C_RED" "$C_WHITE" "⚠${REFLECT_ERR_COUNT} /reflect:errors-ack" "$prev")
  prev="$C_RED"
fi

# Beads (pink)
if [[ -n "$BD_DISPLAY" ]]; then
  bd_text="bd:${BD_COUNT}▸"
  L1+=$(_seg "$C_PINK" "$C_BLACK" "$bd_text" "$prev")
  prev="$C_PINK"
fi

L1+=$(_seg_end "$prev")

# ── Build Line 2 (plain: model · health · bars · cost) ──────────────────────
# Health fg colour matches band
case "$HEALTH_EMOJI" in
  🟡) HEALTH_FG=$FG_YELLOW ;;
  🔴) HEALTH_FG=$FG_RED    ;;
  *)  HEALTH_FG=$FG_GREEN  ;;
esac

L2="${BOLD}${FG_MAGENTA}${MODEL_SHORT}${RESET}"
[[ -n "$EFFORT_DISPLAY" ]] && L2+=" ${SEP} ${EFFORT_DISPLAY}"
L2+=" ${SEP} ${HEALTH_FG}${HEALTH_EMOJI} ${MSG_COUNT}/50${RESET}"
L2+=" ${SEP} ctx ${CTX_BAR}"
if [[ -n "$FIVE_HR_BAR" ]]; then
  L2+=" ${SEP} 5h ${FIVE_HR_BAR}"
  [[ -n "$FIVE_HR_RESET_FMT" ]] && L2+=" ${FG_GREY}↻ ${FIVE_HR_RESET_FMT}${RESET}"
fi
if [[ -n "$WEEK_BAR" ]]; then
  L2+=" ${SEP} wk ${WEEK_BAR}"
  [[ -n "$WEEK_RESET_FMT" ]] && L2+=" ${FG_GREY}↻ ${WEEK_RESET_FMT}${RESET}"
fi
[[ -n "$COST_DISPLAY" ]] && L2+=" ${SEP} ${FG_GREEN}${COST_DISPLAY}${RESET}"

# ── Output ────────────────────────────────────────────────────────────────────
printf '%b\n %b' "$L1" "$L2"

# Reflect timeline dashboard (plugin-shipped, opt-out via REFLECT_TIMELINE_DISABLE=1)
# Helper emits its own leading newline + 4 rows. Falls through silently if absent.
# Pass session_id + project_dir from the stdin JSON so the helper can find
# THIS session's JSONL (Claude Code hashes project root, not cwd — worktrees
# end up under the parent repo's project hash, so cwd-based resolution misses).
# Fallback resolves the latest installed reflect plugin dynamically so the
# statusline keeps working across plugin version upgrades without manual edits.
if [[ -n "${CLAUDE_PLUGIN_ROOT:-}" ]]; then
  TIMELINE_HELPER="$CLAUDE_PLUGIN_ROOT/scripts/reflect_timeline.sh"
else
  _REFLECT_CACHE="$HOME/.claude/plugins/cache/agents-in-a-box/reflect"
  TIMELINE_HELPER="$(ls -1d "$_REFLECT_CACHE"/*/ 2>/dev/null | sort -V | tail -1)scripts/reflect_timeline.sh"
fi
if [[ "${REFLECT_TIMELINE_DISABLE:-0}" != "1" ]] && [[ -x "$TIMELINE_HELPER" ]]; then
  REFLECT_TIMELINE_SESSION_ID="$(_jq '.session_id')" \
  REFLECT_TIMELINE_PROJECT_DIR="$(_jq '.workspace.project_dir // .workspace.current_dir')" \
    "$TIMELINE_HELPER" 2>/dev/null
fi

printf '\n'

exit 0
