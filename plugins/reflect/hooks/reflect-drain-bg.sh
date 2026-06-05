#!/usr/bin/env bash
# reflect-drain-bg.sh — closed-loop pending-reflections drainer.
#
# Purpose
# -------
# Runs detached in the background after a Claude Code (or compatible harness)
# session starts. Drains entries from ~/.reflect/pending_reflections.jsonl by
# invoking `claude -p "/reflect <transcript>"` headlessly, then triggers an
# incremental GraphRAG reindex so the new learnings show up in recall results.
#
# Design
# ------
# - Idempotent: PID-based lockfile at ~/.reflect/drain.lock.
# - Cost-capped: REFLECT_DRAIN_MAX per run (default 3),
#                REFLECT_DRAIN_DAILY_MAX per UTC day (default 20).
# - Stale-tolerant: skips queue entries whose transcript no longer exists.
# - Poison-message-tolerant: per-entry retry counter at ~/.reflect/retry-count.jsonl;
#                           entries that fail >3 times are archived as "poison".
# - Always exits 0 so the calling SessionStart hook never thinks bootstrap broke.
#
# Configuration (env)
# -------------------
# REFLECT_DRAIN_MAX           Max entries per single drain run.       Default: 3
# REFLECT_DRAIN_DAILY_MAX     Max entries per UTC day.                Default: 20
# REFLECT_DRAIN_MAX_RETRIES   Per-entry retry cap before poison.      Default: 3
# REFLECT_DRAIN_LOG_MAX_BYTES Drain.log rotation threshold.           Default: 10485760
# REFLECT_DRAIN_DRY_RUN       If "1", don't call claude -p; just log. Default: 0
# REFLECT_STATE_DIR           State dir.                               Default: ~/.reflect
# REFLECT_DRAIN_CLAUDE_BIN    Path to claude binary.                  Default: claude (PATH)
# REFLECT_DRAIN_TIMEOUT       Per-entry claude -p wall-clock cap (s). Default: 180
# REFLECT_DRAIN_MAX_TURNS     Per-entry claude -p turn budget.        Default: 8
# REFLECT_DRAIN_TOKEN_MAX     Poison a transcript whose run reports   Default: 2000000
#                             more than this many total tokens.
# REFLECT_DRAIN_MODEL         Model alias for claude -p (--model).    Default: sonnet
# REFLECT_DRAIN_DEBOUNCE_SEC  Min seconds between drain runs.         Default: 600
# REFLECT_DISABLED            If "1", drainer is a hard no-op.        Default: 0
#
# Circuit-breaker rationale (2026-05-31 incident: a single drain ran 223 Opus
# turns / 41.5M tokens in 9.6min because the only bound was a 600s wall-clock).
# Defence in depth: turn cap + wall-clock cap + post-hoc token-budget poison +
# an ATOMIC (mkdir) lock so concurrent SessionStart spawns can't each slip past
# the daily cap (that race blew a cap of 20 to 61 in one day), plus a debounce
# so a burst of session starts triggers at most one drain per window.

set -uo pipefail

# ── Hard kill switch ────────────────────────────────────────────────────────
# Honoured before any work so an operator can stop all drains instantly.
if [[ "${REFLECT_DISABLED:-0}" == "1" ]]; then
    exit 0
fi

# ── Config ────────────────────────────────────────────────────────────────────
STATE_DIR="${REFLECT_STATE_DIR:-$HOME/.reflect}"
QUEUE_FILE="${STATE_DIR}/pending_reflections.jsonl"
LOCK_DIR="${STATE_DIR}/drain.lock.d"          # atomic mkdir lock (replaces racy PID file)
LOG_FILE="${STATE_DIR}/drain.log"
RETRY_FILE="${STATE_DIR}/retry-count.jsonl"
COST_FILE="${STATE_DIR}/drain-cost.jsonl"
POISON_FILE="${STATE_DIR}/poison-reflections.jsonl"
DEBOUNCE_FILE="${STATE_DIR}/drain.last-run"   # epoch seconds of last drain start

