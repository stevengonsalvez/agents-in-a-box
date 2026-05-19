#!/usr/bin/env bash
# ABOUTME: Renders the 4-row reflect timeline dashboard (8 signals, paired 2-per-row) as
# ABOUTME: ANSI lines appended to the statusline. Reads local files only; 10s cache; opt-out via REFLECT_TIMELINE_DISABLE=1.

if [[ "${REFLECT_TIMELINE_DISABLE:-0}" == "1" ]]; then exit 0; fi

set -uo pipefail

# ── Constants ────────────────────────────────────────────────────────────────
CACHE_FILE="/tmp/claude-statusline-timeline-${USER}.txt"
EXPLAIN_FILE="/tmp/reflect-timeline-explain-${USER}.txt"
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
# pwd — so worktrees end up sharing the parent repo's project dir. Resolution
# order prefers the git-common-dir walk because $REFLECT_TIMELINE_PROJECT_DIR
# from statusline.sh carries the worktree cwd (which hashes to a project dir
# that exists but has no memory/ subdir — Claude Code's auto-memory writes
# always land under the git-root hash).
#
# 1. Walk up cwd to find .git dir, hash THAT path (matches Claude Code)
# 2. $REFLECT_TIMELINE_PROJECT_DIR fallback (non-git contexts)
# 3. Literal pwd hash (legacy)
_resolve_project_dir() {
  local source_path hash
  if command -v git >/dev/null 2>&1; then
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
  [[ -z "$source_path" ]] && source_path="${REFLECT_TIMELINE_PROJECT_DIR:-}"
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

# ── --explain drill-down mode (plain text, no ANSI) ──────────────────────────
# Emits a sectioned report of the actual events feeding each dashboard row over
# the last 2h. Optional ROW filter: REC|MEM|ING|DRN|TOK|ERR|COM|AGT|all.
_render_explain_text() {
  local filter="${1:-all}"
  python3 - "$filter" "$RECALL_LOG" "$INGEST_LOG" "$ERRORS_JSON" "$DRAIN_LOG" \
                       "$CLOUD_LOG" "${PROJECT_DIR:-}" "${SESSION_JSONL:-}" 2>/dev/null <<'PY'
import sys, os, json, datetime, subprocess, shutil, re

filter_row   = sys.argv[1]
recall_log   = sys.argv[2]
ingest_log   = sys.argv[3]
errors_json  = sys.argv[4]
drain_log    = sys.argv[5]
cloud_log    = sys.argv[6]
project_dir  = sys.argv[7]
session_jsonl= sys.argv[8]

now    = int(datetime.datetime.now().timestamp())
cutoff = now - 7200  # 2h
gen_ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")

def parse_ts(s):
    if s is None: return None
    if isinstance(s, (int, float)): return int(s)
    s = str(s).strip()
    if not s: return None
    if s.startswith('E'):
        try: return int(s[1:])
        except: return None
    try: return int(s)
    except: pass
    s2 = s.replace('Z', '+00:00')
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

def hhmmss(ts):
    return datetime.datetime.fromtimestamp(ts).strftime("%H:%M:%S")

def section(name, lines, total_label=None):
    out = []
    out.append("")
    out.append(f"▼ {name}")
    if not lines:
        out.append("  (none in the last 2h)")
    else:
        out.extend("  " + ln for ln in lines)
        if total_label:
            out.append(f"  ({total_label})")
    return "\n".join(out)

def gather_rec():
    rows = []
    if os.path.isfile(recall_log):
        try:
            with open(recall_log, 'r', errors='replace') as fh:
                for line in fh.readlines()[-5000:]:
                    line = line.strip()
                    if not line: continue
                    try: obj = json.loads(line)
                    except: continue
                    ts = parse_ts(obj.get("ts"))
                    if ts is None or ts < cutoff: continue
                    q = obj.get("query") or obj.get("q") or ""
                    hits = obj.get("hits") or obj.get("results") or obj.get("count") or ""
                    lat  = obj.get("latency_ms") or obj.get("latency") or ""
                    extras = []
                    if hits != "": extras.append(f"hits={hits}")
                    if lat != "":  extras.append(f"latency={lat}ms")
                    qstr = f'query="{q}"' if q else ""
                    rows.append((ts, f'{hhmmss(ts)}  {qstr}  {" ".join(extras)}'.strip()))
        except Exception:
            pass
    rows.sort()
    lines = [r[1] for r in rows]
    return lines, f"{len(lines)} total"

def gather_mem():
    rows = []
    if project_dir and os.path.isdir(os.path.join(project_dir, "memory")):
        mem_dir = os.path.join(project_dir, "memory")
        for root, _, files in os.walk(mem_dir):
            for f in files:
                if not f.endswith(".md"): continue
                p = os.path.join(root, f)
                try: mt = int(os.path.getmtime(p))
                except: continue
                if mt < cutoff: continue
                rows.append((mt, f"{hhmmss(mt)}  {f}"))
    rows.sort()
    lines = [r[1] for r in rows]
    return lines, f"{len(lines)} total"

def gather_ing():
    rows = []
    if os.path.isfile(ingest_log):
        try:
            with open(ingest_log, 'r', errors='replace') as fh:
                lines = fh.readlines()
            cur_ts = None; cur_file = None
            for line in lines:
                m = re.match(r'^  ingested_at:\s*"?([^"\s]+)"?', line)
                if m:
                    cur_ts = parse_ts(m.group(1))
                fm = re.match(r'^- file:\s*"?([^"\n]+?)"?\s*$', line)
                if fm:
                    cur_file = fm.group(1)
                if cur_ts is not None and cur_ts >= cutoff and cur_file:
                    rows.append((cur_ts, f"{hhmmss(cur_ts)}  {cur_file}"))
                    cur_ts = None; cur_file = None
                elif cur_ts is not None and cur_ts < cutoff:
                    cur_ts = None
        except Exception:
            pass
    rows.sort()
    out = [r[1] for r in rows]
    return out, f"{len(out)} total"

def gather_drn():
    rows = []
    if os.path.isfile(drain_log):
        try:
            with open(drain_log, 'r', errors='replace') as fh:
                for line in fh.readlines()[-2000:]:
                    if "drain start" not in line: continue
                    m = re.match(r'^\[([^\]]+)\]\s*.*drain start[^\d]*(\d+)?', line)
                    if not m: continue
                    ts = parse_ts(m.group(1))
                    pid = m.group(2) or "?"
                    if ts is None or ts < cutoff: continue
                    rows.append((ts, f"{hhmmss(ts)}  drain start pid={pid}"))
        except Exception:
            pass
    rows.sort()
    out = [r[1] for r in rows]
    return out, f"{len(out)} total"

def gather_tok():
    rows = []
    total = 0
    if session_jsonl and os.path.isfile(session_jsonl):
        try:
            with open(session_jsonl, 'r', errors='replace') as fh:
                for line in fh:
                    try: obj = json.loads(line)
                    except: continue
                    msg = obj.get("message") or {}
                    usage = msg.get("usage")
                    if not usage: continue
                    ts = parse_ts(obj.get("timestamp"))
                    if ts is None or ts < cutoff: continue
                    inp = int(usage.get("input_tokens") or 0)
                    outp= int(usage.get("output_tokens") or 0)
                    tot = inp + outp
                    total += tot
                    tools = []
                    content = msg.get("content")
                    if isinstance(content, list):
                        for c in content:
                            if isinstance(c, dict) and c.get("type") == "tool_use":
                                n = c.get("name")
                                if n: tools.append(n)
                    tool_str = f"  tool=[{', '.join(tools)}]" if tools else ""
                    rows.append((ts, f"{hhmmss(ts)}  in={inp} out={outp}  total={tot}{tool_str}"))
        except Exception:
            pass
    rows.sort()
    out = [r[1] for r in rows]
    return out, f"{len(out)} total; sum={total}"

def gather_err():
    rows = []
    if os.path.isfile(errors_json):
        try:
            with open(errors_json, 'r', errors='replace') as fh:
                data = json.load(fh)
            for e in data.get("errors", []):
                if e.get("acked"): continue
                ts = parse_ts(e.get("ts"))
                if ts is None or ts < cutoff: continue
                msg = e.get("message") or e.get("error") or e.get("summary") or ""
                rows.append((ts, f"{hhmmss(ts)}  {msg}".rstrip()))
        except Exception:
            pass
    rows.sort()
    out = [r[1] for r in rows]
    return out, f"{len(out)} total"

def gather_com():
    commits = []
    pushes = []
    if shutil.which("git"):
        try:
            r = subprocess.run(
                ["git", "log", "--since=2 hours ago", "--pretty=format:%cI\t%h\t%s"],
                capture_output=True, text=True, timeout=4
            )
            for line in r.stdout.splitlines():
                parts = line.split("\t", 2)
                if len(parts) < 3: continue
                ts = parse_ts(parts[0])
                if ts is None or ts < cutoff: continue
                commits.append((ts, f"{hhmmss(ts)}  {parts[1]}  {parts[2]}"))
        except Exception:
            pass
        try:
            br = subprocess.run(
                ["git", "rev-parse", "--abbrev-ref", "HEAD"],
                capture_output=True, text=True, timeout=2
            ).stdout.strip()
            if br and br != "HEAD":
                r2 = subprocess.run(
                    ["git", "reflog", "show", f"origin/{br}", "--since=2 hours ago",
                     "--pretty=format:%cI\t%h\t%s"],
                    capture_output=True, text=True, timeout=4
                )
                for line in r2.stdout.splitlines():
                    parts = line.split("\t", 2)
                    if len(parts) < 3: continue
                    ts = parse_ts(parts[0])
                    if ts is None or ts < cutoff: continue
                    pushes.append((ts, f"{hhmmss(ts)}  push {parts[1]}  {parts[2]}"))
        except Exception:
            pass
    rows = sorted(commits + pushes)
    out = [r[1] for r in rows]
    return out, f"{len(commits)} commits, {len(pushes)} pushes in window"

def gather_agt():
    rows = []
    # Task tool_use from session JSONL
    if session_jsonl and os.path.isfile(session_jsonl):
        try:
            with open(session_jsonl, 'r', errors='replace') as fh:
                for line in fh:
                    try: obj = json.loads(line)
                    except: continue
                    ts = parse_ts(obj.get("timestamp"))
                    if ts is None or ts < cutoff: continue
                    msg = obj.get("message") or {}
                    content = msg.get("content")
                    if not isinstance(content, list): continue
                    for c in content:
                        if not isinstance(c, dict): continue
                        if c.get("type") != "tool_use": continue
                        # Subagent-spawn tools: Task (interactive Claude Code) and
                        # Agent (background-job harness). TaskCreate is the todo-list
                        # tool, NOT a spawn — excluding it removes false positives.
                        if c.get("name") not in ("Task", "Agent"): continue
                        descr = ""
                        ipt = c.get("input") or {}
                        if isinstance(ipt, dict):
                            descr = ipt.get("description") or ipt.get("subagent_type") or ipt.get("prompt") or ""
                            if descr and len(descr) > 80: descr = descr[:77] + "..."
                        rows.append((ts, f'{hhmmss(ts)}  Task: "{descr}"'))
        except Exception:
            pass
    # tmux sessions
    if shutil.which("tmux"):
        try:
            r = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_created} #{session_name}"],
                capture_output=True, text=True, timeout=2
            )
            for line in r.stdout.splitlines():
                parts = line.split(" ", 1)
                if len(parts) < 2: continue
                try: ts = int(parts[0])
                except: continue
                name = parts[1]
                if not re.match(r'^(dev-|agent-|swarm-)', name): continue
                if ts < cutoff: continue
                rows.append((ts, f"{hhmmss(ts)}  tmux: {name}"))
        except Exception:
            pass
    # cloud-coding
    if os.path.isfile(cloud_log):
        try:
            with open(cloud_log, 'r', errors='replace') as fh:
                for line in fh:
                    try: obj = json.loads(line)
                    except: continue
                    ts = parse_ts(obj.get("ts"))
                    if ts is None or ts < cutoff: continue
                    label = obj.get("name") or obj.get("task") or obj.get("id") or "run"
                    rows.append((ts, f"{hhmmss(ts)}  cloud-coding: {label}"))
        except Exception:
            pass
    rows.sort()
    out = [r[1] for r in rows]
    return out, f"{len(out)} total"

