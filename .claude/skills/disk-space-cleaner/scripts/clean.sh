#!/usr/bin/env bash
# disk-space-cleaner: reclaim disk by deleting regenerable build and dependency
# directories, biggest first, skipping anything a live process is using.
#
# Deletes NOTHING by default (dry-run). Pass --apply to actually remove.
#
# Three rules this script exists to enforce:
#   1. Only ever delete a build/dependency directory. Never the directory that
#      contains it. A worktree is never removed, only its target/ or
#      node_modules/ is. Checked BEFORE the rm, not after: a check that runs
#      after the deletion cannot prevent anything, and if the path was already
#      corrupted the check derives from the same corrupted value and passes.
#   2. Decide by LIVENESS, not by age. A target rebuilt an hour ago by a session
#      that has since finished is safe to clear; one untouched for a month can
#      belong to a session that is running right now. mtime cannot tell those
#      apart, so it is not used as a safety signal.
#   3. Fail CLOSED. A liveness signal that could not be collected is not the
#      same as a signal that came back empty. If ps, tmux or lsof is missing or
#      unreadable, deletions are refused rather than performed blind.
set -uo pipefail

# ---- defaults ----
ROOTS=()
# `dist` and `.gradle` are deliberately NOT default targets. `dist` is routinely
# a deployed artifact directory with nothing local able to rebuild it, and
# `~/.gradle` is a tool home holding jdks/, native/ and the daemon registry
# rather than project output. Both remain available through --names.
NAMES=(target node_modules .next build .turbo .venv)
MIN_SIZE_MB=500        # ignore anything smaller; the long tail is not worth the risk
MAXDEPTH=8
APPLY=0
IGNORE_LIVE=0          # escape hatch, documented as dangerous
PROTECT_LOCKED=1       # skip anything under a `locked` git worktree
TOP=0                  # 0 = no limit

# The shortest path component the tmux test will consider. Two characters and
# below match almost any session name by accident. Real worktree names like
# `api`, `web`, `ainb` and `hangar` are protected; an earlier 8-character floor
# silently exempted every one of them.
MIN_TMUX_COMPONENT=3

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
                      (default: target,node_modules,.next,build,.turbo,.venv).
  --max-depth N       How deep to search (default: 8).
  --top N             Only act on the N largest ACTIONABLE candidates. Ones
                      skipped as in use do not consume a slot.
  --apply             Actually delete. Without this, only prints.
  --no-protect-locked Do NOT skip git worktrees marked `locked` (not recommended).
  --ignore-live       Delete even where a live process was detected, and even
                      when a liveness signal could not be collected. Dangerous:
                      these are the checks that stop a running build losing its
                      artifacts mid-compile. Do not use casually.
  -h, --help          Show this help.

A directory is treated as IN USE, and skipped, if any of these hold:
  - it sits under a git worktree marked `locked`
  - a build process (cargo, rustc, node, npm/pnpm/yarn, next, vite, webpack,
    tsc, esbuild, turbo, gradle, java, python/pip/uv, go) has the owning
    directory OR the candidate itself in its command line
  - a tmux session name contains a path component of the owning directory
  - any process has its cwd inside the owning directory

The same checks run in dry-run and under --apply, so the preview is never more
permissive than the action. Under --apply the process and tmux signals are
re-read immediately before each deletion, because the snapshot taken before the
sizing pass is minutes stale by the time the rm happens.

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
    --no-protect-locked) PROTECT_LOCKED=0; shift ;;
    --ignore-live) IGNORE_LIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage; exit 2 ;;
    *) ROOTS+=("$1"); shift ;;
  esac
