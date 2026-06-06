#!/usr/bin/env bash
# diagnose-tripwire.sh — run when a tripwire fails for unclear reasons.
#
# Walks through the common silent-trap diagnostics:
#   1. Verify staged plugin binaries are AMFI-clean (exit !=137 standalone)
#   2. Tail the latest JSONL log for plugin lifecycle events
#   3. Show what onboarding.toml the test would seed
#
# Usage:
#   ./diagnose-tripwire.sh                 # runs all checks
#   ./diagnose-tripwire.sh amfi            # AMFI standalone probe only
#   ./diagnose-tripwire.sh logs            # last log tail only
#
# Run from the ainb-tui workspace root.
set -euo pipefail

cd "$(dirname "$0")/../../../.."  # ainb-tui workspace root

CHECK="${1:-all}"

probe_amfi() {
    echo "=== AMFI standalone probe ==="
    # AMFI-killed binaries exit 137 in <1ms with no stderr, regardless
    # of whether stdin is empty. Healthy binaries with </dev/null read
    # EOF and exit cleanly (0), or wait for input until timeout.
    for bin in dist/plugins/*/[!.]*; do
        [[ -x "$bin" && ! -d "$bin" ]] || continue
        local name="$(basename "$bin")"
        # Run with /dev/null stdin and a 1s ceiling. Track wall time —
        # AMFI kills are instant; honest exits take at least a few ms.
        local start ms exit_code stderr_size
        start=$(perl -e 'use Time::HiRes qw(time); print int(time()*1000)')
        ./"$bin" </dev/null >/dev/null 2>/tmp/.amfi-probe.stderr &
        local pid=$!
        # Wait up to 1s — if it's still alive, kill it.
        local waited=0
        while [[ $waited -lt 10 ]] && kill -0 "$pid" 2>/dev/null; do
            sleep 0.1
            waited=$((waited + 1))
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null
            wait "$pid" 2>/dev/null
            exit_code="killed-by-us"
        else
            wait "$pid" 2>/dev/null
            exit_code=$?
        fi
        ms=$(($(perl -e 'use Time::HiRes qw(time); print int(time()*1000)') - start))
        stderr_size=$(wc -c </tmp/.amfi-probe.stderr 2>/dev/null | tr -d ' ')

        if [[ "$exit_code" == "137" && "$ms" -lt 100 && "$stderr_size" == "0" ]]; then
            echo "  ❌ $name: exit=137 in ${ms}ms, no stderr — AMFI silent kill"
            echo "       Fix: just stage-plugins   (re-signs with codesign --sign -)"
        elif [[ "$exit_code" == "0" || "$exit_code" == "killed-by-us" ]]; then
            echo "  ✅ $name: ran ${ms}ms, exit=$exit_code"
        else
            echo "  ⚠  $name: exit=$exit_code in ${ms}ms (stderr: ${stderr_size}B)"
        fi
    done
    rm -f /tmp/.amfi-probe.stderr
}

probe_logs() {
    echo "=== Latest JSONL log key events ==="
    local log_dir="$HOME/.agents-in-a-box/logs"
    if [[ ! -d "$log_dir" ]]; then
        echo "  (no logs at $log_dir — has ainb tui run?)"
        return
    fi
    local latest
    latest=$(ls -t "$log_dir"/*.jsonl 2>/dev/null | head -1 || true)
    if [[ -z "$latest" ]]; then
        echo "  (no jsonl files in $log_dir)"
        return
    fi
    echo "  Log: $latest"
    if command -v jq >/dev/null 2>&1; then
        jq -r 'select(.fields.message | test("plugin|spawn|broken|pipe|exit|on_init|inbound|host->plugin"; "i"))
               | "  \(.timestamp[11:23]) [\(.level)] plugin=\(.fields.plugin // "?") \(.fields.message)"' \
           "$latest" 2>/dev/null | head -30
    else
        echo "  (install jq for filtered output)"
        head -30 "$latest"
    fi
}

probe_wizard_seed() {
    echo "=== Wizard-skip onboarding.toml the test should seed ==="
    local ver
    ver=$(awk '/^\[workspace\.package\]/{p=1} p && /^version *=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
    cat <<EOF
  completed = true
  completed_at = "2026-05-11T00:00:00+00:00"
  version = "$ver"
  skipped_dependencies = []
  git_directories = []
EOF
    echo "  ↑ write this to \$HOME_TEMP/.agents-in-a-box/config/onboarding.toml"
}

case "$CHECK" in
    amfi)   probe_amfi ;;
    logs)   probe_logs ;;
    wizard) probe_wizard_seed ;;
    all|*)
        probe_amfi
        echo
        probe_logs
        echo
        probe_wizard_seed
        ;;
esac
