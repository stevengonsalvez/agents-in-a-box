#!/usr/bin/env bash
# Run every Hangar `tripwire_*` integration test as the authoritative
# end-to-end gate for the Hangar control-plane (P0-P9).
#
# The suite spans three workspace locations:
#
#   crates/ainb-hangar-daemon/tests/tripwire_*.rs   P4 per-screen + P7 autopilot
#                                                   + P8 kanban/health/otel
#                                                   + P9 pr_capture/pr_badge + …
#   crates/ainb-hangar-store/tests/tripwire_*.rs    P0 sqlx migration determinism
#   plugins/hangar-tui/tests/tripwire_*.rs          P3.8/P4.10 plugin↔daemon roundtrip
#
# Conventions (deliberately mirroring the repo, NOT the stale P9 plan):
#
#   * `set -o pipefail` so a failing `cargo test` is never masked by a
#     downstream pipe (`reference_tail_masks_pipe_exit`).
#   * `--test-threads=1` for the whole suite: several tripwires mutate the
#     process env (`ENV_LOCK`-guarded) and bind per-test sockets; serialising
#     keeps cross-process/within-binary env races deterministic
#     (`reference_env_lock_for_parallel_tests`).
#   * The OTLP export tripwire is `#![cfg(feature = "otlp")]` — without the
#     feature its file compiles to nothing — so it runs in a SEPARATE
#     `cargo test --features otlp` invocation.
#   * Exit code = number of failed tripwire binaries. On failure the offending
#     tripwire name is printed to stderr so CI surfaces it directly.
#
# PREREQUISITES (the caller stages these; CI does it as explicit steps):
#   * `cargo build -p ainb-hangar-daemon --bin ainb-hangar-daemon`
#   * `bash scripts/build-plugins.sh`  (stages dist/plugins/hangar-tui/)
# The plugin↔daemon roundtrip tripwire needs the staged plugin + daemon binary.
#
# Run from the repo root OR anywhere — the script cd's to the ainb-tui
# workspace itself.
#
#   bash scripts/hangar/run_all_tripwires.sh
set -o pipefail
set -u

# Resolve the ainb-tui workspace relative to this script (repo_root/scripts/hangar/..).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE="$REPO_ROOT/ainb-tui"
cd "$WORKSPACE"

failures=0
ran=0

# run_one <package> <test-binary-name> [extra cargo args…]
run_one() {
    local pkg="$1"
    local test_name="$2"
    shift 2
    ran=$((ran + 1))
    local out
    if out="$(cargo test -p "$pkg" "$@" --test "$test_name" -- --test-threads=1 2>&1)"; then
        printf 'ok   %s::%s\n' "$pkg" "$test_name"
    else
        printf 'FAIL %s::%s\n' "$pkg" "$test_name" >&2
        printf '%s\n' "$out" | tail -20 >&2
        failures=$((failures + 1))
    fi
}

# ── ainb-hangar-daemon: every tripwire_* except the otlp one (run below with
#    --features otlp) and the *_common helper (no #[test], not a binary).
for f in crates/ainb-hangar-daemon/tests/tripwire_*.rs; do
    name="$(basename "$f" .rs)"
    case "$name" in
        *_common) continue ;;
        tripwire_otel_export_when_endpoint_set) continue ;;
    esac
    run_one ainb-hangar-daemon "$name"
done

# ── OTLP export tripwire — feature-gated, separate invocation.
run_one ainb-hangar-daemon tripwire_otel_export_when_endpoint_set --features otlp

# ── ainb-hangar-store: sqlx migration determinism.
for f in crates/ainb-hangar-store/tests/tripwire_*.rs; do
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    run_one ainb-hangar-store "$name"
done

# ── plugins/hangar-tui: plugin↔daemon roundtrip (needs staged plugin + daemon).
for f in "$REPO_ROOT"/plugins/hangar-tui/tests/tripwire_*.rs; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    run_one ainb-plugin-hangar "$name"
done

echo "─────────────────────────────────────────"
echo "hangar tripwires: ran=$ran failed=$failures"
exit "$failures"
