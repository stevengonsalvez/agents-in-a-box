#!/usr/bin/env bash
# disk-space-cleaner: reclaim disk by deleting regenerable build and dependency
# directories, biggest first, skipping anything a live process is using.
#
# Deletes NOTHING by default (dry-run). Pass --apply to actually remove.
#
# Two rules this script exists to enforce:
#   1. Only ever delete a build/dependency directory. Never the directory that
#      contains it. A worktree is never removed, only its target/ or
#      node_modules/ is.
#   2. Decide by LIVENESS, not by age. A target rebuilt an hour ago by a session
#      that has since finished is safe to clear; one untouched for a month can
#      belong to a session that is running right now. mtime cannot tell those
#      apart, so it is not used as a safety signal.
set -uo pipefail

# ---- defaults ----
ROOTS=()
NAMES=(target node_modules .next dist build .gradle .turbo .venv)
MIN_SIZE_MB=500        # ignore anything smaller; the long tail is not worth the risk
MAXDEPTH=8
APPLY=0
IGNORE_LIVE=0          # escape hatch, documented as dangerous
TOP=0                  # 0 = no limit

usage() {
  cat <<'EOF'
Usage: clean.sh [ROOT ...] [options]

Finds regenerable build and dependency directories, sorts them biggest first,
and skips any whose owning directory shows signs of active use. Prints what it
would delete (dry-run). Add --apply to delete.

Positional:
  ROOT ...            Directories to scan (default: current dir).

Options:
  --min-size MB       Ignore directories smaller than this (default: 500).
  --names a,b,c       Directory names to target
                      (default: target,node_modules,.next,dist,build,.gradle,.turbo,.venv).
  --max-depth N       How deep to search (default: 8).
  --top N             Only act on the N largest candidates.
  --apply             Actually delete. Without this, only prints.
  --ignore-live       Delete even where a live process was detected. Dangerous:
                      this is the check that stops a running build losing its
                      artifacts mid-compile. Do not use casually.
  -h, --help          Show this help.

A directory is treated as IN USE, and skipped, if any of these hold:
  - a `cargo` or `rustc` process has its path in the command line
  - a tmux session name contains the owning directory's name
  - any process has its cwd inside the owning directory

Only the named build directories are ever removed. The directory containing
them is never touched, so a git worktree survives having its target/ cleared.
EOF
}

# ---- parse args ----
while [ $# -gt 0 ]; do
  case "$1" in
    --min-size) MIN_SIZE_MB="$2"; shift 2 ;;
    --names) IFS=',' read -r -a NAMES <<< "$2"; shift 2 ;;
    --max-depth) MAXDEPTH="$2"; shift 2 ;;
    --top) TOP="$2"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --ignore-live) IGNORE_LIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage; exit 2 ;;
    *) ROOTS+=("$1"); shift ;;
  esac