SECTIONS = [
    ("REC", "REC — Recall events (~/.reflect/recall_log.jsonl)", gather_rec),
    ("MEM", "MEM — Auto-memory writes (project's memory/*.md mtimes)", gather_mem),
    ("ING", "ING — Ingest entries (~/.learnings/.memory-ingest-log.yaml)", gather_ing),
    ("DRN", "DRN — Drain runs (~/.reflect/drain.log)", gather_drn),
    ("TOK", "TOK — Token consumption (current session JSONL, per assistant turn)", gather_tok),
    ("ERR", "ERR — Errors written to ~/.reflect/errors.json (unacked only)", gather_err),
    ("COM", "COM — Git commits + pushes", gather_com),
    ("AGT", "AGT — Agent spawns (Task tool + tmux dev/agent/swarm + cloud-coding)", gather_agt),
]

bar = "=" * 63
print(bar)
print(f" Reflect Timeline — Drill-down  (last 2h, generated {gen_ts})")
print(bar)

want = filter_row.upper() if filter_row else "ALL"
for code, title, fn in SECTIONS:
    if want != "ALL" and want != code: continue
    try:
        lines, total = fn()
    except Exception as exc:
        lines, total = [], f"error: {exc}"
    print(section(title, lines, total))

print("")
PY
}

