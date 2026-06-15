#!/usr/bin/env bash
# ~/.copilot/statusline.sh — GitHub Copilot CLI rich single-line statusline
#
# A copilot-native port of toolkit/claude-code-4.5/statusline.sh. Renders ONE
# powerline line (Copilot only renders the first line of stdout):
#
#   cwd › git branch+ahead/behind+dirty › model+ctx bar › duration › remote
#
# Reads JSON session state from stdin (Copilot CLI schema — fields differ from
# Claude). The only slow external command is git, which is timeout-guarded
# (500ms) with silent fallbacks so it never blocks Copilot's per-response render
# loop. Unlike the Claude original, there is NO 10s /tmp cache here: the only
# signals Claude cached (bd / reflect errors / ccusage) were all dropped in this
# port, so the sole remaining slow signal (git) is timeout-guarded, not cached.
#
# ── COPILOT stdin fields consumed (confirmed via docs / research) ─────────────
#   .cwd
#   .model.id, .model.display_name
#   .context_window.current_context_used_percentage   (preferred ctx signal)
#   .context_window.current_context_tokens            (ratio fallback numerator)
#   .context_window.context_window_size               (ratio fallback denominator)
#   .cost.total_duration_ms                            (wall-clock, rendered as "Xm Ys")
#   .remote.connected, .remote.indicator               (rendered if present)
#   .session_id, .version                              (read defensively, not rendered)
#
# ── Claude signals DROPPED for Copilot (no equivalent in Copilot stdin) ───────
#   • USD cost    (.cost.total_cost_usd)      — Copilot exposes duration_ms, not $.
#                  → rendered as a DURATION segment instead of a cost segment.
#   • 5h / weekly rate-limit bars (.rate_limits.*) — Copilot has no OAuth-grade
#                  budget window block; nothing to source. Omitted entirely.
#   • session-health N/50 badge (.transcript_path + user-turn count) — Copilot
#                  provides no transcript path nor message count. Omitted.
#   • reasoning effort / fast_mode (.effort.level, .fast_mode) — NOT present in
#                  Copilot's confirmed field set. Read defensively below; if the
#                  field ever appears it renders, otherwise silently omitted.
#                  (Needs live confirmation once the binary is unblocked.)
#   • beads (bd) badge, reflect-error badge, ainb side-channel, reflect-timeline
#                  dashboard — Claude-ecosystem extras, out of the Copilot port
#                  scope and/or incompatible with single-line output. Omitted.
#
# Omission rule: a segment that has no data is NOT printed (no empty/zero
# segments) — graceful degradation, exactly one line out.

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

# ── Unicode progress bar (field-agnostic — ported verbatim from Claude) ───────
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

# ── timeout wrapper (ported verbatim) ─────────────────────────────────────────
# Prefer `timeout`, fall back to coreutils' `gtimeout`, else run with no limit.
# macOS Homebrew installs coreutils' timeout as `gtimeout` and doesn't symlink
# `timeout` unless gnubin is on PATH — without this the guarded git segments
# below would silently blank on a stock mac.
_to() {
  if command -v timeout >/dev/null 2>&1; then timeout "$@"
  elif command -v gtimeout >/dev/null 2>&1; then gtimeout "$@"
  else shift; "$@"; fi
}

# ── Read JSON from stdin once ─────────────────────────────────────────────────
INPUT=$(cat)
_jq() { printf '%s' "$INPUT" | jq -r "${1} // empty" 2>/dev/null; }

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 1 — Model short name
#   Copilot reports e.g. "GPT-4o", "Claude Sonnet 4.6", "Gemini 2.5 Pro".
#   The Claude tier/version extractor still applies for Claude-family ids; any
#   other vendor falls through to the lowercased display name unchanged.
# ════════════════════════════════════════════════════════════════════════════
MODEL_DISPLAY=$(_jq '.model.display_name')
MODEL_ID=$(_jq '.model.id')

if [[ -n "$MODEL_DISPLAY" ]]; then
  # "Claude Opus 4.7" → "opus-4.7"; "GPT-4o" → "gpt-4o"; "Gemini 2.5 Pro" → "gemini 2.5 pro"
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
  MODEL_SHORT="$MODEL_ID"
else
  MODEL_SHORT="unknown"
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 1b — Reasoning effort + fast mode (DEFENSIVE — not in Copilot's
#   confirmed field set; renders only if the field ever appears, else omitted).
# ════════════════════════════════════════════════════════════════════════════
EFFORT_DISPLAY=""
EFFORT_LEVEL=$(_jq '.effort.level')
if [[ -n "$EFFORT_LEVEL" ]]; then
  case "$EFFORT_LEVEL" in
    low)    EFFORT_TAG="lo"   ;;
    medium) EFFORT_TAG="med"  ;;
    high)   EFFORT_TAG="high" ;;
    xhigh)  EFFORT_TAG="xhi"  ;;
    *)      EFFORT_TAG="$EFFORT_LEVEL" ;;
  esac
  FAST_FLAG=""
  [[ "$(_jq '.fast_mode')" == "true" ]] && FAST_FLAG="⚡"
  EFFORT_DISPLAY="eff ${EFFORT_TAG}${FAST_FLAG}"
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 2 — Working directory (~ collapsed, adaptive-shortened)
# ════════════════════════════════════════════════════════════════════════════
CWD=$(_jq '.cwd')
[[ -z "$CWD" ]] && CWD=$(pwd 2>/dev/null || echo "?")
CWD_DISPLAY="${CWD/#$HOME/\~}"