MAX_PER_RUN="${REFLECT_DRAIN_MAX:-3}"
DAILY_MAX="${REFLECT_DRAIN_DAILY_MAX:-20}"
MAX_RETRIES="${REFLECT_DRAIN_MAX_RETRIES:-3}"
LOG_MAX_BYTES="${REFLECT_DRAIN_LOG_MAX_BYTES:-10485760}"
DRY_RUN="${REFLECT_DRAIN_DRY_RUN:-0}"
CLAUDE_BIN="${REFLECT_DRAIN_CLAUDE_BIN:-claude}"
ENTRY_TIMEOUT="${REFLECT_DRAIN_TIMEOUT:-180}"
MAX_TURNS="${REFLECT_DRAIN_MAX_TURNS:-8}"
TOKEN_MAX="${REFLECT_DRAIN_TOKEN_MAX:-2000000}"
DRAIN_MODEL="${REFLECT_DRAIN_MODEL:-sonnet}"
DEBOUNCE_SEC="${REFLECT_DRAIN_DEBOUNCE_SEC:-600}"
CASCADE_ENABLED="${REFLECT_DRAIN_CASCADE:-1}"   # W4: gate+slice before /reflect
DRAIN_CWD="${REFLECT_DRAIN_CWD:-$HOME}"          # W5: neutral cwd for claude -p

# Locate sibling scripts (cascade) relative to this hook, robust to symlinks.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASCADE_SCRIPT="${SCRIPT_DIR}/../scripts/reflect_cascade.py"

mkdir -p "$STATE_DIR"

# Current epoch seconds, portable (date +%s works on macOS + Linux).
now_epoch() { date +%s; }

# ── Logging ───────────────────────────────────────────────────────────────────
rotate_log_if_needed() {
    if [[ -f "$LOG_FILE" ]]; then
        local size
        size=$(stat -f%z "$LOG_FILE" 2>/dev/null || stat -c%s "$LOG_FILE" 2>/dev/null || echo 0)
        if [[ "$size" -gt "$LOG_MAX_BYTES" ]]; then
            mv "$LOG_FILE" "${LOG_FILE}.1"
        fi
    fi
}

log() {
    rotate_log_if_needed
    printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$LOG_FILE"
}

# timeout wrapper: prefer `timeout`, fall back to coreutils' `gtimeout`, else
# run with no limit. macOS Homebrew installs coreutils' timeout as `gtimeout`
# and doesn't symlink `timeout` unless gnubin is on PATH.
_to() {
    if command -v timeout >/dev/null 2>&1; then timeout "$@"
    elif command -v gtimeout >/dev/null 2>&1; then gtimeout "$@"
    else shift; "$@"; fi
}

# Run a reflect_kb.errors subcommand via the best available path: the installed
# `reflect` CLI (`uv tool install reflect-kb`) first, else bare system
# `python3 -m` (works only if reflect_kb is importable by system python3).
# We deliberately do NOT use `uv run --with reflect-kb` — that resolves the
# name from PyPI, where reflect-kb is NOT published (it ships from the monorepo
# via git), so it would fail or fetch an unrelated package.
_reflect_errors_run() {
    if command -v reflect >/dev/null 2>&1; then
        reflect errors "$@"
    else
        python3 -m reflect_kb.errors "$@"
    fi
}

emit_error() {
    # emit_error <severity> <kind> <message> [transcript_path]
    local severity="$1" kind="$2" message="$3" transcript="${4:-}"
    # Build the context JSON with json.dumps so a transcript path containing a
    # quote or backslash can't produce a malformed --context argument.
    # (Pure stdlib json — does NOT need reflect_kb.)
    local context
    context=$(python3 -c 'import json,sys; print(json.dumps({"transcript_path": sys.argv[1]}))' "$transcript" 2>/dev/null || printf '{}')
    _reflect_errors_run append \
        --severity "$severity" --source drain --kind "$kind" \
        --message "$message" \
        --context "$context" \
        >/dev/null 2>&1 || true
}