if [[ "${1:-}" == "--explain" ]]; then
  shift
  _render_explain_text "${1:-all}"
  exit 0
fi

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

# OSC 8 hyperlink: emit "<ESC>]8;;URL<ESC>\\TEXT<ESC>]8;;<ESC>\\"
# Terminals that don't grok OSC 8 silently render just TEXT.
_link() { printf '\033]8;;%s\033\\%s\033]8;;\033\\' "$1" "$2"; }

EXPLAIN_URL="file://${EXPLAIN_FILE}"

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
  # I: ingest — accept entries with double-quoted, single-quoted, or unquoted
  # ISO timestamps. Earlier regex only handled double quotes, so single-quoted
  # entries (e.g. written by yaml.dump in some Python contexts) extracted the
  # value WITH the quotes attached, breaking the downstream timestamp parser.
  if [[ -f "$INGEST_LOG" ]]; then
    grep -E '^  ingested_at:' "$INGEST_LOG" 2>/dev/null \
      | sed -E "s/^  ingested_at:[[:space:]]*['\"]?([^'\"]*)['\"]?.*/I\t\1\t1/"
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
    # A from same JSONL: subagent-spawn tool_use entries (Task = interactive
    # Claude Code; Agent = background-job harness). TaskCreate is the todo
    # tool, not a spawn — excluded.
    jq -r 'select(.message.content and .timestamp)
      | .timestamp as $t
      | (.message.content | if type=="array" then .[] else . end)
      | select(.type=="tool_use" and (.name=="Task" or .name=="Agent"))
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
  local linked_label
  linked_label=$(_link "$EXPLAIN_URL" "${label}:")
  local out="$(_fg "$r" "$g" "$b")${linked_label}${RESET} "
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
  # %s, NOT %b — $out contains raw ANSI bytes from _link / _fg, possibly
  # including literal `\E` (the OSC 8 terminator `\\` followed by labels
  # starting with `E` like ERR). Bash printf %b interprets `\E` as ESC
  # (bash extension), corrupting the bytes. %s preserves them verbatim.
  printf '%s' "$out"
}

