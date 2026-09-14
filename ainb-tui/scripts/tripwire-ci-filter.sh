#!/usr/bin/env bash
# Print the nextest filter for the ainb-core tripwires, built from
# crates/ainb-core/tests/tripwire_ci_exclusions.txt (one `binary::test` per line).
#
#   scripts/tripwire-ci-filter.sh             every tripwire test not excluded
#   scripts/tripwire-ci-filter.sh --excluded  only the excluded tests
set -euo pipefail

cd "$(dirname "$0")/.."

included='binary(/^tripwire_/)'
excluded=''
while read -r name _; do
  case "$name" in '' | '#'*) continue ;; esac
  test_expr="(binary(=${name%%::*}) & test(=${name#*::}))"
  included="$included & not $test_expr"
  excluded="${excluded:+$excluded | }$test_expr"
done < crates/ainb-core/tests/tripwire_ci_exclusions.txt

if [ "${1:-}" = "--excluded" ]; then
  if [ -z "$excluded" ]; then
    echo "no tripwire test is excluded" >&2
    exit 1
  fi
  echo "$excluded"
else
  echo "$included"
fi
