#!/usr/bin/env bash
# Proof harness: run every delivered programme node against the built binary.
#
#   scripts/proof/run.sh [--build] [--only <node>]... [--out <dir>]
#
#   --build        rebuild ainb, ainb-hangar-daemon and the plugins first
#   --only <node>  run one scenario (repeatable); default is every scenario
#   --out <dir>    where results go; default ainb-tui/proof-out
#
# Each scenario under scenarios/<node>.sh sets EXPECT (one line) and defines
# `scenario`, which drives the real TUI and CLI through tmux in a private
# world (see lib.sh) and records what it observed. The harness writes
#
#   proof-out/<node>/result.json    node, expected, observed, pass, issue,
#                                   capture, binary, started_at
#   proof-out/<node>/*.txt *.ans    plain and ANSI pane captures, CLI output
#   proof-out/summary.json          every result, in run order
#   proof-out/summary.md            one table row per node
#
# Exit status is 0 only when every node passed. A node whose failure is a
# filed product defect still fails; its row names the issue.

# shellcheck source-path=SCRIPTDIR
set -uo pipefail

PROOF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AINB_TUI_DIR="$(cd "$PROOF_DIR/../.." && pwd)"

BUILD=0
ONLY=()
OUT=""
while (($#)); do
  case "$1" in
    --build) BUILD=1 ;;
    --only) ONLY+=("${2:?--only needs a node name}"); shift ;;
    --out) OUT="${2:?--out needs a directory}"; shift ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

export PROOF_OUT="${OUT:-$AINB_TUI_DIR/proof-out}"
# The PATH a world starts from: system tools only, so nothing from the
# operator's own PATH (a real `claude`, a real `headroom`) leaks in.
export PROOF_BASE_PATH="/usr/local/bin:/usr/bin:/bin"

# Checked on the PATH every world actually runs with, not the operator's.
for tool in tmux jq git curl python3 ss ps; do
  PATH="$PROOF_BASE_PATH" command -v "$tool" >/dev/null 2>&1 \
    || { echo "missing required tool on $PROOF_BASE_PATH: $tool" >&2; exit 2; }
done

if ((BUILD)); then
  echo "building ainb, ainb-hangar-daemon and plugins" >&2
  (cd "$AINB_TUI_DIR" \
    && CARGO_INCREMENTAL=0 cargo build -j 4 -p ainb -p ainb-hangar-daemon \
    && bash scripts/build-plugins.sh) || { echo "build failed" >&2; exit 2; }
  # The desktop shell (d1-shell) is its own cargo workspace and needs the
  # platform webview. Built only where webkit2gtk-4.1 is present; elsewhere
  # d1-shell records a skip that says why. Its frontend is built first because
  # `bundled` serves ui/dist from inside the binary. CARGO_TARGET_DIR is dropped
  # so the binary lands where d1-shell looks for it by default.
  if pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
    echo "building the desktop shell (ui/dist, then --features bundled)" >&2
    (cd "$AINB_TUI_DIR/crates/ainb-desktop" \
      && npm --prefix ui ci \
      && npm --prefix ui run build \
      && env -u CARGO_TARGET_DIR CARGO_INCREMENTAL=0 cargo build -j 4 --features bundled) \
      || { echo "desktop shell build failed" >&2; exit 2; }
  else
    echo "webkit2gtk-4.1 not found; not building the desktop shell (d1-shell will skip)" >&2
  fi
fi

# A shared cargo target (CARGO_TARGET_DIR) is where the build put the binary;
# the plugins are always staged under this checkout's dist/.
AINB_BIN="${CARGO_TARGET_DIR:-$AINB_TUI_DIR/target}/debug/ainb"
[[ -x "$AINB_BIN" ]] || { echo "no binary at $AINB_BIN (run with --build)" >&2; exit 2; }
[[ -x "$AINB_TUI_DIR/dist/plugins/hangar-tui/hangar-tui" ]] \
  || { echo "plugins are not staged (run with --build)" >&2; exit 2; }
BINARY_LINE="$("$AINB_BIN" --version 2>&1 | head -1)"
export AINB_BIN BINARY_LINE

