#!/usr/bin/env bash
# Check a `core-tripwires` nextest log against what the job expects to see.
#
# Fails when no test passed, or when a test printed `SKIP:` without a line in
# tests/tripwire_ci_skips.txt: it passed without exercising anything. A listed
# test that no longer skips (a runner that gained the tool) is a warning, so
# the list goes stale visibly without reddening trunk. Tests that passed on a
# retry are named as warnings.
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
skips=$(awk '
  match($0, /\) ainb::[^ ]+ [^ ]+/) {
    split(substr($0, RSTART + 8, RLENGTH - 8), name, " ")
    current = name[1] "::" name[2]
  }
  /^[[:space:]]*SKIP:/ && current != "" { sub(/^[[:space:]]*/, ""); print current "\t" $0 }
' <<<"$plain" | sort -u)
actual=$(cut -f1 <<<"$skips" | grep . | sort -u || true)
want=$(grep -vE '^\s*(#|$)' "$expected" | awk '{ print $1 }' | sort -u)

echo "passed=$passed skipped_tests=$(grep -c . <<<"$actual" || true)"
unexpected=$(comm -23 <(printf '%s\n' "$actual") <(printf '%s\n' "$want") | grep . || true)
stale=$(comm -13 <(printf '%s\n' "$actual") <(printf '%s\n' "$want") | grep . || true)
status=0
for test in $unexpected; do
  reason=$(awk -F'\t' -v t="$test" '$1 == t { print $2; exit }' <<<"$skips")
  echo "::error::$test printed \"$reason\" but is not in $expected, so it passed without running"
  status=1
done
for test in $stale; do
  echo "::warning::$test is in $expected but did not SKIP on this runner; remove its line if that holds"
done
exit $status
