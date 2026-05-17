#!/usr/bin/env bash
# ABOUTME: Renders the 4-row reflect timeline dashboard (8 signals, paired 2-per-row) as
# ABOUTME: ANSI lines appended to the statusline. Reads local files only; 10s cache; opt-out via REFLECT_TIMELINE_DISABLE=1.

if [[ "${REFLECT_TIMELINE_DISABLE:-0}" == "1" ]]; then exit 0; fi

set -uo pipefail

# ── Constants ────────────────────────────────────────────────────────────────
CACHE_FILE="/tmp/claude-statusline-timeline-${USER}.txt"
CACHE_TTL=10
WINDOW_SEC=7200       # 2h
BUCKET_SEC=300        # 5min
NCELLS=24
TOKEN_FULLBAR=${REFLECT_TIMELINE_TOKEN_FULLBAR:-20000}

RECALL_LOG="$HOME/.reflect/recall_log.jsonl"
INGEST_LOG="$HOME/.learnings/.memory-ingest-log.yaml"
ERRORS_JSON="$HOME/.reflect/errors.json"
DRAIN_LOG="$HOME/.reflect/drain.log"
CLOUD_LOG="$HOME/.cloud-coding/runs.jsonl"
PROJECTS_DIR="$HOME/.claude/projects"

# ── Mtime helpers ────────────────────────────────────────────────────────────
_mtime() {
  local f=$1
  [[ -e "$f" ]] || { printf '0'; return; }
  stat -f %m "$f" 2>/dev/null || stat -c %Y "$f" 2>/dev/null || printf '0'
}

# ── Source mtime fingerprint (for stale-cache detection) ─────────────────────
_fingerprint() {
  local out=""
  for f in "$RECALL_LOG" "$INGEST_LOG" "$ERRORS_JSON" "$DRAIN_LOG" "$CLOUD_LOG"; do
    out+="$f:$(_mtime "$f") "
  done
  # Current session JSONL (most-recent .jsonl under cwd-hash dir)
  if [[ -n "${SESSION_JSONL:-}" && -f "$SESSION_JSONL" ]]; then
    out+="$SESSION_JSONL:$(_mtime "$SESSION_JSONL") "
  fi
  printf '%s' "$out"
}