# ── Locking (atomic) ────────────────────────────────────────────────────────
# `mkdir` is atomic on POSIX (create-or-fail in one syscall), so it is a safe
# cross-machine mutex — unlike the old check-then-write PID file, where two
# concurrent SessionStart spawns could both see "no lock" and both proceed,
# each passing the daily-cap check independently (the 20→61 overspend bug).
# macOS ships no `flock`, so mkdir is the portable primitive here. We stash the
# PID inside for stale-lock reclamation after a crash.
acquire_lock() {
    if mkdir "$LOCK_DIR" 2>/dev/null; then
        echo $$ > "$LOCK_DIR/pid"
        return 0
    fi
    # Lock dir exists — is the owner still alive?
    local existing_pid
    existing_pid=$(cat "$LOCK_DIR/pid" 2>/dev/null || echo "")
    if [[ -n "$existing_pid" ]] && kill -0 "$existing_pid" 2>/dev/null; then
        log "another drain is running (pid=$existing_pid); exiting"
        exit 0
    fi
    log "stale lock detected (pid=${existing_pid:-?} not running); reclaiming"
    rm -rf "$LOCK_DIR"
    if mkdir "$LOCK_DIR" 2>/dev/null; then
        echo $$ > "$LOCK_DIR/pid"
        return 0
    fi
    # Lost a race to reclaim — another drain won it. Defer to them.
    log "lost lock-reclaim race; exiting"
    exit 0
}

release_lock() {
    rm -rf "$LOCK_DIR"
}

# Make sure we always release the lock and never leave a non-zero exit code.
trap 'release_lock' EXIT
trap 'release_lock; exit 0' INT TERM

# ── Debounce ──────────────────────────────────────────────────────────────────
# SessionStart fires the drainer on every new session. A burst of starts (a
# fleet/swarm spinning up, or rapid /clear) would otherwise spawn a drain each.
# Once the lock is held, collapse a burst to one drain per window. MUST run
# while holding the lock so the check+update is atomic.
debounce_ok() {
    local last now delta
    now=$(now_epoch)
    if [[ -f "$DEBOUNCE_FILE" ]]; then
        last=$(cat "$DEBOUNCE_FILE" 2>/dev/null || echo 0)
        [[ "$last" =~ ^[0-9]+$ ]] || last=0
        delta=$((now - last))
        if [[ "$delta" -lt "$DEBOUNCE_SEC" ]]; then
            log "debounce: last drain ${delta}s ago (< ${DEBOUNCE_SEC}s); skipping"
            return 1
        fi
    fi
    echo "$now" > "$DEBOUNCE_FILE"
    return 0
}

# ── Daily cost cap ────────────────────────────────────────────────────────────
# Sum the `entries` field for today (NOT line count): LLM-invoking outcomes set
# entries=1, while $0 outcomes (cascade skip, stale, pre-run poison) set
# entries=0 — so gated skips never consume the daily budget the cap protects.
today_drain_count() {
    if [[ ! -f "$COST_FILE" ]]; then echo 0; return; fi
    local today
    today=$(date -u +%Y-%m-%d)
    python3 - "$COST_FILE" "$today" <<'PY' 2>/dev/null || echo 0
import json, sys
path, today = sys.argv[1], sys.argv[2]
n = 0
try:
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except Exception:
                continue
            if e.get("day") == today:
                n += int(e.get("entries", 1) or 0)
except FileNotFoundError:
    pass
print(n)
PY
}

