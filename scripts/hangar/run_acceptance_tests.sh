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
# Shape: ONE `cargo nextest run` per package, not one `cargo test --test X` per
# file. The per-file loop spawned one cargo process per target (227 today), each
# re-resolving the dependency graph, re-checking every fingerprint, and
# relinking before running a single binary; that overhead, not the tests, was
# most of this script's CI wall-clock. nextest takes the target list as repeated
# `--test` flags, so the globs below still decide exactly what runs, including
# the `ainb` crate's explicit prefix list, which must NOT become a blanket
# `--tests`.
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

# Binary ids nextest reported as anything other than a pass. A status line is
# `<STATUS> [ 1.234s] (3/68) pkg::target test_name`, so the binary id is the
# first field containing `::`.
failed_binaries() {
    awk '/^ *(FAIL|TIMEOUT|LEAK-FAIL|SIG[A-Z]+) \[/ {
        for (i = 1; i <= NF; i++) if ($i ~ /::/) { print $i; break }
    }' | sort -u
}

# Binary ids that printed a `SKIP:` line. Scanned only in the trailing
# `--success-output final` block, i.e. only for tests that PASSED, which is the
# vacuous-green signal the old per-file loop grepped for.
skipped_binaries() {
    awk '
        /^ *Summary \[/ { tail = 1; next }
        !tail { next }
        /^ *(PASS|SLOW) \[/ { id = ""
            for (i = 1; i <= NF; i++) if ($i ~ /::/) { id = $i; break }
            next }
        /SKIP:/ { if (id != "") print id }
    ' | sort -u
}

# run_group <label> <cargo nextest args…>
# One nextest invocation for a whole package group. `ran` counts `--test`
# flags, so the summary line stays in the same unit (test binaries) the
# per-file loop reported, and the exit code stays "number of failed test
# binaries".
run_group() {
    local label="$1"
    shift
    local n=0 a
    for a in "$@"; do
        if [ "$a" = "--test" ]; then n=$((n + 1)); fi
    done
    [ "$n" -gt 0 ] || return 0
    ran=$((ran + n))

    local log rc
    log="$(mktemp)"
    # `--success-output final` because `skip()` reports via eprintln and the
    # harness swallows a passing test's output; without it a SKIP is invisible.
    # Failures keep nextest's default immediate output, which already prints the
    # panic and the captured stdout/stderr under the failing binary's name.
    #
    # Piped rather than captured so the run streams to the CI log as it happens
    # (a stuck binary is then visible while it is stuck, not only at the job
    # timeout); `tee` keeps the full text on disk for the counts below. awk
    # prints the run verbatim and then, from the trailing success block, only
    # the tests that skipped.
    # `--color never` is LOAD-BEARING, not cosmetic. ci.yml sets
    # `CARGO_TERM_COLOR: always` workflow-wide, so without this every status
    # line arrives ANSI-wrapped and each parser below anchors at `^ *(FAIL|PASS|
    # Summary)` against an ESC byte instead. Failures then attribute to nothing,
    # `skips` is permanently 0, and the vacuous-green trap this script exists to
    # spring reports `failed=0 skipped=0` on a leg where every tripwire SKIPped.
    cargo nextest run --color never --no-fail-fast --test-threads=1 --success-output final "$@" 2>&1 \
        | tee "$log" \
        | awk '
            !tail { print; fflush(); if ($0 ~ /^ *Summary \[/) tail = 1; next }
            /^ *(PASS|SLOW) \[/ { hdr = $0; shown = 0; next }
            /SKIP:/ { if (!shown++) print hdr; if (shown <= 3) print }
        '
    # nextest's status, not awk's, decides whether this group failed.
    rc="${PIPESTATUS[0]}"

    local group_skips group_fails failed
    group_skips="$(skipped_binaries < "$log" | grep -c . || true)"
    failed="$(failed_binaries < "$log")"
    group_fails="$(printf '%s\n' "$failed" | grep -c . || true)"
    # The offending test binary names on stderr, so CI surfaces them directly
    # instead of only inside nextest's stdout log.
    if [ "$group_fails" -gt 0 ]; then
        printf 'FAIL %s\n' $failed >&2
        # The panic lines on their own, too. A failing TUI test dumps a full
        # 40-row tmux pane into its assertion message, which buries the actual
        # `panicked at ...` line inside nextest's captured output, so CI logs
        # showed failures and no reasons.
        grep -A2 'panicked at' "$log" >&2 || true
    fi
    # A non-zero exit with no attributed failure is a build/harness error, not a
    # green run: count it so `set -o pipefail`'s guarantee is not undone here.
    if [ "$rc" -ne 0 ] && [ "$group_fails" -eq 0 ]; then
        printf 'FAIL %s: nextest exited %s with no per-test failure (build error?)\n' \
            "$label" "$rc" >&2
        group_fails=1
    fi
    rm -f "$log"
    skips=$((skips + group_skips))
    failures=$((failures + group_fails))
}

# ── hangar crates: every non-tripwire, non-helper integration test target.
#
# `ainb-plugin-hangar` is in this list because NOTHING else in CI compiled its
# integration targets: the `Test` job runs nextest with no `-p`/`--workspace`, so
# `default-members` (ainb-core + ainb-hangar-daemon) is all it builds, and
# `run_all_tripwires.sh` only ever builds `tripwire_*`. Its ~35 non-tripwire
# targets were therefore ungated and silently rotted (a golden pinned the
# workspace version and stayed 1.15.0 into 1.16.1). They gate here now.
for pkg in ainb-hangar-store ainb-hangar-proto ainb-hangar-sandbox ainb-hangar-daemon \
           ainb-plugin-hangar; do
    sel=()
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
        sel+=(--test "$name")
    done
    if [ "${#sel[@]}" -gt 0 ]; then
        run_group "$pkg" -p "$pkg" "${sel[@]}"
    fi
done

# ── ainb crate: every Hangar acceptance target. Globbed by the `hangar_*` /
#    `tripwire_hangar_*` prefix (so new ones — daemon-lifecycle, webhook CLI, … —
#    gate automatically) but NOT a blanket `--tests`: the ainb crate's other
#    integration targets (tests/ui_tests.rs, tests/behavioral/, …) carry
#    pre-existing NewSessionState drift that fails to compile and is unrelated to
#    Hangar; naming the Hangar prefix avoids building them.
sel=()
for f in crates/ainb-core/tests/hangar_*.rs crates/ainb-core/tests/tripwire_hangar_*.rs; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    sel+=(--test "$name")
done
if [ "${#sel[@]}" -gt 0 ]; then
    run_group ainb -p ainb "${sel[@]}"
fi

echo "─────────────────────────────────────────"
echo "hangar acceptance: ran=$ran failed=$failures skipped=$skips"
exit "$failures"
