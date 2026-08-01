#!/usr/bin/env bash
#
# Fail if a temporary debugging probe was left behind in the Rust sources.
#
# Why this exists: during the "(broken) workspace name" fix an agent replaced
# the body of `get_source_repository` with a marked stub
# (`// REVERT-PROBE: pre-fix behaviour (linked worktree only).`) to confirm the
# unit tests actually caught the regression, and the stub was still sitting in
# the working tree minutes later while every doc comment, test, skill and help
# string around it asserted the fixed behaviour. A probe is a legitimate tool;
# committing one is not. This gate makes an abandoned probe loud instead of
# silent.
#
# Convention: mark any deliberately-reverted / scaffolded code with one of the
# markers below. The gate then guarantees it cannot reach a commit.
#
# Usage: bash scripts/check-no-probes.sh   (run from the ainb-tui workspace root)

set -euo pipefail

cd "$(dirname "$0")/.."

MARKERS='REVERT-PROBE|DO-NOT-COMMIT|DO NOT COMMIT'

# `git grep` so we only ever look at working-tree sources and never at target/.
# `--untracked` is load-bearing: a new test file is untracked until it is
# committed, which is exactly when a probe left in it would slip past a gate
# that only scans the index.
hits=$(git grep -nE --untracked "$MARKERS" -- 'crates/**/*.rs' 'xtask/**/*.rs' || true)

if [[ -n "$hits" ]]; then
    echo "error: temporary debugging probe left in the sources." >&2
    echo >&2
    echo "$hits" >&2
    echo >&2
    echo "Remove the probe (restore the real implementation) before committing." >&2
    exit 1
fi

echo "check-no-probes: clean (no REVERT-PROBE / DO-NOT-COMMIT markers in Rust sources)"
