#!/usr/bin/env bash
# Check a `core-tripwires` nextest log against what the job expects to see.
#
# Fails when no test passed, or when the set of tests that printed `SKIP:`
# differs from tests/tripwire_ci_skips.txt in either direction: a new SKIP
# passes a test without exercising it, and a listed test that no longer skips
# means the list is stale. Tests that passed on a retry are named as warnings.
#
# The log must come from `--success-output final` (or `immediate`), so each
# test's stderr follows its PASS line.
#
#   scripts/tripwire-ci-guard.sh core-tripwires.log crates/ainb-core/tests/tripwire_ci_skips.txt
set -euo pipefail

log=$1
expected=$2

plain=$(sed 's/\x1b\[[0-9;]*m//g' "$log")

passed=$(grep -oE '[0-9]+ tests run: [0-9]+ passed' <<<"$plain" | tail -1 | grep -oE '[0-9]+ passed' | cut -d' ' -f1 || true)
if [ -z "$passed" ] || [ "$passed" -eq 0 ]; then
  echo "::error::no ainb-core tripwire passed, so this job exercised nothing"
  exit 1
fi

grep -E '^\s*FLAKY ' <<<"$plain" | sed 's/^\s*/::warning::passed on retry: /' || true

# Attribute each SKIP line to the status line before it: `PASS [ 0.1s] (1/9) ainb::<binary> <test>`.
actual=$(awk '
  match($0, /\) ainb::[^ ]+ [^ ]+/) {
    split(substr($0, RSTART + 8, RLENGTH - 8), name, " ")
    current = name[1] "::" name[2]
  }
  /^[[:space:]]*SKIP:/ && current != "" { print current }
' <<<"$plain" | sort -u)
want=$(grep -vE '^\s*(#|$)' "$expected" | awk '{ print $1 }' | sort -u)

echo "passed=$passed skipped_tests=$(grep -c . <<<"$actual" || true)"
unexpected=$(comm -23 <(printf '%s\n' "$actual") <(printf '%s\n' "$want") | grep . || true)
stale=$(comm -13 <(printf '%s\n' "$actual") <(printf '%s\n' "$want") | grep . || true)
status=0
for test in $unexpected; do
  echo "::error::$test printed SKIP but is not in $expected, so it passed without running"
  status=1
done
for test in $stale; do
  echo "::error::$test is in $expected but did not SKIP; remove its line"
  status=1
done
exit $status
