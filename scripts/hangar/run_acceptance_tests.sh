#!/usr/bin/env bash
# Run the Hangar control-plane ACCEPTANCE tests — the framed-socket RPC,
# migration-upgrade, and CLI-surface integration tests that prove each feature
# bead end to end.
#
# Why this exists (the gate gap it closes):
#   * CI's `test` job runs `cargo nextest run --lib` — LIB unit tests only, so
#     NOTHING under `tests/` is built or run there.
#   * CI's `hangar-e2e` job runs `run_all_tripwires.sh` — `tripwire_*` only.
#   So the framed-socket + CLI acceptance proofs (rpc_event_push,
#   rpc_issue_update, rpc_comment_add, rpc_comment_mention_spawn,
#   migration_*_upgrade, hangar_cli_integration, …) were gated by NOTHING in CI
#   and only passed because they were run locally. This script gates them.
#
# Scope: every NON-`tripwire_*` integration test target in the three hangar
# crates (auto-globbed, so new acceptance tests are covered without editing this
# file), PLUS the two hangar acceptance targets in the `ainb` crate, run by
# EXPLICIT --test name — the `ainb` crate's other integration targets
# (tests/ui_tests.rs, tests/behavioral/, …) carry pre-existing `NewSessionState`
# drift that fails to compile and is unrelated to Hangar; naming the targets
# explicitly avoids building them.
#
#   bash scripts/hangar/run_acceptance_tests.sh
set -o pipefail
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE="$REPO_ROOT/ainb-tui"
cd "$WORKSPACE"

failures=0
ran=0
skips=0

# run_one <package> <test-binary-name> [extra cargo args…]
run_one() {
    local pkg="$1"
    local test_name="$2"
    shift 2
    ran=$((ran + 1))
    local out
    # --nocapture so a test's `SKIP:` eprintln is visible (libtest swallows a
    # passing test's output otherwise) — a vacuous skip (e.g. the sandbox
    # confinement test on a Linux host without Landlock) must NOT read as a
    # genuinely-green run.
    if out="$(cargo test -p "$pkg" "$@" --test "$test_name" -- --test-threads=1 --nocapture 2>&1)"; then
        if printf '%s\n' "$out" | grep -q '^SKIP:'; then
            printf 'SKIP %s::%s\n' "$pkg" "$test_name"
            printf '%s\n' "$out" | grep '^SKIP:' | head -3
            skips=$((skips + 1))
        else
            printf 'ok   %s::%s\n' "$pkg" "$test_name"
        fi
    else
        printf 'FAIL %s::%s\n' "$pkg" "$test_name" >&2
        printf '%s\n' "$out" | tail -25 >&2
        failures=$((failures + 1))
    fi
}

# ── hangar crates: every non-tripwire, non-helper integration test target.
for pkg in ainb-hangar-store ainb-hangar-proto ainb-hangar-sandbox ainb-hangar-daemon; do
    for f in crates/"$pkg"/tests/*.rs; do
        [ -e "$f" ] || continue
        name="$(basename "$f" .rs)"
        case "$name" in
            tripwire_*) continue ;;   # covered by run_all_tripwires.sh
            *_common) continue ;;     # shared helper, not a test binary
            # Pre-existing flaky cross-process-serialization test (passes macOS,
            # interleaves on CI Linux's scheduler). Never CI-gated before this
            # acceptance gate existed; excluded until the BdClient serialization
            # is hardened. Tracked separately — NOT an e38 feature test.
            beads_adapter) continue ;;
        esac
        run_one "$pkg" "$name"
    done
done

# ── ainb crate: every Hangar acceptance target. Globbed by the `hangar_*` /
#    `tripwire_hangar_*` prefix (so new ones — daemon-lifecycle, webhook CLI, … —
#    gate automatically) but NOT a blanket `--tests`: the ainb crate's other
#    integration targets (tests/ui_tests.rs, tests/behavioral/, …) carry
#    pre-existing NewSessionState drift that fails to compile and is unrelated to
#    Hangar; naming the Hangar prefix avoids building them.
for f in crates/ainb-core/tests/hangar_*.rs crates/ainb-core/tests/tripwire_hangar_*.rs; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    run_one ainb "$name"
done

echo "─────────────────────────────────────────"
echo "hangar acceptance: ran=$ran failed=$failures skipped=$skips"
exit "$failures"
