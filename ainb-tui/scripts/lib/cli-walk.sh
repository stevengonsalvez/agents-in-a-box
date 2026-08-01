# shellcheck shell=bash
# Shared CLI-tree walk helpers for the generated docs. Sourced, never executed,
# so it has no shebang.
#
# Sourced by BOTH scripts/gen-cli-reference.sh (docs/tui/cli.md) and
# scripts/gen-man.sh (docs/man/ainb.1). They live here so the two generators
# can never disagree about what counts as a subcommand or as a command's
# "about" line. A divergence there would show up as a permanent, unfixable diff
# between the two CI freshness gates.
#
# Contract: the caller must have `BIN` set to an executable `ainb`. Both
# helpers drive the REAL binary's `--help`, never a clap tree, because
# `tui`/`diff-review` are attached in main.rs after the reusable builders and
# `skill`/`source`/`search` are intercepted before clap exists at all.
#
# NB: these helpers early-exit their `awk`, which SIGPIPEs the producing
# `ainb --help`. Callers must therefore keep `pipefail` OFF (see the note in
# gen-cli-reference.sh) or generation aborts mid-tree.

# Child subcommand names from a node's `--help` Commands: block (skip `help`).
# Prints nothing (exit 0) for a leaf command.
children() {
  "$BIN" "$@" --help 2>&1 | awk '
    /^Commands:/{f=1; next}
    /^Options:/{f=0}
    f && /^  [a-z]/ {print $1}' | grep -vxE 'help' || true
}

# First non-empty line before "Usage:": the command "about".
about_of() {
  "$BIN" "$@" --help 2>&1 | awk 'NR>0 && !/^$/ && !/^Usage:/{print; exit} /^Usage:/{exit}'
}
