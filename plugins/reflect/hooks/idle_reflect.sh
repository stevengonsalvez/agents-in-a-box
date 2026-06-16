#!/usr/bin/env bash
# idle_reflect.sh — idle-session reflection sweeper (SG3).
#
# Purpose
# -------
# Runs on a launchd timer (com.reflect.idle.plist). Today reflect captures
# only at Stop or PreCompact — explicit session ends. Sessions that simply
# go quiet (user steps away, switches context) never fire either hook, so
# their lessons are lost. This sweep watches transcript mtimes under
# ~/.claude/projects/*/ and enqueues sessions idle for longer than the
# threshold with trigger='idle'. The drain prompt tags the resulting
# learnings 'speculative' (the session may still resume), and recall
# down-ranks that tag.
#
# Design
# ------
# - Thin wrapper: all scan / dedup / enqueue logic lives in
#   scripts/reflect_gate.py --idle-sweep (stdlib-only, testable in-process).
# - Double-process safe: a per-(path, mtime) idle-state file stops
#   re-evaluating a still-idle session, and the gate's queue/cost-log dedup
#   stops re-enqueueing after a resume (see idle_sweep docstring).
# - Always exits 0 so launchd never flags the job as failing.
#
# Configuration (env)
# -------------------
# REFLECT_IDLE_THRESHOLD_SEC   Quiet seconds before a session is idle. Default: 600
# REFLECT_IDLE_MAX_AGE_SEC     Ignore transcripts older than this.      Default: 86400
# REFLECT_IDLE_MAX_PER_SWEEP   Max enqueues per sweep.                  Default: 5
# REFLECT_IDLE_PROJECTS_ROOT   Transcript root.            Default: ~/.claude/projects
# REFLECT_IDLE_LOG_MAX_BYTES   idle.log rotation threshold.             Default: 1048576
# REFLECT_STATE_DIR            State dir.                               Default: ~/.reflect
# REFLECT_IDLE_DISABLED        If "1", the sweep is a hard no-op.       Default: 0
# REFLECT_DISABLED             Global kill switch (shared with drain).  Default: 0

set -uo pipefail

# ── Hard kill switches ───────────────────────────────────────────────────────
# Honoured before any work so an operator can stop all sweeps instantly.
if [[ "${REFLECT_DISABLED:-0}" == "1" || "${REFLECT_IDLE_DISABLED:-0}" == "1" ]]; then
    exit 0
fi

# ── Config ────────────────────────────────────────────────────────────────────
STATE_DIR="${REFLECT_STATE_DIR:-$HOME/.reflect}"
LOG_FILE="${STATE_DIR}/idle.log"
LOG_MAX_BYTES="${REFLECT_IDLE_LOG_MAX_BYTES:-1048576}"

# Locate the gate relative to this hook, robust to symlinks.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE_SCRIPT="${SCRIPT_DIR}/../scripts/reflect_gate.py"

mkdir -p "$STATE_DIR"

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

if [[ ! -f "$GATE_SCRIPT" ]]; then
    log "idle sweep SKIP: reflect_gate.py not found at $GATE_SCRIPT"
    exit 0
fi

# The gate reads REFLECT_IDLE_* / REFLECT_STATE_DIR from env itself; this
# wrapper only adds logging and the kill switches.
summary=$(python3 "$GATE_SCRIPT" --idle-sweep 2>>"$LOG_FILE")
if [[ -n "$summary" ]]; then
    log "idle sweep: $summary"
else
    log "idle sweep: gate produced no output (see stderr above)"
fi

exit 0
