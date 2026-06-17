#!/usr/bin/env bash
# Soak watch for the session-reader incremental-refresh contract.
#
# Tails the host JSONL logs (~/.agents-in-a-box/logs/) for the
# counter line session-reader emits via host.log() after every refresh:
#
#   session-reader: incremental refresh statted=N parsed=N cache_hits=N
#     stable_skipped=N stable_reused=true|false rebuilt=true|false
#
# and samples the CPU of every running session-reader process. What a
# healthy steady state looks like:
#
#   - refreshes triggered by file changes report a small parsed=N
#   - a refresh with NO session-log changes reports parsed=0 and
#     stable_reused=true  ← the original 97%-CPU bug is dead when you
#     see this repeatedly
#   - rebuilt=true should appear at most ~once/day (a file aged past
#     the window) or after a hard refresh / flush
#   - session-reader CPU returns to ~0% between refreshes
#
# Usage:
#   scripts/soak-session-reader.sh            # follow logs + CPU
#   scripts/soak-session-reader.sh --history  # one-shot: grep existing logs, no follow
#
# Run one per machine (it watches every TUI's log file at once).
set -euo pipefail

LOG_DIR="${AINB_LOG_DIR:-$HOME/.agents-in-a-box/logs}"
MODE="${1:-follow}"

if [[ ! -d "$LOG_DIR" ]]; then
    echo "no log dir at $LOG_DIR — start an ainb TUI first" >&2
    exit 1
fi

# The counter line + cold-start + hard-refresh markers.
PATTERN='incremental refresh statted=|built full usage history|HARD refresh requested|flush_cache requested'

# Pull the interesting fields out of a JSONL line. jq if present,
# otherwise pass the raw line through.
pretty() {
    if command -v jq >/dev/null 2>&1; then
        jq --unbuffered -r '
            select(.fields.message? // "" | test("'"$PATTERN"'")) |
            "\(.timestamp)  \(.fields.plugin // "session-reader")  \(.fields.message)"
        ' 2>/dev/null
    else
        grep --line-buffered -E "$PATTERN"
    fi
}

newest_logs() {
    # Every TUI writes its own timestamped file — watch the 6 newest so
    # a multi-instance soak sees all of them.
    ls -t "$LOG_DIR"/agents-in-a-box-*.jsonl 2>/dev/null | head -6
}

if [[ "$MODE" == "--history" ]]; then
    echo "── refresh history (all logs) ──────────────────────────────"
    cat "$LOG_DIR"/agents-in-a-box-*.jsonl 2>/dev/null | pretty
    exit 0
fi

LOGS=$(newest_logs)
if [[ -z "$LOGS" ]]; then
    echo "no JSONL logs in $LOG_DIR yet — start an ainb TUI first" >&2
    exit 1
fi

echo "── soaking session-reader ──────────────────────────────────────"
echo "logs:"
printf '  %s\n' $LOGS
echo "watch for: parsed=0 stable_reused=true on no-change refreshes,"
echo "and session-reader CPU ~0%% between refreshes. Ctrl-C to stop."
echo "────────────────────────────────────────────────────────────────"

# CPU sampler: one line every 10s, only when a session-reader runs.
(
    while true; do
        ps -Ao pcpu,pid,etime,comm \
            | grep -E "[s]ession-reader$|[a]inb-plugin-session-reader" \
            | while read -r pcpu pid etime comm; do
                printf 'cpu   %s  pid=%s  %%cpu=%s  up=%s\n' \
                    "$(date '+%Y-%m-%dT%H:%M:%S')" "$pid" "$pcpu" "$etime"
            done
        sleep 10
    done
) &
CPU_PID=$!
trap 'kill "$CPU_PID" 2>/dev/null' EXIT

# tail -F follows across rotations; new TUI instances create NEW files,
# so re-run the script after launching additional TUIs mid-soak.
# shellcheck disable=SC2086
tail -F -n +1 $LOGS 2>/dev/null | pretty