record_cost_event() {
    # record_cost_event <entries> <transcript> <outcome> \
    #   [tokens] [cost_usd] [turns] [model] [cache_read] [cache_creation] [input] [output]
    # The token/cost fields default to 0 so pre-run outcomes (stale/poison) and
    # the legacy 3-arg call shape still emit valid JSON. `reflect cost` reads
    # these for the cached-vs-uncached timeline (W3).
    local entry_count="$1" transcript="$2" outcome="$3"
    local tokens="${4:-0}" cost="${5:-0}" turns="${6:-0}" model="${7:-}"
    local cache_read="${8:-0}" cache_creation="${9:-0}" input_tok="${10:-0}" output_tok="${11:-0}"
    # Coerce anything non-numeric to 0/valid so the JSON line never breaks.
    [[ "$tokens"         =~ ^[0-9]+$        ]] || tokens=0
    [[ "$cost"           =~ ^[0-9]+([.][0-9]+)?$ ]] || cost=0
    [[ "$turns"          =~ ^[0-9]+$        ]] || turns=0
    [[ "$cache_read"     =~ ^[0-9]+$        ]] || cache_read=0
    [[ "$cache_creation" =~ ^[0-9]+$        ]] || cache_creation=0
    [[ "$input_tok"      =~ ^[0-9]+$        ]] || input_tok=0
    [[ "$output_tok"     =~ ^[0-9]+$        ]] || output_tok=0
    local today ts
    today=$(date -u +%Y-%m-%d)
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    # Emit the JSON line via json.dumps: transcript/outcome/model are strings
    # that could contain a quote or backslash, which raw printf %s would turn
    # into a malformed line — and today_drain_count silently drops malformed
    # lines, so a broken line would under-count the daily cap. Numeric fields
    # are already coerced above so they serialise as bare numbers.
    python3 - "$ts" "$today" "$entry_count" "$transcript" "$outcome" "$model" \
        "$turns" "$tokens" "$cost" "$input_tok" "$output_tok" "$cache_read" "$cache_creation" \
        >> "$COST_FILE" <<'PY'
import json, sys
(ts, day, entries, transcript, outcome, model,
 turns, tokens, cost, inp, out, cr, cc) = sys.argv[1:14]
def _num(x):
    try:
        return int(x)
    except ValueError:
        try:
            return float(x)
        except ValueError:
            return 0
print(json.dumps({
    "ts": ts, "day": day, "entries": _num(entries),
    "transcript": transcript, "outcome": outcome, "model": model,
    "turns": _num(turns), "tokens": _num(tokens), "cost_usd": _num(cost),
    "input": _num(inp), "output": _num(out),
    "cache_read": _num(cr), "cache_creation": _num(cc),
}))
PY
}

# ── Retry counters (sidecar JSONL keyed by transcript_path) ───────────────────
get_retry_count() {
    local transcript="$1"
    if [[ ! -f "$RETRY_FILE" ]]; then echo 0; return; fi
    # Most-recent wins: walk the file and keep last numeric for this transcript.
    python3 - "$transcript" "$RETRY_FILE" <<'PY'
import json, sys
key = sys.argv[1]
path = sys.argv[2]
count = 0
try:
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except Exception:
                continue
            if e.get("transcript") == key:
                count = int(e.get("count", count))
except FileNotFoundError:
    pass
print(count)
PY
}

bump_retry_count() {
    local transcript="$1"
    local current
    current=$(get_retry_count "$transcript")
    local next=$((current + 1))
    local ts
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    # json.dumps so a transcript path with a quote/backslash can't corrupt the
    # retry log (get_retry_count parses it to decide poison). The python stdout
    # is redirected into the file; the echo below is this function's return.
    python3 - "$ts" "$transcript" "$next" >> "$RETRY_FILE" <<'PY'
import json, sys
ts, transcript, count = sys.argv[1], sys.argv[2], sys.argv[3]
print(json.dumps({"ts": ts, "transcript": transcript, "count": int(count)}))
PY
    echo "$next"
}

# ── Queue rewrite (atomic) ────────────────────────────────────────────────────
# Take the original queue and a list of transcript paths whose entries were
# successfully drained or poisoned, and rewrite the queue without those lines.
rewrite_queue() {
    local removed_list="$1"  # newline-delimited transcript paths to drop
    if [[ ! -s "$QUEUE_FILE" ]]; then return 0; fi
    local tmp
    tmp=$(mktemp "${QUEUE_FILE}.XXXXXX")
    python3 - "$QUEUE_FILE" "$removed_list" "$tmp" <<'PY'
import json, sys
queue_path, removed_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
removed = set()
try:
    with open(removed_path) as f:
        for line in f:
            line = line.strip()
            if line:
                removed.add(line)
except FileNotFoundError:
    pass
kept = 0
with open(queue_path) as src, open(out_path, "w") as dst:
    for line in src:
        s = line.strip()
        if not s:
            continue
        try:
            e = json.loads(s)
        except Exception:
            # Preserve malformed lines so we don't silently lose data.
            dst.write(line if line.endswith("\n") else line + "\n")
            kept += 1
            continue
        tp = e.get("transcript_path", "")
        if tp in removed:
            continue
        dst.write(line if line.endswith("\n") else line + "\n")
        kept += 1
print(kept)
PY
    mv "$tmp" "$QUEUE_FILE"
}