done
[ ${#ROOTS[@]} -eq 0 ] && ROOTS=(".")

# Canonicalise every root to an absolute physical path.
#
# The default root is `.`, and with a relative root every candidate is `./target`
# and every owner is `.`. A fixed-string search for "." matches essentially every
# ps and lsof line, so the liveness verdict was decided by an unrelated
# substring: `--apply` skipped everything while the dry-run, which did not
# collect lsof, offered to delete the same directories.
abs_dir() { (cd "$1" 2>/dev/null && pwd -P) || return 1; }

ABS_ROOTS=()
for r in "${ROOTS[@]}"; do
  if a="$(abs_dir "$r")"; then
    ABS_ROOTS+=("$a")
  else
    echo "skipping unreadable root: $r" >&2
  fi
done
[ ${#ABS_ROOTS[@]} -eq 0 ] && { echo "no readable roots" >&2; exit 2; }

# ---- liveness signals ----
# Collected once up front for the sizing pass; ps and tmux are re-read per
# deletion (see refresh_volatile_snapshots).

# Commands whose presence in a command line means something is building. The
# previous version matched only `cargo ` and `rustc `, while the default target
# list covered node_modules, .next and .venv, so every non-Rust build was
# invisible to the one signal advertised as primary.
BUILD_PROCESS_RE='cargo |rustc |node |npm |pnpm |yarn |next |vite |webpack |tsc |esbuild |turbo |gradle |java |python|pip |uv |go build|go run'

# Reasons a signal could not be collected. Non-empty means fail closed.
DEGRADED=""

have() { command -v "$1" >/dev/null 2>&1; }

collect_ps() {
  # Exclude grep/ripgrep lines and this script's own process. `ps` shows the
  # command line of whatever is doing the checking, and a probe that mentions
  # both "cargo" and a worktree path will match itself, reporting a live build
  # that does not exist. That false positive protects directories forever.
  ps -Awwo pid,command 2>/dev/null \
    | grep -v -E '(^| )[0-9]+ +(grep|rg|egrep|fgrep) ' \
    | grep -v -E "^ *$$ " \
    | grep -vF "clean.sh"
}

# `tmux ls` exits non-zero when no server is running, which is a legitimate
# "no sessions" and NOT a collection failure. A missing binary is a failure.
collect_tmux() { tmux ls 2>/dev/null || true; }

PS_SNAPSHOT=""
TMUX_SNAPSHOT=""
LSOF_SNAPSHOT=""

refresh_volatile_snapshots() {
  PS_SNAPSHOT="$(collect_ps || true)"
  if [ -z "$PS_SNAPSHOT" ]; then
    # ps always reports at least this script, so empty means it did not run.
    case "$DEGRADED" in *"ps"*) : ;; *) DEGRADED="${DEGRADED}ps unreadable; " ;; esac
  fi
  if have tmux; then
    TMUX_SNAPSHOT="$(collect_tmux)"
  else
    TMUX_SNAPSHOT=""
    case "$DEGRADED" in *"tmux"*) : ;; *) DEGRADED="${DEGRADED}tmux not installed; " ;; esac
  fi
}

if ! have ps; then
  DEGRADED="${DEGRADED}ps not installed; "
fi
refresh_volatile_snapshots

# `lsof -d cwd` is the expensive one, and it used to be collected only under
# --apply. That made the dry-run a strict superset of what --apply would touch,
# so the list the operator approved was produced with one of three guards
# switched off. A preview must be at least as protective as the action it
# previews, so it is now always collected.
if have lsof; then
  LSOF_SNAPSHOT="$(lsof -d cwd 2>/dev/null || true)"
else
  DEGRADED="${DEGRADED}lsof not installed; "
fi

# ---- locked git worktrees ----
# An explicit, deterministic human "do not touch" marker that outlives the
# session that set it. The process and tmux signals only see what is running at
# snapshot time, so a locked worktree that is mid-rebase, paused, or whose
# terminal was closed has no other protection.
LOCKED=()
if [ "$PROTECT_LOCKED" -eq 1 ]; then
  if have git; then
    for r in "${ABS_ROOTS[@]}"; do
      while IFS= read -r wt; do
        [ -n "$wt" ] && LOCKED+=("$wt")
      done < <(git -C "$r" worktree list --porcelain 2>/dev/null \
               | awk '/^worktree /{p=$2} /^locked/{print p}' || true)
    done
  else
    DEGRADED="${DEGRADED}git not installed (locked worktrees unknown); "
  fi
fi

