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
skips=0

# Optional SMOKE subset (the CI macOS leg). The hosted GitHub macOS runner is
# too small to finish the full 34-tripwire serial suite within budget — ~4
# random heavy per-screen TUI tripwires time out at the scaled ceiling, a
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

# run_one <package> <test-binary-name> [extra cargo args…]
run_one() {
    local pkg="$1"
    local test_name="$2"
    shift 2
    ran=$((ran + 1))
    local out
    # `--nocapture` because `skip()` reports via eprintln and the libtest
    # harness swallows passing tests' output — without it a SKIP is invisible.
    if out="$(cargo test -p "$pkg" "$@" --test "$test_name" -- --test-threads=1 --nocapture 2>&1)"; then
        # A green binary may still have SKIPped (missing tmux/binaries/plugin).
        # Surface that on success — otherwise an environment where every TUI
        # tripwire skips reads as a genuinely green suite (the macOS-leg
        # vacuous-green trap).
        if printf '%s\n' "$out" | grep -q '^SKIP:'; then
            printf 'SKIP %s::%s\n' "$pkg" "$test_name"
            printf '%s\n' "$out" | grep '^SKIP:' | head -3
            skips=$((skips + 1))
        else
            printf 'ok   %s::%s\n' "$pkg" "$test_name"
        fi
    else
        printf 'FAIL %s::%s\n' "$pkg" "$test_name" >&2
        # The panic line FIRST. A failing TUI tripwire dumps a full 40-row tmux
        # pane into its assertion message, which pushed the actual `panicked
        # at …` line far outside a plain `tail`, so CI logs showed eight
        # failures and zero reasons.
        printf '%s\n' "$out" | grep -A2 'panicked at' >&2
        printf '%s\n' "$out" | tail -40 >&2
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
    if [ -n "$SMOKE" ] && ! smoke_keeps_daemon_tripwire "$name"; then
        continue
    fi
    run_one ainb-hangar-daemon "$name"
done

# ── OTLP export tripwire — feature-gated, separate invocation. (Full leg only;
#    the smoke leg proves binary launch, not every exporter path.)
if [ -z "$SMOKE" ]; then
    run_one ainb-hangar-daemon tripwire_otel_export_when_endpoint_set --features otlp
fi

# ── ainb-hangar-store: sqlx migration determinism.
for f in crates/ainb-hangar-store/tests/tripwire_*.rs; do
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    run_one ainb-hangar-store "$name"
done

# ── ainb-plugin-hangar: plugin↔daemon roundtrip (needs staged plugin + daemon).
for f in "$REPO_ROOT"/ainb-tui/crates/ainb-plugin-hangar/tests/tripwire_*.rs; do
    [ -e "$f" ] || continue
    name="$(basename "$f" .rs)"
    case "$name" in *_common) continue ;; esac
    run_one ainb-plugin-hangar "$name"
done

echo "─────────────────────────────────────────"
echo "hangar tripwires: ran=$ran failed=$failures skipped=$skips"
exit "$failures"