# ── Process a single entry ────────────────────────────────────────────────────
# Returns 0 on success, 1 on retryable failure, 2 on poison/skip-permanently.
process_entry() {
    local entry_json="$1"
    local transcript session_id trigger
    transcript=$(printf '%s' "$entry_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("transcript_path",""))' 2>/dev/null || echo "")
    session_id=$(printf '%s' "$entry_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("session_id","unknown"))' 2>/dev/null || echo "unknown")
    trigger=$(printf '%s' "$entry_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("trigger","unknown"))' 2>/dev/null || echo "unknown")

    if [[ -z "$transcript" ]]; then
        log "  skip: entry has no transcript_path"
        return 2
    fi

    if [[ ! -f "$transcript" ]]; then
        log "  skip-stale: transcript missing on disk: $transcript"
        emit_error warn drain_stale "transcript missing: $transcript" "$transcript"
        record_cost_event 0 "$transcript" "stale"
        return 2  # treat as permanent skip — drop from queue
    fi

    local retry
    retry=$(get_retry_count "$transcript")
    if [[ "$retry" -ge "$MAX_RETRIES" ]]; then
        log "  poison: $transcript (retries=$retry >= $MAX_RETRIES); archiving"
        emit_error error drain_poison "poison after $retry retries: $transcript" "$transcript"
        printf '%s\n' "$entry_json" >> "$POISON_FILE"
        record_cost_event 0 "$transcript" "poison"
        return 2
    fi

    log "  process: session=$session_id trigger=$trigger retries=$retry transcript=$transcript"

    # ── Cascade (W4): gate + slice before any model spend ──────────────────────
    # Default-on. Skips reflect-on-reflect / no-signal / already-captured for $0,
    # and shrinks the input from the full transcript to just the signal-bearing
    # windows (~10x) so /reflect runs cheap on Sonnet with a low turn budget.
    local reflect_target="$transcript" slice_path=""
    if [[ "$CASCADE_ENABLED" == "1" && -f "$CASCADE_SCRIPT" ]]; then
        local prep_json prep_action prep_reason prep_slice
        # prepare exits 0=reflect / 1=skip but ALWAYS prints valid JSON to
        # stdout. Capture stdout directly — do NOT `|| echo {}`, which would
        # append a second object and corrupt the parse. Empty (true crash)
        # falls through to the "reflect" default below (fail-open).
        prep_json=$(python3 "$CASCADE_SCRIPT" prepare "$transcript" 2>>"$LOG_FILE")
        [[ -z "$prep_json" ]] && prep_json='{}'
        prep_action=$(printf '%s' "$prep_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("action","reflect"))' 2>/dev/null || echo "reflect")
        prep_reason=$(printf '%s' "$prep_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("reason",""))' 2>/dev/null || echo "")
        if [[ "$prep_action" == "skip" ]]; then
            log "  cascade skip ($prep_reason): no model spend"
            record_cost_event 0 "$transcript" "skip_${prep_reason//[^a-zA-Z0-9_]/_}"
            return 2  # permanent skip — drop from queue
        fi
        prep_slice=$(printf '%s' "$prep_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("slice_path") or "")' 2>/dev/null || echo "")
        if [[ -n "$prep_slice" && -f "$prep_slice" ]]; then
            reflect_target="$prep_slice"
            slice_path="$prep_slice"
            log "  cascade: sliced transcript -> $prep_slice (reflecting on slice)"
        fi
    fi

    if [[ "$DRY_RUN" == "1" ]]; then
        log "    DRY_RUN=1 → would have called: $CLAUDE_BIN -p --model $DRAIN_MODEL ... /reflect $reflect_target"
        [[ -n "$slice_path" ]] && rm -f "$slice_path"
        record_cost_event 1 "$transcript" "dry_run"
        return 0
    fi

    # Build the prompt. The /reflect skill analyzes whatever path we hand it —
    # the cascade slice when enabled, else the full transcript.
    local prompt
    prompt="/reflect

Process the transcript at: ${reflect_target}

Extract any HIGH-confidence corrections, MEDIUM-confidence approved approaches, and noteworthy patterns. Write each as a learning document via the standard reflect workflow. When done, summarize what you captured. Do NOT touch the queue file — the drain script handles archiving."

    local out_json exit_code
    # Neutral cwd (W5): the bg drainer inherits the cwd of whatever session
    # triggered it, so reflect used to run inside a random repo (the incident
    # ran in research-tech while analysing a cochilli transcript). Pin it to a
    # neutral dir so reflect can't accidentally touch a project tree.
    out_json=$(cd "$DRAIN_CWD" && _to "$ENTRY_TIMEOUT" "$CLAUDE_BIN" \
        -p "$prompt" \
        --model "$DRAIN_MODEL" \
        --output-format json \
        --permission-mode bypassPermissions \
        --max-turns "$MAX_TURNS" 2>>"$LOG_FILE")
    exit_code=$?

    # Slice is consumed — remove it regardless of how the run turns out.
    [[ -n "$slice_path" ]] && rm -f "$slice_path"

    # We expect a JSON object on stdout regardless of exit code (claude -p
    # writes the result envelope even on max_turns / errors).
    local is_error result_summary cost terminal_reason num_turns total_tokens
    is_error=$(printf '%s' "$out_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("is_error", True))' 2>/dev/null || echo "True")
    result_summary=$(printf '%s' "$out_json" | python3 -c 'import json,sys; r=json.load(sys.stdin).get("result","")[:200]; print(r.replace(chr(10)," | "))' 2>/dev/null || echo "")
    cost=$(printf '%s' "$out_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("total_cost_usd","?"))' 2>/dev/null || echo "?")
    terminal_reason=$(printf '%s' "$out_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("terminal_reason",""))' 2>/dev/null || echo "")
    num_turns=$(printf '%s' "$out_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("num_turns",0))' 2>/dev/null || echo "0")
    # Extract every token bucket the result envelope reports — for the budget
    # check AND the cost timeline (W3 cached-vs-uncached split).
    local input_tok output_tok cache_read cache_creation usage_line
    usage_line=$(printf '%s' "$out_json" | python3 -c '
import json,sys
try:
    u=json.load(sys.stdin).get("usage",{}) or {}
except Exception:
    u={}
i=int(u.get("input_tokens",0) or 0); o=int(u.get("output_tokens",0) or 0)
cr=int(u.get("cache_read_input_tokens",0) or 0); cc=int(u.get("cache_creation_input_tokens",0) or 0)
print(i,o,cr,cc,i+o+cr+cc)' 2>/dev/null || echo "0 0 0 0 0")
    read -r input_tok output_tok cache_read cache_creation total_tokens <<< "$usage_line"
    [[ "$total_tokens" =~ ^[0-9]+$ ]] || { input_tok=0; output_tok=0; cache_read=0; cache_creation=0; total_tokens=0; }
    # Sanitize cost ("?" on parse failure) to a JSON-safe number.
    [[ "$cost" =~ ^[0-9]+([.][0-9]+)?$ ]] || cost=0

    # Fatal subprocess errors (signal, timeout, process couldn't start) — no JSON.
    if [[ -z "$out_json" ]]; then
        log "    claude -p produced no output (exit=$exit_code); likely timeout or auth issue"
        emit_error error drain_no_output "claude -p produced no output (exit=$exit_code)" "$transcript"
        bump_retry_count "$transcript" >/dev/null
        record_cost_event 1 "$transcript" "fail_no_output_exit_${exit_code}"
        return 1
    fi

    # ── Token-budget circuit breaker (post-hoc poison) ─────────────────────────
    # claude -p only reports usage at completion, so turns + wall-clock are the
    # mid-run hard stops; this catches a run that finished but cost too much and
    # poisons the transcript so a retry can never repeat the spend.
    if [[ "$total_tokens" -gt "$TOKEN_MAX" ]]; then
        log "    BUDGET poison: run used ${total_tokens} tokens (> ${TOKEN_MAX}); archiving transcript"
        emit_error error drain_budget_exceeded "run used ${total_tokens} tokens (> ${TOKEN_MAX})" "$transcript"
        printf '%s\n' "$entry_json" >> "$POISON_FILE"
        record_cost_event 1 "$transcript" "poison_budget" "$total_tokens" "$cost" "$num_turns" "$DRAIN_MODEL" "$cache_read" "$cache_creation" "$input_tok" "$output_tok"
        return 2
    fi

    # max_turns: claude probably did useful work — write_flow may have already
    # written learnings to disk. Treat as "made progress" and remove from queue
    # to avoid re-spending cost; the retry counter still bumps so repeated
    # max_turns on the same transcript eventually poisons it.
    if [[ "$terminal_reason" == "max_turns" ]]; then
        local retries_after
        retries_after=$(bump_retry_count "$transcript")
        log "    partial: terminal=max_turns turns=${num_turns} cost=\$${cost} tokens=${total_tokens} retries=${retries_after}"
        record_cost_event 1 "$transcript" "partial_max_turns" "$total_tokens" "$cost" "$num_turns" "$DRAIN_MODEL" "$cache_read" "$cache_creation" "$input_tok" "$output_tok"
        # If we've hit max_turns repeatedly, give up and drop from queue.
        if [[ "$retries_after" -ge "$MAX_RETRIES" ]]; then
            emit_error warn drain_max_turns_exhausted "max_turns hit $MAX_RETRIES times" "$transcript"
            return 2
        fi
        return 1  # leave in queue for another shot with fresh budget
    fi

    if [[ "$is_error" == "True" || "$is_error" == "true" ]]; then
        log "    claude reported is_error=true terminal=${terminal_reason} result=${result_summary}"
        bump_retry_count "$transcript" >/dev/null
        record_cost_event 1 "$transcript" "fail_is_error" "$total_tokens" "$cost" "$num_turns" "$DRAIN_MODEL" "$cache_read" "$cache_creation" "$input_tok" "$output_tok"
        return 1
    fi

    if [[ $exit_code -ne 0 ]]; then
        log "    claude -p exit=$exit_code (but is_error=false; treating as soft fail)"
        bump_retry_count "$transcript" >/dev/null
        record_cost_event 1 "$transcript" "fail_exit_${exit_code}" "$total_tokens" "$cost" "$num_turns" "$DRAIN_MODEL" "$cache_read" "$cache_creation" "$input_tok" "$output_tok"
        return 1
    fi

    log "    OK turns=${num_turns} cost=\$${cost} tokens=${total_tokens} result=${result_summary}"
    record_cost_event 1 "$transcript" "ok" "$total_tokens" "$cost" "$num_turns" "$DRAIN_MODEL" "$cache_read" "$cache_creation" "$input_tok" "$output_tok"
    return 0
}

# ── Main ──────────────────────────────────────────────────────────────────────
main() {
    log "──── drain start (pid=$$ model=$DRAIN_MODEL max_per_run=$MAX_PER_RUN daily_max=$DAILY_MAX max_turns=$MAX_TURNS timeout=${ENTRY_TIMEOUT}s token_max=$TOKEN_MAX dry_run=$DRY_RUN) ────"

    if [[ ! -s "$QUEUE_FILE" ]]; then
        log "queue empty or missing; nothing to do"
        return 0
    fi

    acquire_lock

    # Atomic under the lock: collapse a burst of session starts to one drain.
    if ! debounce_ok; then
        return 0
    fi

    local already_today
    already_today=$(today_drain_count | tr -d '[:space:]')
    if [[ "$already_today" =~ ^[0-9]+$ ]] && [[ "$already_today" -ge "$DAILY_MAX" ]]; then
        log "daily cap reached (today=$already_today >= $DAILY_MAX); exiting"
        return 0
    fi

    # Compute remaining headroom for today.
    local headroom=$((DAILY_MAX - already_today))
    local run_max="$MAX_PER_RUN"
    if [[ "$headroom" -lt "$run_max" ]]; then
        run_max="$headroom"
    fi

    log "today_count=$already_today headroom=$headroom run_max=$run_max"

    # Read up to $run_max non-empty lines from the queue.
    local processed_list_file
    processed_list_file=$(mktemp)
    # shellcheck disable=SC2064
    trap "release_lock; rm -f $processed_list_file" EXIT INT TERM

    local count=0
    local ok=0 fail=0 perm=0
    while IFS= read -r line; do
        line="${line#"${line%%[![:space:]]*}"}"  # ltrim
        [[ -z "$line" ]] && continue
        if [[ "$count" -ge "$run_max" ]]; then break; fi
        count=$((count + 1))

        log "[entry $count/$run_max]"
        process_entry "$line"
        local rc=$?
        case $rc in
            0)
                ok=$((ok + 1))
                # Extract transcript_path and add to processed list for queue rewrite.
                printf '%s\n' "$line" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("transcript_path",""))' >> "$processed_list_file"
                ;;
            2)
                # Permanent skip (stale / poison) — also remove from queue.
                perm=$((perm + 1))
                printf '%s\n' "$line" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("transcript_path",""))' >> "$processed_list_file"
                ;;
            *)
                # Retryable failure — leave in queue.
                fail=$((fail + 1))
                ;;
        esac
    done < "$QUEUE_FILE"

    if [[ -s "$processed_list_file" ]]; then
        local kept
        kept=$(rewrite_queue "$processed_list_file")
        log "queue rewritten: kept=$kept entries"
    fi

    log "summary: processed=$count ok=$ok perm_skip=$perm retryable_fail=$fail"

    # Reindex if anything succeeded. Never in DRY_RUN (a dry run must have no
    # side effects beyond logging) or when explicitly skipped (tests).
    if [[ "$ok" -gt 0 && "$DRY_RUN" != "1" && "${REFLECT_DRAIN_SKIP_REINDEX:-0}" != "1" ]]; then
        if ! command -v reflect >/dev/null 2>&1; then
            log "reindex SKIP: 'reflect' CLI not on PATH"
            log "  → install reflect-kb to enable GraphRAG reindex of new learnings:"
            log "      uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'"
            log "  → without it, learnings are still captured to disk; just won't appear in /recall"
            log "    until a manual 'reflect reindex' runs"
        else
            # Self-heal the graphml BEFORE reindex (W5). A doubled close-tag
            # corruption ("not well-formed: invalid token") is what the incident
            # agent spent ~200 turns investigating — here it's a cheap batch
            # step that repairs or flags for full rebuild, never an agent loop.
            local repair_script="${SCRIPT_DIR}/../scripts/graphml_repair.py"
            if [[ -f "$repair_script" ]]; then
                if python3 "$repair_script" --repair --quiet >>"$LOG_FILE" 2>&1; then
                    log "graphml validated/repaired OK"
                else
                    log "graphml corrupt + unrepairable; reindex may need --force rebuild"
                    emit_error warn graphml_corrupt "graphml unrepairable by truncate; full rebuild advised" ""
                fi
            fi
            log "running reflect reindex (incremental)"
            if _to 300 reflect reindex >>"$LOG_FILE" 2>&1; then
                log "reindex OK"
            else
                log "reindex returned non-zero (continuing; not fatal)"
                emit_error error reindex_fail "reflect reindex non-zero exit" ""
            fi
        fi
    fi

    log "──── drain end ────"
}

# Surface missing reflect CLI at SessionStart, not just on first drain failure.
# Drain still runs (enqueue/dequeue logging works without reflect-kb), but
# recall stays empty until reflect-kb is installed.
if [[ "${REFLECT_QUIET_INSTALL_WARNING:-0}" != "1" ]]; then
    if ! command -v reflect >/dev/null 2>&1 && [[ ! -x "${HOME}/.local/bin/reflect" ]]; then
        cat >&2 <<'EOF'
[reflect-kb] CLI not found on PATH.

  Learnings will be queued and child sessions can write .md/.entities.yaml
  files, but `reflect reindex` and `reflect search` will not work — recall
  will be empty.

  Install:
    uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'

  Set REFLECT_QUIET_INSTALL_WARNING=1 to suppress this message.
EOF
    fi
fi

main "$@" || true
exit 0