# ── Resolve the live session's project dir + JSONL ───────────────────────────
# Claude Code hashes the PROJECT ROOT (the dir containing .git), not the literal
# pwd — so worktrees end up sharing the parent repo's project dir. Three-tier
# resolution, env-driven first because that's what statusline.sh hands us.
#
# 1. $REFLECT_TIMELINE_PROJECT_DIR (passed by statusline.sh from stdin JSON)
# 2. Walk up cwd to find .git dir, hash THAT path
# 3. Fall back to literal pwd hash (legacy behaviour)
_resolve_project_dir() {
  local source_path hash
  source_path="${REFLECT_TIMELINE_PROJECT_DIR:-}"
  if [[ -z "$source_path" ]] && command -v git >/dev/null 2>&1; then
    # `git rev-parse --git-common-dir`'s parent → the main repo root for both
    # regular checkouts AND worktrees. Worktrees have .git as a file pointing
    # to <main>/.git/worktrees/<name>; --git-common-dir resolves through that
    # to <main>/.git. Claude Code hashes this same path, so we match its
    # project-dir convention exactly.
    local gcd
    gcd=$(git rev-parse --git-common-dir 2>/dev/null)
    if [[ -n "$gcd" ]]; then
      # Make absolute if relative (it's usually relative to cwd)
      [[ "$gcd" != /* ]] && gcd="$(pwd)/$gcd"
      source_path=$(dirname "$gcd")
    fi
  fi
  # Last-ditch fallback: literal pwd
  [[ -z "$source_path" ]] && source_path=$(pwd 2>/dev/null || echo "")
  [[ -z "$source_path" ]] && return
  hash=$(printf '%s' "$source_path" | tr '/.' '-')
  local dir="${PROJECTS_DIR}/${hash}"
  [[ -d "$dir" ]] && printf '%s' "$dir"
}

_resolve_session_jsonl() {
  # If session_id is explicit, find <session_id>.jsonl anywhere — most reliable.
  if [[ -n "${REFLECT_TIMELINE_SESSION_ID:-}" ]]; then
    local match
    match=$(find "$PROJECTS_DIR" -maxdepth 2 -name "${REFLECT_TIMELINE_SESSION_ID}.jsonl" 2>/dev/null | head -1)
    [[ -n "$match" ]] && { printf '%s' "$match"; return; }
  fi
  # Else: pick the most-recently-modified JSONL inside the resolved project dir.
  local dir latest
  dir=$(_resolve_project_dir)
  [[ -z "$dir" ]] && return
  latest=$(ls -t "$dir"/*.jsonl 2>/dev/null | head -1)
  [[ -n "$latest" ]] && printf '%s' "$latest"
}

SESSION_JSONL=$(_resolve_session_jsonl)
PROJECT_DIR=$(_resolve_project_dir)

# ── Cache check ──────────────────────────────────────────────────────────────
_now=$(date +%s)
if [[ -f "$CACHE_FILE" ]]; then
  cache_mtime=$(_mtime "$CACHE_FILE")
  age=$(( _now - cache_mtime ))
  if (( age < CACHE_TTL )); then
    # Verify fingerprint still matches stored fingerprint
    stored_fp=$(head -1 "$CACHE_FILE" 2>/dev/null | sed -n 's/^# sources: //p')
    current_fp=$(_fingerprint)
    if [[ "$stored_fp" == "$current_fp" ]]; then
      tail -n +2 "$CACHE_FILE"
      exit 0
    fi
  fi
fi

# ── ANSI helpers ─────────────────────────────────────────────────────────────
RESET=$'\033[0m'
_fg() { printf '\033[38;2;%d;%d;%dm' "$1" "$2" "$3"; }

# Block glyphs (height 0..8)
GLYPHS=('·' '▁' '▂' '▃' '▄' '▅' '▆' '▇' '█')

# ── Parsers — each emits "<unix_ts>\t<count>" rows on stdout ─────────────────
# ── Gather raw timestamps per signal (text/jsonl extraction only) ────────────
# Each emitter writes lines: <signal>\t<iso_or_epoch>\t<count>
_gather_raw() {
  # R: recall
  if [[ -f "$RECALL_LOG" ]]; then
    tail -n 5000 "$RECALL_LOG" 2>/dev/null \
      | jq -r '"R\t" + (.ts // "") + "\t1"' 2>/dev/null
  fi
  # I: ingest
  if [[ -f "$INGEST_LOG" ]]; then
    grep -E '^  ingested_at:' "$INGEST_LOG" 2>/dev/null \
      | sed -E 's/^  ingested_at:[[:space:]]*"?([^"]*)"?.*/I\t\1\t1/'
  fi
  # E: errors (unacked only)
  if [[ -f "$ERRORS_JSON" ]]; then
    jq -r '.errors[] | select((.acked // false) == false) | "E\t" + .ts + "\t1"' \
      "$ERRORS_JSON" 2>/dev/null
  fi
  # D: drain
  if [[ -f "$DRAIN_LOG" ]]; then
    grep '──── drain start' "$DRAIN_LOG" 2>/dev/null \
      | tail -n 500 \
      | sed -E 's/^\[([^]]+)\].*/D\t\1\t1/'
  fi
  # M: memory mtimes (already epoch). Uses the same resolved project dir as
  # the session JSONL — see _resolve_project_dir above.
  if [[ -n "$PROJECT_DIR" && -d "$PROJECT_DIR/memory" ]]; then
    find "$PROJECT_DIR/memory" -maxdepth 2 -name '*.md' -newermt '2 hours ago' 2>/dev/null \
      | while IFS= read -r f; do printf 'M\tE%s\t1\n' "$(_mtime "$f")"; done
  fi
  # T: tokens
  if [[ -n "$SESSION_JSONL" && -f "$SESSION_JSONL" ]]; then
    jq -r 'select(.message.usage and .timestamp)
      | "T\t" + .timestamp + "\t" + (((.message.usage.input_tokens // 0)
        + (.message.usage.output_tokens // 0)) | tostring)' \
      "$SESSION_JSONL" 2>/dev/null
    # A from same JSONL: Task tool_use entries
    jq -r 'select(.message.content and .timestamp)
      | .timestamp as $t
      | (.message.content | if type=="array" then .[] else . end)
      | select(.type=="tool_use" and (.name=="Task" or .name=="TaskCreate"))
      | "A\t" + $t + "\t1"' "$SESSION_JSONL" 2>/dev/null
  fi
  # C: git commits + pushes
  if command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
    git log --since='2 hours ago' --pretty=format:'C	%cI	1' 2>/dev/null
    local br
    br=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    if [[ -n "$br" && "$br" != "HEAD" ]]; then
      git reflog show "origin/$br" --since='2 hours ago' --pretty=format:'C	%cI	1' 2>/dev/null
    fi
  fi
  # A: tmux sessions (epoch already)
  if command -v tmux >/dev/null 2>&1; then
    tmux list-sessions -F '#{session_created} #{session_name}' 2>/dev/null \
      | awk '$2 ~ /^(dev-|agent-|swarm-)/ { printf "A\tE%s\t1\n", $1 }'
  fi
  # A: cloud-coding
  if [[ -f "$CLOUD_LOG" ]]; then
    jq -r '"A\t" + (.ts // "") + "\t1"' "$CLOUD_LOG" 2>/dev/null
  fi
}

# ── Single-pass bucketer (Python): parse ISO once, bucket into 24 slots ──────
# Input lines: <signal>\t<iso_or_E<epoch>>\t<count>
# Output: one line per signal: <signal> <b0> <b1> ... <b23>
_bucket_all() {
  python3 -c "
import sys, datetime
now=$_now
ncells=$NCELLS
bsec=$BUCKET_SEC
cutoff = now - ncells*bsec
sigs = ['R','I','E','D','M','T','C','A']
b = {s: [0]*ncells for s in sigs}
def parse_ts(s):
    if not s: return None
    if s.startswith('E'):
        try: return int(s[1:])
        except: return None
    s2 = s.replace('Z','+00:00')
    try:
        d = datetime.datetime.fromisoformat(s2)
    except ValueError:
        try: d = datetime.datetime.fromisoformat(s2.split('.')[0])
        except: return None
    except Exception:
        return None
    if d.tzinfo is None:
        d = d.astimezone()
    return int(d.timestamp())
for line in sys.stdin:
    parts = line.rstrip('\n').split('\t')
    if len(parts) < 3: continue
    sig, tsraw, cnt = parts[0], parts[1], parts[2]
    if sig not in b: continue
    ts = parse_ts(tsraw)
    if ts is None or ts < cutoff: continue
    try: c = int(float(cnt))
    except: continue
    idx = ncells - 1 - (now - ts) // bsec
    if 0 <= idx < ncells:
        b[sig][idx] += c
for s in sigs:
    print(s, *b[s])
"
}

# ── Renderers ────────────────────────────────────────────────────────────────
# Per-row max-count → glyph height index (1..8). Empty → 0 (·).
_glyph_idx() {
  # args: count max_for_full
  local c=$1 mx=$2
  (( c <= 0 )) && { printf '0'; return; }
  (( mx <= 0 )) && mx=1
  local idx=$(( c * 8 / mx ))
  (( idx < 1 )) && idx=1
  (( idx > 8 )) && idx=8
  printf '%d' "$idx"
}

_render_sparkline() {
  # args: label r g b counts...
  # Renders "<label>: <24 cells>" — label in its base color at full saturation,
  # cells intensity-scaled by ABSOLUTE count (1 event = ▁, 8+ events = █).
  #
  # Why absolute, not per-row max: with per-row-max scaling, a row that has
  # ONE event in one bucket normalizes that single event to height 8 (because
  # it IS the row's max), producing a "pipe" — a full-height block surrounded
  # by zeros. Looks nothing like a bar chart. Absolute mapping means rare
  # events are visibly small (▁), busy buckets stack up tall, and the
  # bar-chart shape forms naturally across the 24 cells.
  local label=$1 r=$2 g=$3 b=$4
  shift 4
  local counts=("$@")
  local max=8 c
  local out="$(_fg "$r" "$g" "$b")${label}:${RESET} "
  local i idx
  for (( i=0; i<NCELLS; i++ )); do
    c=${counts[i]:-0}
    idx=$(_glyph_idx "$c" "$max")
    if (( idx == 0 )); then
      out+="$(_fg 90 90 110)${GLYPHS[0]}${RESET}"
    else
      # Full base color for every non-zero cell — glyph height alone conveys
      # intensity. Previous version dimmed by 30+idx*25 alpha, which on a
      # dark terminal rendered low-count cells (idx=1 → 21% brightness)
      # near-invisible. Sparkline convention: height = intensity, color =
      # signal identity. Don't dim both.
      out+="$(_fg "$r" "$g" "$b")${GLYPHS[idx]}${RESET}"
    fi
  done
  printf '%b' "$out"
}

_render_tokens() {
  # args: counts (already token totals per bucket)
  # Heat gradient: green ≤5k, yellow 5k–15k, red >15k. Label uses neutral
  # gold so it's visually distinct from the heat ramp.
  local counts=("$@")
  local out="$(_fg 240 200 80)TOK:${RESET} "
  local i c idx h alpha r g b
  for (( i=0; i<NCELLS; i++ )); do
    c=${counts[i]:-0}
    if (( c <= 0 )); then
      out+="$(_fg 90 90 110)${GLYPHS[0]}${RESET}"
      continue
    fi
    # Height: count / TOKEN_FULLBAR scaled to 1..8
    h=$(( c * 8 / TOKEN_FULLBAR ))
    (( h < 1 )) && h=1
    (( h > 8 )) && h=8
    # Heat: green ≤5k, yellow 5k–15k, red >15k
    if   (( c <= 5000 ));  then r=120; g=200; b=120
    elif (( c <= 15000 )); then r=240; g=200; b=80
    else                        r=240; g=80;  b=80
    fi
    out+="$(_fg $r $g $b)${GLYPHS[h]}${RESET}"
  done
  printf '%b' "$out"
}

# ── Build buckets — single-pass ──────────────────────────────────────────────
BUCKETS_RAW=$( _gather_raw | _bucket_all )
declare -A SIG
while IFS= read -r line; do
  s=${line%% *}
  rest=${line#* }
  SIG[$s]="$rest"
done <<< "$BUCKETS_RAW"

RECALL_B=( ${SIG[R]:-} )
INGEST_B=( ${SIG[I]:-} )
ERRORS_B=( ${SIG[E]:-} )
DRAIN_B=(  ${SIG[D]:-} )
MEMORY_B=( ${SIG[M]:-} )
TOKENS_B=( ${SIG[T]:-} )
COMMITS_B=( ${SIG[C]:-} )
AGENTS_B=(  ${SIG[A]:-} )

# Pad to NCELLS in case a signal had no data
for arr in RECALL_B INGEST_B ERRORS_B DRAIN_B MEMORY_B TOKENS_B COMMITS_B AGENTS_B; do
  eval "len=\${#${arr}[@]}"
  if (( len < NCELLS )); then
    while (( len < NCELLS )); do
      eval "${arr}+=( 0 )"
      len=$(( len + 1 ))
    done
  fi
done

# ── Render 8 individual sparklines, paired 2-per-row ─────────────────────────
# Row 3: R (recall, blue)        | M (auto-memory writes, cyan)
# Row 4: I (ingest, green)       | D (drain runs, orange)
# Row 5: T (tokens, heat)        | E (errors, red)
# Row 6: C (commits+pushes, gray)| A (agent spawns, cyan)
SPARK_R=$(_render_sparkline "REC"  80 180 255 "${RECALL_B[@]}")
SPARK_M=$(_render_sparkline "MEM" 100 200 220 "${MEMORY_B[@]}")
SPARK_I=$(_render_sparkline "ING" 120 200 120 "${INGEST_B[@]}")
SPARK_D=$(_render_sparkline "DRN" 230 150  90 "${DRAIN_B[@]}")
SPARK_T=$(_render_tokens "${TOKENS_B[@]}")
SPARK_E=$(_render_sparkline "ERR" 240  80  80 "${ERRORS_B[@]}")
SPARK_C=$(_render_sparkline "COM" 180 180 180 "${COMMITS_B[@]}")
SPARK_A=$(_render_sparkline "AGT"  80 200 220 "${AGENTS_B[@]}")

# Pair side-by-side with 3-space separator. Each line gets a single leading
# space so it visually aligns under line 2 (which is printed with a leading
# space by statusline.sh — see "printf '%b\n %b' ..." block).
GAP="   "
ROW1="${SPARK_R}${GAP}${SPARK_M}"
ROW2="${SPARK_I}${GAP}${SPARK_D}"
ROW3="${SPARK_T}${GAP}${SPARK_E}"
ROW4="${SPARK_C}${GAP}${SPARK_A}"

# ── Write cache and emit ─────────────────────────────────────────────────────
OUT=$(printf '\n%b\n %b\n %b\n %b' "$ROW1" "$ROW2" "$ROW3" "$ROW4")
FP=$(_fingerprint)
{
  printf '# sources: %s\n' "$FP"
  printf '%s\n' "$OUT"
} > "$CACHE_FILE" 2>/dev/null

printf '%s\n' "$OUT"
exit 0
