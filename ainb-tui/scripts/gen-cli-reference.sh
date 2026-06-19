#!/usr/bin/env bash
# Generate the full multi-hierarchy CLI reference (docs/tui/cli.md) from the
# real `ainb` binary's recursive `--help` output. The binary is the single
# source of truth — every command's about, flags, and EXAMPLES come straight
# from `--help`, so this page can never drift from the actual CLI.
#
# Driving the real binary (rather than the clap tree directly) means the
# skill-manager commands intercepted before clap (`skill`/`source`/`search`/
# `migrate`) are documented too — same faithfulness as tests/cli_help_audit.rs.
#
# Usage:
#   scripts/gen-cli-reference.sh            # builds ainb (release), regenerates
#   AINB_BIN=target/debug/ainb scripts/gen-cli-reference.sh   # reuse a built bin
#
# CI freshness gate (see .github/workflows/ci.yml):
#   AINB_BIN=target/debug/ainb scripts/gen-cli-reference.sh
#   git diff --exit-code -- docs/tui/cli.md
#
# Output is deterministic (registry order, no timestamps/versions) so the gate
# only fires on real CLI-surface changes. DO NOT edit docs/tui/cli.md by hand.
#
# NB: pipefail is intentionally OFF — the parse helpers below early-exit their
# `awk`, which SIGPIPEs the producing `ainb --help`; with pipefail+`set -e` that
# non-zero pipe status would abort generation mid-tree. cargo build is run
# unpiped, so we don't lose its failure signal.
set -eu

# Repo paths — script lives in ainb-tui/scripts/, docs live at repo-root docs/.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AINB_TUI_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$AINB_TUI_DIR/.." && pwd)"
OUT="$REPO_ROOT/docs/tui/cli.md"

# Resolve the binary: explicit AINB_BIN, else build release.
if [[ -n "${AINB_BIN:-}" ]]; then
  BIN="$AINB_BIN"
  [[ "$BIN" = /* ]] || BIN="$AINB_TUI_DIR/$BIN"
else
  echo "[gen-cli-reference] building ainb (release)…" >&2
  ( cd "$AINB_TUI_DIR" && cargo build --release -p ainb >&2 )
  BIN="$AINB_TUI_DIR/target/release/ainb"
fi
[[ -x "$BIN" ]] || { echo "[gen-cli-reference] binary not found: $BIN" >&2; exit 1; }

# Child subcommand names from a node's `--help` Commands: block (skip `help`).
children() {
  "$BIN" "$@" --help 2>&1 | awk '
    /^Commands:/{f=1; next}
    /^Options:/{f=0}
    f && /^  [a-z]/ {print $1}' | grep -vxE 'help' || true
}

# First non-empty line before "Usage:" — the command "about".
about_of() {
  "$BIN" "$@" --help 2>&1 | awk 'NR>0 && !/^$/ && !/^Usage:/{print; exit} /^Usage:/{exit}'
}

# Emit one command section: heading at `depth` (1=##, 2=###, 3=####) + a fenced
# console block of its full --help.
emit_node() {
  local depth="$1"; shift
  local path="$*"
  local hashes; hashes=$(printf '#%.0s' $(seq 1 $((depth + 1))))
  printf '%s `ainb %s`\n\n' "$hashes" "$path"
  local about; about=$(about_of "$@")
  [[ -n "$about" ]] && printf '%s\n\n' "$about"
  printf '```console\n$ ainb %s --help\n' "$path"
  "$BIN" "$@" --help 2>&1
  printf '```\n\n'
}

{
  # ---- Fixed preamble (authored once; kept stable so the diff gate only
  #      reacts to real CLI changes) ----
  cat <<'PREAMBLE'
---
title: "ainb CLI Reference"
description: "Full multi-hierarchy reference for every ainb subcommand — generated from the binary's --help."
---

`ainb` is both an interactive terminal UI and a scriptable, headless CLI. Every
operation the TUI performs is also exposed as a subcommand with `--format json`
output, so humans drive it from the dashboard and agents drive it from shell
scripts. Run `ainb` with no arguments to launch the TUI; use any subcommand
below for non-interactive work.

> **Generated — do not edit by hand.** This page is produced from the live
> binary by [`ainb-tui/scripts/gen-cli-reference.sh`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/ainb-tui/scripts/gen-cli-reference.sh),
> which walks `ainb <cmd> --help` for every command. CI fails if it drifts, so
> the output of `ainb --help` stays the source of truth. To update: run the
> script and commit the result.

## Global flags

| Flag | Description |
|------|-------------|
| `--format <text\|json\|csv\|markdown>` | Output format for any command (default `text`). `json` is the machine-readable form for scripting/agents. |
| `-h, --help` | Print help for the command (recursive — works at every level). |
| `-V, --version` | Print the build identity (commit + date), or `-V` for the bare semver. |

## Reading this reference

Each command shows its description followed by the verbatim `ainb <cmd> --help`
output — including its arguments, flags, and an `EXAMPLES:` block. Groups
(`config`, `git`, `usage`, `fleet`, `hangar`, …) nest their subcommands as
sub-sections; recursive help (`ainb <group> <sub> --help`) works for every
node. The page's right-hand "On this page" panel is the full command tree.

PREAMBLE

  echo "## Command reference"
  echo

  # ---- Generated command tree (registry order; deterministic) ----
  while IFS= read -r s; do
    [[ -z "$s" ]] && continue
    emit_node 1 "$s"
    while IFS= read -r ss; do
      [[ -z "$ss" ]] && continue
      emit_node 2 "$s" "$ss"
      while IFS= read -r sss; do
        [[ -z "$sss" ]] && continue
        emit_node 3 "$s" "$ss" "$sss"
      done < <(children "$s" "$ss")
    done < <(children "$s")
  done < <(children)

  # ---- Fixed footer ----
  cat <<'FOOTER'
## Skill manager

The skill-manager commands (`skill`, `source`, `search`, `migrate`) are
intercepted before clap and routed to the unit manager; they are documented in
their own section under [Skill manager](../skill-manager/guide) and surfaced in
`ainb --help` under "SKILL MANAGER". Run `ainb skill --help` for the full verb
list.

## Hidden / daemon commands

A few commands are hidden from `ainb --help` because they are internal
daemon/hook entrypoints rather than everyday verbs. Run `<cmd> --help` for each:

- `ainb notifyd <run|stop|install|uninstall|status|list>` — the ainb-hooks
  notification daemon. `ainb notifyd list [--format json]` reads persisted
  notifications headlessly; the TUI Inbox is the interactive view.
- `ainb statusline` — legacy Claude Code statusline alias (prefer
  `ainb claudecode statusline`).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Runtime error (the command ran but failed). |
| `2` | Usage error (bad flags/args), or a required plugin/tool is not installed. |

## Scripting recipes

```bash
# List running sessions as JSON and pull workspace names
ainb list --running --format json | jq -r '.[].workspace'

# Spawn a session in an isolated worktree with an initial prompt
ainb run --repo . --worktree -p "fix the failing tests"

# Machine-readable diff of the working tree (no TUI)
ainb diff-review --format json | jq '.total_insertions, .total_deletions'

# Find orphaned sessions and clean them up
ainb recover list --format json && ainb recover cleanup

# Search the knowledge base headlessly
ainb learnings search "redis connection pooling" --format json
```
FOOTER
} > "$OUT"

echo "[gen-cli-reference] wrote $OUT" >&2