done
[ ${#ROOTS[@]} -eq 0 ] && ROOTS=(".")

# ---- liveness signals ----
# Collected once up front: walking every candidate would re-run ps and lsof per
# directory, and lsof in particular is slow on a near-full disk.

# Exclude grep/ripgrep lines and this script's own process. `ps` shows the
# command line of whatever is doing the checking, and a probe that mentions both
# "cargo" and a worktree path will match itself, reporting a live build that does
# not exist. That false positive protects directories forever.
PS_SNAPSHOT="$(ps -Awwo pid,command 2>/dev/null \
  | grep -v -E '(^| )[0-9]+ +(grep|rg|egrep|fgrep) ' \
  | grep -v -E "^ *$$ " \
  | grep -vF "clean.sh" || true)"
TMUX_SNAPSHOT="$(tmux ls 2>/dev/null || true)"
# `lsof -d cwd` is the expensive one. Skipped entirely when nothing will be
# deleted, since a dry-run does not need it to be authoritative.
if [ "$APPLY" -eq 1 ] && [ "$IGNORE_LIVE" -eq 0 ]; then
  LSOF_SNAPSHOT="$(lsof -d cwd 2>/dev/null || true)"
else
  LSOF_SNAPSHOT=""
fi

# Why is this directory in use? Echoes a reason, or nothing if it looks idle.
# $1 = the directory that OWNS the build dir (the worktree, not the target).
live_reason() {
  local owner="$1"

  # A rust build names paths under the project in nearly every rustc argv, so a
  # fixed-string match of the owning directory anywhere in a cargo/rustc command
  # line is the reliable signal. Matching the path, not a basename: basenames
  # like "ainb-tui" repeat across every worktree and would be useless here.
  # NOTE: `grep -q` must not be used in these pipelines. It exits on the first
  # match, the upstream grep takes SIGPIPE, and with `pipefail` set the whole
  # pipeline then reports failure, so the check silently never fires. Counting
  # greps read all input and cannot lose that way.
  if [ "$(printf '%s\n' "$PS_SNAPSHOT" | grep -E 'cargo |rustc ' | grep -cF -- "$owner")" -gt 0 ]; then
    echo "live cargo/rustc"; return
  fi

  # tmux names sessions after the worktree, not after the crate subdirectory, so
  # test path components rather than just the immediate parent. Only components
  # BELOW the scan root are considered: everything at or above it (a username, a
  # "worktrees" directory, the repo name shared by every worktree) appears in
  # unrelated session names and would mark every candidate as in use.
  if [ -n "$TMUX_SNAPSHOT" ]; then
    local rel part
    rel="${owner#"$2"}"
    while IFS= read -r part; do
      [ ${#part} -lt 8 ] && continue
      if [ "$(printf '%s\n' "$TMUX_SNAPSHOT" | grep -cF -- "$part")" -gt 0 ]; then
        echo "live tmux session"; return
      fi
    done < <(printf '%s\n' "$rel" | tr '/' '\n')
  fi

  if [ -n "$LSOF_SNAPSHOT" ] && [ "$(printf '%s\n' "$LSOF_SNAPSHOT" | grep -cF -- "$owner")" -gt 0 ]; then
    echo "process cwd inside"; return
  fi
  echo ""
}

# ---- find candidates ----
NAME_ARGS=()
for i in "${!NAMES[@]}"; do
  [ "$i" -gt 0 ] && NAME_ARGS+=(-o)
  NAME_ARGS+=(-name "${NAMES[$i]}")
done

CANDIDATES="$(mktemp)"
trap 'rm -f "$CANDIDATES"' EXIT

for r in "${ROOTS[@]}"; do
  while IFS= read -r -d '' d; do
    kb=$(du -sk "$d" 2>/dev/null | cut -f1)
    [ -z "$kb" ] && continue
    [ "$kb" -lt $((MIN_SIZE_MB * 1024)) ] && continue
    printf '%s\t%s\t%s\n' "$kb" "$d" "$r" >> "$CANDIDATES"
  done < <(find "$r" -maxdepth "$MAXDEPTH" \( -type d \( "${NAME_ARGS[@]}" \) \) -prune -print0 2>/dev/null)
done

human_gb() { awk -v k="$1" 'BEGIN{printf "%.1fG", k/1024/1024}'; }

echo "== disk-space-cleaner =="
echo "roots:   ${ROOTS[*]}"
echo "targets: ${NAMES[*]}"
echo "min-size: ${MIN_SIZE_MB}MB | apply: $([ "$APPLY" -eq 1 ] && echo yes || echo 'NO (dry-run)')"
[ "$IGNORE_LIVE" -eq 1 ] && echo "WARNING: --ignore-live set, liveness checks disabled"
echo

TOTAL_KB=0; COUNT=0; SKIPPED=0; N=0
while IFS=$'\t' read -r kb dir SCAN_ROOT; do
  [ -z "$dir" ] && continue
  N=$((N + 1))
  [ "$TOP" -gt 0 ] && [ "$N" -gt "$TOP" ] && break

  owner="$(dirname "$dir")"
  reason=""
  [ "$IGNORE_LIVE" -eq 0 ] && reason="$(live_reason "$owner" "$SCAN_ROOT")"

  if [ -n "$reason" ]; then
    printf 'skip  %8s  %s\n         (%s)\n' "$(human_gb "$kb")" "$dir" "$reason"
    SKIPPED=$((SKIPPED + 1))
    continue
  fi

  if [ "$APPLY" -eq 1 ]; then
    if rm -rf "$dir"; then
      printf 'rm    %8s  %s\n' "$(human_gb "$kb")" "$dir"
      # Rule 1, verified rather than assumed: the owning directory must survive.
      [ -d "$owner" ] || echo "         ERROR: owning directory disappeared: $owner" >&2
      TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
    else
      echo "         FAILED to remove $dir" >&2
    fi
  else
    printf 'would rm %6s  %s\n' "$(human_gb "$kb")" "$dir"
    TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
  fi
done < <(sort -rn "$CANDIDATES")

echo
printf 'summary: %d dirs %s %s, %d skipped as in use\n' \
  "$COUNT" "$([ "$APPLY" -eq 1 ] && echo removed || echo 'would free')" \
  "$(human_gb "$TOTAL_KB")" "$SKIPPED"
[ "$APPLY" -eq 0 ] && echo "(dry-run, re-run with --apply to delete)"
exit 0