_render_tokens() {
  # args: counts (already token totals per bucket)
  # Heat gradient: green ≤5k, yellow 5k–15k, red >15k. Label uses neutral
  # gold so it's visually distinct from the heat ramp.
  local counts=("$@")
  local linked_label
  linked_label=$(_link "$EXPLAIN_URL" "TOK:")
  local out="$(_fg 240 200 80)${linked_label}${RESET} "
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
  # %s, not %b — see _render_sparkline comment about \E corruption.
  printf '%s' "$out"
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
HINT=" $(_fg 110 110 130)↑ click a label or run: reflect timeline --explain TOK${RESET}"
# %s for args (preserves raw ANSI/OSC8 bytes); \n stays in the format string.
# Avoids bash printf %b's interpretation of `\E` → ESC which corrupts the ERR
# label (see _render_sparkline comment).
OUT=$(printf '\n%s\n %s\n %s\n %s\n%s' "$ROW1" "$ROW2" "$ROW3" "$ROW4" "$HINT")

# Refresh the drill-down file in background — clicks land on fresh data.
# Fire-and-forget; must not block the render.
( "$0" --explain > "$EXPLAIN_FILE" 2>/dev/null & ) >/dev/null 2>&1

FP=$(_fingerprint)
{
  printf '# sources: %s\n' "$FP"
  printf '%s\n' "$OUT"
} > "$CACHE_FILE" 2>/dev/null

printf '%s\n' "$OUT"
exit 0