# Detect terminal width (Copilot stdin doesn't expose it; fall back to tput).
TERM_COLS=$(tput cols 2>/dev/null || echo 120)
[[ -z "$TERM_COLS" || "$TERM_COLS" -lt 20 ]] && TERM_COLS=120

# _shorten_path — keep last N segments (field-agnostic, ported verbatim)
_shorten_path() {
  local p=$1
  local keep=$2
  local IFS='/'; read -r -a parts <<< "$p"
  local n=${#parts[@]}
  (( n <= keep )) && { printf '%s' "$p"; return; }
  local tail="" i
  for (( i=n-keep; i<n; i++ )); do tail+="/${parts[i]}"; done
  printf '…%s' "$tail"
}

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
#   (entirely field-agnostic — keyed off $CWD; ported verbatim from Claude)
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
    STAGED=$(printf '%s\n' "$STATUS_OUTPUT" | awk '$0 !~ /^[ ?]/ {n++} END {print n+0}')
    UNSTAGED=$(printf '%s\n' "$STATUS_OUTPUT" | awk 'substr($0,2,1) !~ /[ ?]/ {n++} END {print n+0}')
    UNTRACKED=$(printf '%s\n' "$STATUS_OUTPUT" | grep -c '^\?\?' 2>/dev/null || echo 0)
    DELTA=$(( STAGED + UNSTAGED ))
    [[ $DELTA    -gt 0 ]] && GIT_CHANGES="±${DELTA}"
    [[ $UNTRACKED -gt 0 ]] && GIT_CHANGES="${GIT_CHANGES:+${GIT_CHANGES} }?${UNTRACKED}"
  fi
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 4 — Context window used %
#   Copilot field path differs from Claude:
#     preferred  .context_window.current_context_used_percentage
#     fallback   .context_window.current_context_tokens / .context_window.context_window_size
# ════════════════════════════════════════════════════════════════════════════
CTX_PCT=0
CTX_USED_PCT=$(_jq '.context_window.current_context_used_percentage')

if [[ -n "$CTX_USED_PCT" ]]; then
  CTX_PCT=$(printf "%.0f" "$CTX_USED_PCT" 2>/dev/null || echo 0)
else
  CTX_TOKENS=$(_jq '.context_window.current_context_tokens')
  CTX_WINDOW_SIZE=$(_jq '.context_window.context_window_size')
  [[ -z "$CTX_WINDOW_SIZE" || "$CTX_WINDOW_SIZE" == "0" ]] && CTX_WINDOW_SIZE=0
  if [[ -n "$CTX_TOKENS" && "$CTX_TOKENS" =~ ^[0-9]+$ && "$CTX_TOKENS" -gt 0 \
        && "$CTX_WINDOW_SIZE" =~ ^[0-9]+$ && "$CTX_WINDOW_SIZE" -gt 0 ]]; then
    CTX_PCT=$(( CTX_TOKENS * 100 / CTX_WINDOW_SIZE ))
    (( CTX_PCT > 100 )) && CTX_PCT=100
  fi
fi
# Context window: green <50, amber 50-80, red ≥80 (compaction hits near 80%)
CTX_BAR=$(_bar "$CTX_PCT" 50 80)

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 5 — Duration (Copilot's analogue to Claude's USD cost)
#   .cost.total_duration_ms is wall-clock ms for the session. Render "Xm Ys" /
#   "Ys" / "Xh Ym". Omit if absent. Carries NO "$" — this is the proof the
#   USD cost segment was correctly dropped.
# ════════════════════════════════════════════════════════════════════════════
DURATION_DISPLAY=""
DURATION_MS=$(_jq '.cost.total_duration_ms')
if [[ -n "$DURATION_MS" && "$DURATION_MS" =~ ^[0-9]+$ ]]; then
  TOTAL_SEC=$(( DURATION_MS / 1000 ))
  D_H=$(( TOTAL_SEC / 3600 ))
  D_M=$(( (TOTAL_SEC % 3600) / 60 ))
  D_S=$(( TOTAL_SEC % 60 ))
  if   (( D_H > 0 )); then DURATION_DISPLAY="${D_H}h ${D_M}m"
  elif (( D_M > 0 )); then DURATION_DISPLAY="${D_M}m ${D_S}s"
  else                     DURATION_DISPLAY="${D_S}s"
  fi
fi

# ════════════════════════════════════════════════════════════════════════════
# SIGNAL 6 — Remote session indicator (Copilot-specific)
#   .remote.indicator is a short string Copilot supplies for remote sessions;
#   .remote.connected is the bool gate. Omit when not connected / absent.
# ════════════════════════════════════════════════════════════════════════════
REMOTE_DISPLAY=""
REMOTE_INDICATOR=$(_jq '.remote.indicator')
REMOTE_CONNECTED=$(_jq '.remote.connected')
if [[ -n "$REMOTE_INDICATOR" ]]; then
  REMOTE_DISPLAY="$REMOTE_INDICATOR"
elif [[ "$REMOTE_CONNECTED" == "true" ]]; then
  REMOTE_DISPLAY="remote"
fi

# ════════════════════════════════════════════════════════════════════════════
# POWERLINE RENDER (single line)
# Requires a Nerd Font (iTerm2: Profile → Text → "Use built-in Powerline glyphs"
# OR install MesloLGS NF / JetBrainsMono NF / Hack NF and select it).
# ════════════════════════════════════════════════════════════════════════════

PL_SEP=$''       # right triangle
PL_SEP_THIN=$''  # right chevron

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

# Build one powerline segment. Handles bg→bg transition. (Ported verbatim.)
# Args: <bg r;g;b> <fg r;g;b> <text> <prev_bg r;g;b or empty>
_seg() {
  local bg=$1 fg=$2 text=$3 prev_bg=$4
  local out=""
  if [[ -n "$prev_bg" ]]; then
    if [[ "$prev_bg" == "$bg" ]]; then
      out+=$(_bg ${bg//;/ }; _fg ${fg//;/ })" ${PL_SEP_THIN} "
    else
      out+=$(_fg ${prev_bg//;/ }; _bg ${bg//;/ })"$PL_SEP"
    fi
  fi
  out+=$(_bg ${bg//;/ }; _fg ${fg//;/ })" ${text} "
  printf '%s' "$out"
}

# Close the powerline (last segment tail). (Ported verbatim.)
_seg_end() {
  local last_bg=$1
  printf '%b' "\033[0m$(_fg ${last_bg//;/ })${PL_SEP}\033[0m"
}

# Strip ANSI so we can embed the coloured ctx bar inside a powerline segment
# without leaking the bar's own reset/colour codes into the segment background.
# The _bar() helper paints its own colour; inside a powerline segment we want
# the segment bg to win, so we keep just the visible "[███░░░] NN%" text.
_strip_ansi() { printf '%s' "$1" | sed $'s/\033\\[[0-9;]*m//g'; }

# ── Build the single powerline line ───────────────────────────────────────────
LINE=""
prev=""

# CWD segment (blue)
LINE+=$(_seg "$C_BLUE" "$C_WHITE" "$CWD_DISPLAY" "$prev")
prev="$C_BLUE"

# Git segment (cyan if clean, orange if dirty)
if [[ -n "$GIT_BRANCH" ]]; then
  git_text="${GIT_BRANCH}${GIT_AHEAD_BEHIND}"
  [[ -n "$GIT_CHANGES" ]] && git_text+=" ${GIT_CHANGES}"
  if [[ -n "$GIT_CHANGES" ]]; then
    LINE+=$(_seg "$C_ORANGE" "$C_BLACK" "$git_text" "$prev")
    prev="$C_ORANGE"
  else
    LINE+=$(_seg "$C_CYAN" "$C_BLACK" "$git_text" "$prev")
    prev="$C_CYAN"
  fi
fi

# Model + ctx bar segment (purple). Effort appended if the field ever appears.
CTX_BAR_TEXT=$(_strip_ansi "$CTX_BAR")
model_text="${MODEL_SHORT}"
[[ -n "$EFFORT_DISPLAY" ]] && model_text+=" ${EFFORT_DISPLAY}"
model_text+="  ctx ${CTX_BAR_TEXT}"
LINE+=$(_seg "$C_PURPLE" "$C_BLACK" "$model_text" "$prev")
prev="$C_PURPLE"

# Duration segment (green) — Copilot's stand-in for Claude's USD cost. No "$".
if [[ -n "$DURATION_DISPLAY" ]]; then
  LINE+=$(_seg "$C_GREEN" "$C_BLACK" "$DURATION_DISPLAY" "$prev")
  prev="$C_GREEN"
fi

# Remote segment (pink) — only when Copilot reports a remote session.
if [[ -n "$REMOTE_DISPLAY" ]]; then
  LINE+=$(_seg "$C_PINK" "$C_BLACK" "$REMOTE_DISPLAY" "$prev")
  prev="$C_PINK"
fi

LINE+=$(_seg_end "$prev")

# ── Output (exactly one line, newline-terminated) ─────────────────────────────
printf '%b\n' "$LINE"

exit 0