# Run order follows the programme: slice 1, slice 2, then the status lane.
ALL_NODES=(
  phase1-keymap s-a-config-and-headroom s-b-connections s-c-answered
  phase2-sections phase3-uistate s-d-surfaces
  p1-app-extraction p2-effects p3-hangar-host p4-review-screens
  w0-wire t0-daemon issue-963-presence issue-962-fleet-panel
  t0-section issue-983-redaction issue-1094-own-session
  d1-shell d2-board
)
if ((${#ONLY[@]})); then NODES=("${ONLY[@]}"); else NODES=("${ALL_NODES[@]}"); fi
for node in "${NODES[@]}"; do
  [[ -f "$PROOF_DIR/scenarios/$node.sh" ]] || { echo "no scenario: $node" >&2; exit 2; }
done

mkdir -p "$PROOF_OUT"
if ((${#ONLY[@]} == 0)); then
  # A full run owns the whole directory: no result from an earlier run survives.
  # It only ever clears a directory that is empty or was written by this
  # harness, so `--out ~` or `--out .` cannot delete someone's files.
  if [[ -n "$(find "$PROOF_OUT" -mindepth 1 -maxdepth 1 -print -quit)" ]] \
    && [[ ! -f "$PROOF_OUT/order.txt" && ! -f "$PROOF_OUT/summary.json" ]]; then
    echo "refusing to clear $PROOF_OUT: not empty and not a previous proof-out (no order.txt or summary.json)" >&2
    exit 2
  fi
  find "$PROOF_OUT" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
fi

if ((${#ONLY[@]} == 0)); then printf '%s\n' "${NODES[@]}" >"$PROOF_OUT/order.txt"; fi

# Every world of this run lives under one temp root of its own.
PROOF_TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ainb-proof-run.XXXXXX")" || exit 2
export PROOF_TMP_ROOT

echo "proof: $BINARY_LINE" >&2
echo "proof: ${#NODES[@]} node(s), results in $PROOF_OUT" >&2

for node in "${NODES[@]}"; do
  echo "== $node" >&2
  # A subshell per node: a scenario's exports, traps and globals die with it.
  (
    # Scenarios poll with `cmd | grep -q`. Under pipefail a grep that matches
    # and exits early gets its producer a SIGPIPE, and the match reads as a
    # failure, so a node runs without it.
    set +o pipefail
    # shellcheck source=lib.sh
    source "$PROOF_DIR/lib.sh"
    # shellcheck source=/dev/null
    source "$PROOF_DIR/scenarios/$node.sh"
    world_up "$node"
    trap 'world_down' EXIT
    scenario
    write_result
  )
  # Teardown adds surface and daemon logs after the result is written; list
  # every file that is actually beside the result.
  if [[ -f "$PROOF_OUT/$node/result.json" ]]; then
    files="$(find "$PROOF_OUT/$node" -maxdepth 1 -type f ! -name result.json -printf '%f\n' | sort | jq -R . | jq -sc .)"
    jq --argjson files "$files" '.capture = $files' "$PROOF_OUT/$node/result.json" >"$PROOF_OUT/$node/result.json.tmp" \
      && mv "$PROOF_OUT/$node/result.json.tmp" "$PROOF_OUT/$node/result.json"
  fi
  echo "   $(jq -r 'if .skipped then "SKIP (\(.skipped))" elif .pass then "PASS" else "FAIL" + (if .issue then " (#\(.issue))" else "" end) end' \
    "$PROOF_OUT/$node/result.json" 2>/dev/null || echo "NO RESULT")" >&2
done

# Summaries cover every result directory present, so an --only rerun of one
# node refreshes its row without dropping the others.
# The checkout the scenarios came from; `+dirty` when it has local changes.
SOURCE_LINE="$(git -C "$PROOF_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
if [[ -n "$(git -C "$PROOF_DIR" status --porcelain --untracked-files=no 2>/dev/null)" ]]; then
  SOURCE_LINE+="+dirty"
fi
python3 "$PROOF_DIR/summarize.py" "$PROOF_OUT" "$BINARY_LINE" "$SOURCE_LINE"
status=$?

# Only this run's worlds: a process belongs to it when its environment names a
# path under this run's own temp root, so a concurrent run is never counted.
leftover=""
for pid in $(pgrep -u "$(id -u)"); do
  [[ "$pid" == "$$" ]] && continue
  if { tr '\0' '\n' <"/proc/$pid/environ"; } 2>/dev/null | grep -F "=$PROOF_TMP_ROOT/" >/dev/null; then
    leftover+="$pid "
  fi
done
if [[ -n "$leftover" ]]; then
  echo "proof: processes from this run's worlds are still running: $leftover" >&2
  status=1
else
  rm -rf "$PROOF_TMP_ROOT"
fi
exit "$status"
