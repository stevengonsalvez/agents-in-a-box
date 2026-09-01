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
#   ainb-tui/crates/ainb-plugin-hangar/tests/tripwire_*.rs          P3.8/P4.10 plugin↔daemon roundtrip
#
# Conventions (deliberately mirroring the repo, NOT the stale P9 plan):
#
#   * `set -o pipefail` so a failing test run is never masked by a
#     downstream pipe (`reference_tail_masks_pipe_exit`).
#   * ONE `cargo nextest run` per package, not one `cargo test --test X` per
#     file. The per-file loop spawned one cargo process per target (68 today),
#     each re-resolving the dependency graph, re-checking every fingerprint, and
#     relinking before running a single binary; that overhead, not the tests,
#     was most of this script's CI wall-clock. nextest takes the target list as
#     repeated `--test` flags, so the globs below still decide exactly what
#     runs.
#   * `--test-threads=1` for the whole suite: several tripwires mutate the
#     process env (`ENV_LOCK`-guarded) and bind per-test sockets; serialising
#     keeps cross-process/within-binary env races deterministic
#     (`reference_env_lock_for_parallel_tests`).
#   * The OTLP export tripwire is `#![cfg(feature = "otlp")]` — without the
#     feature its file compiles to nothing — so it runs in a SEPARATE
#     `--features otlp` invocation.
#   * Exit code = number of failed tripwire binaries. On failure the offending
#     tripwire name is printed to stderr so CI surfaces it directly.
#     CAVEAT of the one-invocation-per-package shape: a COMPILE error is a
#     property of the group, not of a target. nextest builds the whole group
#     before running anything, so one target that fails to compile costs its
#     package one counted failure and its siblings never run at all. The
#     per-file loop would have run the other 67. A compile break is loud and
#     is caught earlier by `fmt`/`test` anyway; a mid-suite runtime failure,
#     the case this gate actually exists for, still attributes per binary.
#
# PREREQUISITES (the caller stages these; CI does it as explicit steps):
#   * `cargo-nextest` on PATH. CI installs it in the `hangar-e2e` job; locally
#     `cargo install cargo-nextest`. Without it every group reports
#     `nextest exited 101 with no per-test failure (build error?)`.
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
skips=0

# Optional SMOKE subset (the CI macOS leg). The hosted GitHub macOS runner is
# too small to finish the full serial suite (68 tripwires today) within budget:
# ~4 random heavy per-screen TUI tripwires time out at the scaled ceiling, a
# runner-capacity artifact (every tripwire passes deterministically on the Linux
# leg). The tmux tripwires exercise OS-agnostic render/protocol/daemon logic, so
# the Linux leg is the authoritative full matrix; the macOS leg only needs to
# prove the macOS-staged binaries actually LAUNCH + the basic stack works
# (daemon boots, an issue-list TUI renders, the plugin connects, the
# macOS-specific crash/reconnect path). With HANGAR_TRIPWIRE_SMOKE=1 the heavy
# per-screen daemon TUI tripwires are pruned to that subset; the fast
# store-migration and plugin-roundtrip tripwires always run on both legs.
SMOKE="${HANGAR_TRIPWIRE_SMOKE:-}"
smoke_keeps_daemon_tripwire() {
    # The smoke set is the reliable launch-proof subset only: the daemon binary
    # boots + binds its socket, the full stack reaches a happy-path e2e, and the
    # `ainb tui` + plugin subprocess launch and render a real screen (the macOS
    # AMFI/codesign "do the staged binaries actually run" signal). Deliberately
    # NOT included: heavy per-screen render + lifecycle tripwires (autopilots,
    # create-flow, agent-picker, cross-screen, plugin-crash/reconnect, the
    # mouse-drag board tripwire) — they are load-sensitive on the small macOS
    # runner and flake here; the Linux leg runs them all deterministically, and
    # `plugin_crash_reconnect`'s parent-death path is being hardened separately
    # before it can rejoin a gating leg. The mouse-drag tripwire
    # (`tripwire_mouse_drag_moves_card`) drives a real SGR mouse drag against the
    # live TUI — OS-agnostic render/protocol logic the Linux full leg authorises.
    case "$1" in
        tripwire_daemon_boots | tripwire_full_e2e | tripwire_p4_issue_list_renders)
            return 0 ;;
        *) return 1 ;;
    esac
}

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
# per-file loop reported, and the exit code stays "number of failed tripwire
# binaries".
run_group() {
    local label="$1"
    shift
    local n=0 a
    for a in "$@"; do
        if [ "$a" = "--test" ]; then n=$((n + 1)); fi
    done
    [ "$n" -gt 0 ] || return 0

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
    # The offending tripwire names on stderr, so CI surfaces them directly
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
        # `ran` deliberately gets NOTHING here. nextest builds the whole group
        # before running any of it, so a compile error in ONE target means ZERO
        # of this group's targets executed. Counting `n` up front made that case
        # print `ran=68 failed=1 skipped=0`, i.e. 67 phantom green tripwires, in
        # the summary line of the script whose entire job is catching vacuous
        # green. A counter that lies in exactly the scenario it exists to detect
        # is worse than no counter, because people trust it.
    else
        ran=$((ran + n))
    fi
    rm -f "$log"
    skips=$((skips + group_skips))
    failures=$((failures + group_fails))
}

# ── ainb-hangar-daemon: every tripwire_* except the otlp one (run below with
#    --features otlp) and the *_common helper (no #[test], not a binary).
sel=()
for f in crates/ainb-hangar-daemon/tests/tripwire_*.rs; do
    name="$(basename "$f" .rs)"
    case "$name" in
        *_common) continue ;;
        tripwire_otel_export_when_endpoint_set) continue ;;
    esac
    if [ -n "$SMOKE" ] && ! smoke_keeps_daemon_tripwire "$name"; then
        continue
    fi
    sel+=(--test "$name")
done
if [ "${#sel[@]}" -gt 0 ]; then
    run_group ainb-hangar-daemon -p ainb-hangar-daemon "${sel[@]}"
fi

# ── OTLP export tripwire — feature-gated, separate invocation. (Full leg only;
#    the smoke leg proves binary launch, not every exporter path.)
if [ -z "$SMOKE" ]; then
    run_group "ainb-hangar-daemon (otlp)" -p ainb-hangar-daemon --features otlp \
        --test tripwire_otel_export_when_endpoint_set
fi

# ── ainb-hangar-store: sqlx migration determinism.
sel=()
for f in crates/ainb-hangar-store/tests/tripwire_*.rs; do
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    sel+=(--test "$name")
done
if [ "${#sel[@]}" -gt 0 ]; then
    run_group ainb-hangar-store -p ainb-hangar-store "${sel[@]}"
fi

# ── ainb-plugin-hangar: plugin↔daemon roundtrip (needs staged plugin + daemon).
sel=()
for f in "$REPO_ROOT"/ainb-tui/crates/ainb-plugin-hangar/tests/tripwire_*.rs; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    sel+=(--test "$name")
done
if [ "${#sel[@]}" -gt 0 ]; then
    run_group ainb-plugin-hangar -p ainb-plugin-hangar "${sel[@]}"
fi

echo "─────────────────────────────────────────"
echo "hangar tripwires: ran=$ran failed=$failures skipped=$skips"
exit "$failures"