is_locked() {
  local d="$1" l
  for l in "${LOCKED[@]:-}"; do
    [ -z "$l" ] && continue
    case "$d" in "$l"|"$l"/*) return 0 ;; esac
  done
  return 1
}

# Why is this directory in use? Echoes a reason, or nothing if it looks idle.
# $1 = the directory that OWNS the build dir (the worktree, not the target)
# $2 = the absolute scan root this candidate was found under
# $3 = the candidate directory itself
live_reason() {
  local owner="$1" root="$2" cand="$3"

  if is_locked "$owner"; then
    echo "locked git worktree"; return
  fi

  # Match the owning path OR the candidate path anywhere in a build command
  # line. Both are needed: `cd <worktree> && cargo build` has no path in its own
  # argv at all, while the rustc invocations underneath it carry
  # `--out-dir .../target/debug/deps`, which names the candidate, not the owner.
  #
  # NOTE: `grep -q` must not be used in these pipelines. It exits on the first
  # match, the upstream grep takes SIGPIPE, and with `pipefail` set the whole
  # pipeline then reports failure, so the check silently never fires. Counting
  # greps read all input and cannot lose that way.
  local build_lines
  build_lines="$(printf '%s\n' "$PS_SNAPSHOT" | grep -E "$BUILD_PROCESS_RE" || true)"
  if [ -n "$build_lines" ]; then
    if [ "$(printf '%s\n' "$build_lines" | grep -cF -- "$owner")" -gt 0 ]; then
      echo "live build process"; return
    fi
    if [ "$(printf '%s\n' "$build_lines" | grep -cF -- "$cand")" -gt 0 ]; then
      echo "live build process (writing this directory)"; return
    fi
  fi

  # tmux names sessions after the worktree, not after the crate subdirectory, so
  # test path components rather than just the immediate parent. Only components
  # BELOW the scan root are considered: everything at or above it (a username, a
  # "worktrees" directory, the repo name shared by every worktree) appears in
  # unrelated session names and would mark every candidate as in use.
  if [ -n "$TMUX_SNAPSHOT" ]; then
    local rel part
    rel="${owner#"$root"}"
    # When the owner IS the scan root there are no components below it, which
    # used to disable the tmux test entirely for the most natural invocation
    # (`clean.sh <worktree>`, or the default `.` from inside one). Fall back to
    # the root's own basename: the operator named this directory explicitly, so
    # testing its name is exactly what they asked for.
    [ -z "${rel//\//}" ] && rel="$(basename "$root")"
    while IFS= read -r part; do
      [ ${#part} -lt "$MIN_TMUX_COMPONENT" ] && continue
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

# Rule 1, asserted BEFORE the deletion. Every condition here is about the path
# we are one command away from passing to `rm -rf`.
safe_to_delete() {
  local dir="$1" root="$2" base ok n
  [ -d "$dir" ] || { echo "not a directory"; return; }
  case "$dir" in "$root"/*) : ;; *) echo "outside its scan root"; return ;; esac
  base="$(basename "$dir")"
  ok=0
  for n in "${NAMES[@]}"; do [ "$base" = "$n" ] && ok=1; done
  [ "$ok" -eq 1 ] || { echo "basename '$base' is not a target name"; return; }
  [ "$(dirname "$dir")" = "$dir" ] && { echo "has no parent"; return; }
  echo ""
}

# ---- find candidates ----
NAME_ARGS=()
for i in "${!NAMES[@]}"; do
  [ "$i" -gt 0 ] && NAME_ARGS+=(-o)
  NAME_ARGS+=(-name "${NAMES[$i]}")
done

# Candidates live in parallel arrays, never in a delimited text record.
# They used to be written to a temp file as `kb<TAB>dir<TAB>root` and read back
# with `IFS=$'\t' read`, so a path containing a TAB or a newline field-split on
# the way back in: `dir` became a truncated prefix, and the rm ran against the
# SOURCE directory rather than the build directory. Only the sort keys (an
# integer size and an integer index) ever round-trip through text now.
C_KB=(); C_DIR=(); C_ROOT=()

for r in "${ABS_ROOTS[@]}"; do
  while IFS= read -r -d '' d; do
    kb=$(du -sk "$d" 2>/dev/null | cut -f1)
    [ -z "$kb" ] && continue
    [ "$kb" -lt $((MIN_SIZE_MB * 1024)) ] && continue

    # Overlapping roots find the same directory twice with different scan roots,
    # which produced two contradictory verdicts for one path and double-counted
    # the totals. Keep the record whose root is SHORTEST: it yields the longest
    # relative path, so the tmux test gets the most components to check, which
    # is the more protective of the two.
    # ponytail: linear scan, candidates are the >500MB minority so n is tiny.
    dup=-1
    for i in "${!C_DIR[@]}"; do
      [ "${C_DIR[$i]}" = "$d" ] && { dup=$i; break; }
    done
    if [ "$dup" -ge 0 ]; then
      [ ${#r} -lt ${#C_ROOT[$dup]} ] && C_ROOT[$dup]="$r"
      continue
    fi

    C_KB+=("$kb"); C_DIR+=("$d"); C_ROOT+=("$r")
  done < <(find "$r" -maxdepth "$MAXDEPTH" \( -type d \( "${NAME_ARGS[@]}" \) \) -prune -print0 2>/dev/null)
done

human_gb() { awk -v k="$1" 'BEGIN{printf "%.1fG", k/1024/1024}'; }

echo "== disk-space-cleaner =="
echo "roots:   ${ABS_ROOTS[*]}"
echo "targets: ${NAMES[*]}"
echo "min-size: ${MIN_SIZE_MB}MB | apply: $([ "$APPLY" -eq 1 ] && echo yes || echo 'NO (dry-run)')"
[ "${#LOCKED[@]}" -gt 0 ] && echo "protected (locked worktrees): ${#LOCKED[@]}"
[ "$IGNORE_LIVE" -eq 1 ] && echo "WARNING: --ignore-live set, liveness checks disabled"

# Fail closed. An uncollectable signal reads identically to a quiet one, and the
# summary would report "0 skipped as in use" as though everything had been
# checked.
REFUSE_DELETE=0
if [ -n "$DEGRADED" ]; then
  echo "WARNING: liveness incomplete: ${DEGRADED%; }"
  if [ "$APPLY" -eq 1 ] && [ "$IGNORE_LIVE" -eq 0 ]; then
    echo "REFUSING to delete with an incomplete liveness picture."
    echo "Install the missing tool, or re-run with --ignore-live to override."
    REFUSE_DELETE=1
  fi
fi
echo

TOTAL_KB=0; COUNT=0; SKIPPED=0; ACTED=0; FAILED=0; BLOCKED=0

# Sort by size, biggest first, carrying only integers through the pipe.
ORDER=()
while IFS=' ' read -r _kb idx; do
  ORDER+=("$idx")
done < <(for i in "${!C_KB[@]}"; do printf '%s %s\n' "${C_KB[$i]}" "$i"; done | sort -rn)

for idx in "${ORDER[@]:-}"; do
  [ -z "${idx:-}" ] && continue
  kb="${C_KB[$idx]}"; dir="${C_DIR[$idx]}"; root="${C_ROOT[$idx]}"

  # --top caps the candidates ACTED ON, not the ones looked at. Counting skipped
  # ones against the cap meant `--top 3` with three busy directories did nothing
  # at all and never reached the fourth, largest reclaimable one.
  [ "$TOP" -gt 0 ] && [ "$ACTED" -ge "$TOP" ] && break

  owner="$(dirname "$dir")"

  # Under --apply the process and tmux pictures are re-read here rather than
  # reused from before the du pass. Sizing dozens of multi-GB trees takes
  # minutes, and a build started inside that window was invisible to all three
  # checks. lsof is not re-read: it is the slow one, and re-running it per
  # candidate would cost more than the sweep saves.
  [ "$APPLY" -eq 1 ] && [ "$IGNORE_LIVE" -eq 0 ] && refresh_volatile_snapshots

  reason=""
  [ "$IGNORE_LIVE" -eq 0 ] && reason="$(live_reason "$owner" "$root" "$dir")"

  if [ -n "$reason" ]; then
    printf 'skip  %8s  %s\n         (%s)\n' "$(human_gb "$kb")" "$dir" "$reason"
    SKIPPED=$((SKIPPED + 1))
    continue
  fi

  if [ "$APPLY" -eq 1 ]; then
    if [ "$REFUSE_DELETE" -eq 1 ]; then
      printf 'hold  %8s  %s\n         (liveness incomplete)\n' "$(human_gb "$kb")" "$dir"
      BLOCKED=$((BLOCKED + 1))
      continue
    fi
    unsafe="$(safe_to_delete "$dir" "$root")"
    if [ -n "$unsafe" ]; then
      printf 'REFUSED %6s  %s\n         (%s)\n' "$(human_gb "$kb")" "$dir" "$unsafe" >&2
      FAILED=$((FAILED + 1))
      continue
    fi
    ACTED=$((ACTED + 1))
    if rm -rf "$dir"; then
      printf 'rm    %8s  %s\n' "$(human_gb "$kb")" "$dir"
      # Belt and braces. The pre-flight assertion above is the one that can
      # actually prevent the mistake; this only catches an rm that went wider
      # than its argument.
      [ -d "$owner" ] || { echo "         ERROR: owning directory disappeared: $owner" >&2; FAILED=$((FAILED + 1)); }
      TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
    else
      # rm -rf can delete part of a tree and then fail. Counting it, and exiting
      # non-zero, is the difference between a caller seeing a partial deletion
      # and a caller seeing a clean run.
      echo "         FAILED to remove $dir (may be partially deleted)" >&2
      FAILED=$((FAILED + 1))
    fi
  else
    ACTED=$((ACTED + 1))
    printf 'would rm %6s  %s\n' "$(human_gb "$kb")" "$dir"
    TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
  fi
done

echo
printf 'summary: %d dirs %s %s, %d skipped as in use' \
  "$COUNT" "$([ "$APPLY" -eq 1 ] && echo removed || echo 'would free')" \
  "$(human_gb "$TOTAL_KB")" "$SKIPPED"
[ "$BLOCKED" -gt 0 ] && printf ', %d held (liveness incomplete)' "$BLOCKED"
[ "$FAILED" -gt 0 ] && printf ', %d FAILED' "$FAILED"
printf '\n'
[ "$APPLY" -eq 0 ] && echo "(dry-run, re-run with --apply to delete)"

[ "$FAILED" -gt 0 ] && exit 1
[ "$BLOCKED" -gt 0 ] && exit 1
exit 0
