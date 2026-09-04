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
#      The same check runs in dry-run, so the preview cannot offer a path the
#      apply would refuse.
#   2. Decide by LIVENESS, not by age. A target rebuilt an hour ago by a session
#      that has since finished is safe to clear; one untouched for a month can
#      belong to a session that is running right now. mtime cannot tell those
#      apart, so it is not used as a safety signal.
#   3. Fail CLOSED. A liveness signal that could not be collected is not the
#      same as a signal that came back empty. If ps, tmux or lsof is missing or
#      unreadable, deletions are refused rather than performed blind. This is
#      re-evaluated after every re-read, not decided once at startup: a signal
#      that dies mid-run must stop the run, not ride the verdict it had before.
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
  --top N             Only report on the N largest candidates.
  --apply             Actually delete. Without this, only prints.
  --no-protect-locked Do NOT skip git worktrees marked `locked`. This is the
                      ONLY flag that lifts that protection; --ignore-live does
                      not, because a lock is a human decision, not a signal.
  --ignore-live       Delete even where a live process was detected, and even
                      when a liveness signal could not be collected. Dangerous:
                      these are the checks that stop a running build losing its
                      artifacts mid-compile. Do not use casually.
  -h, --help          Show this help.

A directory is treated as IN USE, and skipped, if any of these hold:
  - it sits in, or under, a git worktree marked `locked`
  - a build process (cargo, rustc, node, npm/pnpm/yarn, next, vite, webpack,
    tsc, esbuild, turbo, gradle, java, python/pip/uv, go) has the owning
    directory OR the candidate itself in its command line
  - a tmux SESSION NAME contains a path component of the owning directory
  - any process has its cwd inside the owning directory

The same checks run in dry-run and under --apply, so the preview is never more
permissive than the action. Under --apply the process and tmux signals are
re-read immediately before each deletion, because the snapshot taken before the
sizing pass is minutes stale by the time the rm happens.

Exit status is 1 if any deletion failed, any candidate was refused by the
pre-flight, or any candidate was held for incomplete liveness. A directory that
simply vanished before the rm is not a failure: the outcome wanted was that it
be gone.
EOF
}

# ---- parse args ----
# A non-numeric count silently disables the guard it was meant to tighten:
# `--top one` made `[ "$TOP" -gt 0 ]` print "integer expression expected", the
# test evaluated false, the cap never fired, and every candidate was deleted.
require_int() {
  case "$2" in
    ''|*[!0-9]*) echo "$1 needs a non-negative integer, got: $2" >&2; exit 2 ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --min-size) require_int --min-size "$2"; MIN_SIZE_MB="$2"; shift 2 ;;
    --names) IFS=',' read -r -a NAMES <<< "$2"; shift 2 ;;
    --max-depth) require_int --max-depth "$2"; MAXDEPTH="$2"; shift 2 ;;
    --top) require_int --top "$2"; TOP="$2"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --no-protect-locked) PROTECT_LOCKED=0; shift ;;
    --ignore-live) IGNORE_LIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage; exit 2 ;;
    *) ROOTS+=("$1"); shift ;;
  esac
done
[ ${#ROOTS[@]} -eq 0 ] && ROOTS=(".")

# Canonicalise every root to an absolute physical path, keeping the logical one.
#
# Absolute, because with a relative root every candidate is `./target` and every
# owner is `.`: a fixed-string search for "." matches essentially every ps and
# lsof line, so the liveness verdict was decided by an unrelated substring.
#
# Both forms, because `pwd -P` resolves symlinks and the processes we are
# searching for usually do not. On macOS /tmp is a symlink to /private/tmp, so a
# live `node /tmp/wt/server.js` contains no substring `/private/tmp/wt` and all
# three liveness greps miss a worktree that is plainly in use.
#
# `cd -P --`, not a bare `cd`: a bare cd honours CDPATH, which both resolves a
# relative root to somewhere else entirely and echoes the destination on stdout,
# so the captured value came back as two lines.
abs_dir() { (cd -P -- "$1" >/dev/null 2>&1 && pwd -P) || return 1; }
logical_dir() { (cd -- "$1" >/dev/null 2>&1 && pwd) || return 1; }

# Trailing slashes are stripped so containment tests never build `//*`, a
# pattern no path matches. Without this `clean.sh /` refused every candidate.
strip_slash() {
  local p="$1"
  while [ "${#p}" -gt 1 ] && [ "${p%/}" != "$p" ]; do p="${p%/}"; done
  printf '%s' "$p"
}

# The prefix to build a `<root>/*` glob from. Empty for the filesystem root,
# because "/" + "/*" is `//*`, which matches nothing: `clean.sh /` refused every
# candidate on the disk and exited 1 after a full-disk du.
glob_prefix() { [ "$1" = "/" ] && printf '' || printf '%s' "$1"; }

ABS_ROOTS=()
ORIG_ROOTS=()   # the spelling the operator typed, kept for liveness matching
for r in "${ROOTS[@]}"; do
  # Command substitution strips trailing newlines, so a root that genuinely ends
  # in one cannot survive `$(pwd)`: `clean.sh $'/tmp/repo\n'` would resolve to
  # the SIBLING `/tmp/repo` and delete its target instead. There is no encoding
  # that fixes this, so it is refused rather than silently redirected.
  case "$r" in
    *$'\n') echo "refusing a root whose path ends in a newline: $(printf '%q' "$r")" >&2; exit 2 ;;
  esac
  if a="$(abs_dir "$r")"; then
    ABS_ROOTS+=("$(strip_slash "$a")")
    ORIG_ROOTS+=("$(strip_slash "$r")")
  else
    echo "skipping unreadable root: $r" >&2
  fi
done
[ ${#ABS_ROOTS[@]} -eq 0 ] && { echo "no readable roots" >&2; exit 2; }

# ---- liveness signals ----

# Commands whose presence in a command line means something is building. The
# previous version matched only `cargo ` and `rustc `, while the default target
# list covered node_modules, .next and .venv, so every non-Rust build was
# invisible to the one signal advertised as primary.
BUILD_PROCESS_RE='cargo |rustc |node |npm |pnpm |yarn |next |vite |webpack |tsc |esbuild |turbo |gradle |java |python|pip |uv |go build|go run'

# Reasons a signal could not be collected. Non-empty means fail closed.
DEGRADED=""
REFUSE_DELETE=0

have() { command -v "$1" >/dev/null 2>&1; }

note_degraded() {
  case "$DEGRADED" in
    *"$1"*) : ;;
    *) DEGRADED="${DEGRADED}$1; " ;;
  esac
  # Re-evaluated here, not once before the loop. A signal that dies mid-run used
  # to keep the verdict computed at startup, so --apply carried on deleting with
  # an empty process table and reported a clean run.
  [ "$APPLY" -eq 1 ] && [ "$IGNORE_LIVE" -eq 0 ] && REFUSE_DELETE=1
  return 0
}

# This script's own path, used to drop our own subshells from the process table.
# Matching the bare string "clean.sh" also deleted unrelated live builds whose
# argv merely contained it (`node /tmp/clean.sh-project/build.js ...`), which is
# a fail-OPEN filter on the primary liveness signal.
SELF_PATH="$(abs_dir "$(dirname -- "$0")" 2>/dev/null)/$(basename -- "$0")"

collect_ps() {
  # Exclude grep/ripgrep lines and this script's own process. `ps` shows the
  # command line of whatever is doing the checking, and a probe that mentions
  # both "cargo" and a worktree path will match itself, reporting a live build
  # that does not exist. That false positive protects directories forever.
  ps -Awwo pid,command 2>/dev/null \
    | grep -v -E '(^| )[0-9]+ +(grep|rg|egrep|fgrep) ' \
    | grep -v -E "^ *$$ " \
    | grep -vF -- "$SELF_PATH"
}

# SESSION NAMES ONLY. `tmux ls` renders `name: 1 windows (created ...)`, and a
# fixed-string search over that whole line matches the boilerplate: with a
# three-character floor, path components like `win`, `dow`, `ate` and `cre` hit
# every session on the host and the sweep protected everything while reporting
# it as safety.
#
# Exit 1 with no output means "no server running", which is a real answer.
# Anything else (an unreadable socket dir, a permission failure) is a collection
# FAILURE and must degrade, not read as "no sessions".
collect_tmux() {
  local out status
  out="$(tmux ls -F '#{session_name}' 2>/dev/null)"; status=$?
  if [ "$status" -ne 0 ] && [ "$status" -ne 1 ]; then
    note_degraded "tmux unreadable"
  fi
  printf '%s' "$out"
}

PS_SNAPSHOT=""
PS_BUILD_LINES=""
TMUX_SNAPSHOT=""
LSOF_SNAPSHOT=""

refresh_volatile_snapshots() {
  PS_SNAPSHOT="$(collect_ps || true)"
  if [ -z "$PS_SNAPSHOT" ]; then
    # ps always reports at least this script, so empty means it did not run.
    note_degraded "ps unreadable"
  fi
  # Filtered once per refresh rather than once per candidate: live_reason used
  # to re-scan the whole process table for every directory it looked at.
  PS_BUILD_LINES="$(printf '%s\n' "$PS_SNAPSHOT" | grep -E "$BUILD_PROCESS_RE" || true)"
  if have tmux; then
    TMUX_SNAPSHOT="$(collect_tmux)"
  else
    TMUX_SNAPSHOT=""
    note_degraded "tmux not installed"
  fi
}

have ps || note_degraded "ps not installed"
refresh_volatile_snapshots

# `lsof -d cwd` is the expensive one, and it used to be collected only under
# --apply. That made the dry-run a strict superset of what --apply would touch,
# so the list the operator approved was produced with one of three guards
# switched off. A preview must be at least as protective as the action it
# previews, so it is now always collected.
if have lsof; then
  # An installed lsof that FAILS is not an empty process table. lsof exits 1
  # when it merely found nothing, so only other statuses degrade.
  LSOF_SNAPSHOT="$(lsof -d cwd 2>/dev/null)"; lsof_status=$?
  if [ "$lsof_status" -ne 0 ] && [ "$lsof_status" -ne 1 ]; then
    note_degraded "lsof unreadable"
  fi
else
  note_degraded "lsof not installed"
fi

have git || note_degraded "git not installed (locked worktrees unknown)"

# ---- locked git worktrees ----
# An explicit, deterministic human "do not touch" marker that outlives the
# session that set it. The process and tmux signals only see what is running at
# snapshot time, so a locked worktree that is mid-rebase, paused, or whose
# terminal was closed has no other protection.
#
# Asked of the CANDIDATE's own directory, not of the scan roots. Enumerating
# from the roots meant `git -C <worktrees parent> worktree list` exited 128 with
# "not a git repository" for the canonical sweep-the-parent invocation, stderr
# went to /dev/null, and the protection was silently off in exactly the case it
# exists for.
#
# Read from the gitdir rather than parsed out of `worktree list --porcelain`:
# that porcelain path is whitespace-delimited, so an awk field-split truncated
# any locked worktree whose path contains a space and let it through. A locked
# worktree simply has a `locked` file in its gitdir; there is nothing to parse.
is_locked() {
  local d="$1" gitdir common
  have git || return 1
  gitdir="$(git -C "$d" rev-parse --absolute-git-dir 2>/dev/null)" || return 1
  [ -n "$gitdir" ] || return 1
  # A linked worktree's gitdir is <main>/.git/worktrees/<name>, and its lock is
  # <that>/locked. A main worktree cannot be locked at all.
  [ -f "$gitdir/locked" ] && return 0
  # A candidate can also sit under a locked worktree without being its root.
  common="$(git -C "$d" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
  [ -n "$common" ] && [ "$gitdir" != "$common" ] && [ -f "$gitdir/locked" ] && return 0
  return 1
}

# Why is this directory in use? Echoes a reason, or nothing if it looks idle.
# $1 = the directory that OWNS the build dir (the worktree, not the target)
# $2 = the absolute scan root this candidate was found under
# $3 = the candidate directory itself
live_reason() {
  local owner="$1" root="$2" cand="$3" oroot="${4:-}"
  # The same directory as the operator spelled it. Roots are canonicalised for
  # our own bookkeeping, but a process started as
  # `node /srv/repos/team/../team/app/x` carries the operator's spelling in its
  # argv and nothing else. Testing only the canonical form was a regression:
  # the previous version passed the root through to find unresolved, so that
  # argv matched.
  local owner_orig="" cand_orig=""
  if [ -n "$oroot" ] && [ "$oroot" != "$root" ]; then
    owner_orig="$oroot${owner#"$root"}"
    cand_orig="$oroot${cand#"$root"}"
  fi

  # Match the owning path OR the candidate path anywhere in a build command
  # line. Both are needed: `cd <worktree> && cargo build` has no path in its own
  # argv at all, while the rustc invocations underneath it carry
  # `--out-dir .../target/debug/deps`, which names the candidate, not the owner.
  #
  # Each path is tested in both its physical and its logical form, since the
  # process may have been started through a symlink we have already resolved.
  #
  # NOTE: `grep -q` must not be used in these pipelines. It exits on the first
  # match, the upstream grep takes SIGPIPE, and with `pipefail` set the whole
  # pipeline then reports failure, so the check silently never fires. Counting
  # greps read all input and cannot lose that way.
  if [ -n "$PS_BUILD_LINES" ]; then
    local p
    for p in "$owner" "$(logical_dir "$owner" 2>/dev/null)" "$owner_orig" \
             "$cand" "$cand_orig"; do
      [ -z "$p" ] && continue
      if [ "$(printf '%s\n' "$PS_BUILD_LINES" | grep -cF -- "$p")" -gt 0 ]; then
        echo "live build process"; return
      fi
    done
  fi

  # tmux names sessions after the worktree, not after the crate subdirectory, so
  # test path components rather than just the immediate parent. Only components
  # BELOW the scan root are considered: everything at or above it (a username, a
  # "worktrees" directory, the repo name shared by every worktree) appears in
  # unrelated session names and would mark every candidate as in use.
  if [ -n "$TMUX_SNAPSHOT" ]; then
    local rel part pref
    pref="$(glob_prefix "$root")"
    case "$owner" in
      "$pref"/*) rel="${owner#"$pref"}" ;;
      # The owner is the scan root, or the root is deeper than the owner (which
      # happens when the operator points straight at a build directory). Either
      # way there is nothing below the root to test, so use the owner's own
      # basename: that is the directory the operator named. Stripping nothing
      # and testing the whole absolute path instead would match the username and
      # every shared parent, which is the false positive this block avoids.
      *) rel="$(basename "$owner")" ;;
    esac
    [ -z "${rel//\//}" ] && rel="$(basename "$owner")"
    while IFS= read -r part; do
      [ ${#part} -lt "$MIN_TMUX_COMPONENT" ] && continue
      if [ "$(printf '%s\n' "$TMUX_SNAPSHOT" | grep -cF -- "$part")" -gt 0 ]; then
        echo "live tmux session"; return
      fi
    done < <(printf '%s\n' "$rel" | tr '/' '\n')
  fi

  if [ -n "$LSOF_SNAPSHOT" ]; then
    local p
    for p in "$owner" "$(logical_dir "$owner" 2>/dev/null)" "$owner_orig"; do
      [ -z "$p" ] && continue
      if [ "$(printf '%s\n' "$LSOF_SNAPSHOT" | grep -cF -- "$p")" -gt 0 ]; then
        echo "process cwd inside"; return
      fi
    done
  fi
  echo ""
}

# Rule 1, asserted BEFORE the deletion and in dry-run alike. Every condition
# here is about the path we would be one command away from passing to `rm -rf`.
#
# Echoes "" when safe, "gone" when the directory has simply vanished (a benign
# race with another cleaner or a `cargo clean`, whose outcome is what we wanted
# anyway), or a reason string when it is a genuine rule-1 violation.
safe_to_delete() {
  local dir="$1" root="$2" base ok n pref
  [ -d "$dir" ] || { echo "gone"; return; }
  # `dir == root` is legitimate: the operator pointed straight at a build
  # directory. Only a path OUTSIDE the root is a violation.
  pref="$(glob_prefix "$root")"
  case "$dir" in
    "$root"|"$pref"/*) : ;;
    *) echo "outside its scan root"; return ;;
  esac
  base="$(basename "$dir")"
  ok=0
  # Glob-matched, not compared literally, so this agrees with the `find -name`
  # predicate that produced the candidate. A literal test rejected every
  # candidate found through a pattern (`--names 'target*'`), turning a working
  # sweep into an all-REFUSED run.
  for n in "${NAMES[@]}"; do
    case "$base" in $n) ok=1 ;; esac
  done
  [ "$ok" -eq 1 ] || { echo "basename '$base' is not a target name"; return; }
  [ "$(dirname "$dir")" = "$dir" ] && { echo "has no parent"; return; }
  # A mountpoint is another filesystem borrowing this path. `rm -rf` descends
  # into it and destroys data that has nothing to do with this build tree, and
  # nothing above would notice: the basename is still `target`, and it is still
  # under the scan root.
  if [ -d "$dir" ] && [ -d "$dir/.." ]; then
    local dev_self dev_parent
    dev_self="$(stat -f '%d' "$dir" 2>/dev/null || stat -c '%d' "$dir" 2>/dev/null)"
    dev_parent="$(stat -f '%d' "$dir/.." 2>/dev/null || stat -c '%d' "$dir/.." 2>/dev/null)"
    if [ -n "$dev_self" ] && [ -n "$dev_parent" ] && [ "$dev_self" != "$dev_parent" ]; then
      echo "is a mountpoint"; return
    fi
  fi
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
C_KB=(); C_DIR=(); C_ROOT=(); C_OROOT=()

for ri in "${!ABS_ROOTS[@]}"; do
  r="${ABS_ROOTS[$ri]}"; oroot="${ORIG_ROOTS[$ri]}"
  while IFS= read -r -d '' d; do
    d="$(strip_slash "$d")"
    # `du -sk` echoes the path it sized, so a path containing a NEWLINE makes
    # its output span lines and `cut -f1` returns a multi-line value. That value
    # then rides the integer-only sort channel and can order a 1 KiB candidate
    # ahead of a 10 GiB one, which under `--top 1 --apply` deletes the wrong
    # directory. Anything that is not a plain integer is not a size.
    kb=$(du -sk "$d" 2>/dev/null | head -1 | cut -f1)
    case "$kb" in
      ''|*[!0-9]*) echo "skipping (unreadable size): $(printf '%q' "$d")" >&2; continue ;;
    esac
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
      if [ ${#r} -lt ${#C_ROOT[$dup]} ]; then
        C_ROOT[$dup]="$r"; C_OROOT[$dup]="$oroot"
      fi
      continue
    fi

    C_KB+=("$kb"); C_DIR+=("$d"); C_ROOT+=("$r"); C_OROOT+=("$oroot")
  done < <(find "$r" -maxdepth "$MAXDEPTH" \( -type d \( "${NAME_ARGS[@]}" \) \) -prune -print0 2>/dev/null)
done

human_gb() { awk -v k="$1" 'BEGIN{printf "%.1fG", k/1024/1024}'; }

echo "== disk-space-cleaner =="
echo "roots:   ${ABS_ROOTS[*]}"
echo "targets: ${NAMES[*]}"
echo "min-size: ${MIN_SIZE_MB}MB | apply: $([ "$APPLY" -eq 1 ] && echo yes || echo 'NO (dry-run)')"
[ "$PROTECT_LOCKED" -eq 0 ] && echo "WARNING: --no-protect-locked set, locked worktrees are eligible"
[ "$IGNORE_LIVE" -eq 1 ] && echo "WARNING: --ignore-live set, liveness checks disabled (locked worktrees still protected)"

# Fail closed. An uncollectable signal reads identically to a quiet one, and the
# summary would report "0 skipped as in use" as though everything had been
# checked.
if [ -n "$DEGRADED" ]; then
  echo "WARNING: liveness incomplete: ${DEGRADED%; }"
  if [ "$REFUSE_DELETE" -eq 1 ]; then
    echo "REFUSING to delete with an incomplete liveness picture."
    echo "Install the missing tool, or re-run with --ignore-live to override."
  fi
fi
echo

TOTAL_KB=0; COUNT=0; SKIPPED=0; LOCKED_N=0; SHOWN=0; FAILED=0; BLOCKED=0; VANISHED=0

# Sort by size, biggest first, carrying only integers through the pipe.
ORDER=()
while IFS=' ' read -r _kb idx; do
  ORDER+=("$idx")
done < <(for i in "${!C_KB[@]}"; do printf '%s %s\n' "${C_KB[$i]}" "$i"; done | sort -rn)

for idx in "${ORDER[@]:-}"; do
  [ -z "${idx:-}" ] && continue
  kb="${C_KB[$idx]}"; dir="${C_DIR[$idx]}"; root="${C_ROOT[$idx]}"; oroot="${C_OROOT[$idx]}"

  # --top caps the candidates REPORTED ON, including the ones held or refused.
  # Keying it on successful actions alone meant a degraded run walked every
  # candidate on the disk emitting two lines each, and `--top 3` with three busy
  # directories did nothing and never reached the fourth.
  [ "$TOP" -gt 0 ] && [ "$SHOWN" -ge "$TOP" ] && break

  owner="$(dirname "$dir")"

  # A lock is a human decision, not a liveness signal, so it is tested outside
  # live_reason and --ignore-live does not lift it. Only --no-protect-locked
  # does, which is what its help text says.
  if [ "$PROTECT_LOCKED" -eq 1 ] && is_locked "$owner"; then
    SHOWN=$((SHOWN + 1)); LOCKED_N=$((LOCKED_N + 1)); SKIPPED=$((SKIPPED + 1))
    printf 'skip  %8s  %s\n         (locked git worktree)\n' "$(human_gb "$kb")" "$dir"
    continue
  fi

  # Under --apply the process and tmux pictures are re-read here rather than
  # reused from before the du pass. Sizing dozens of multi-GB trees takes
  # minutes, and a build started inside that window was invisible to all three
  # checks. Skipped once REFUSE_DELETE is set: every remaining candidate is
  # going to be held regardless, so the process table need not be dumped again.
  # lsof is not re-read: it is the slow one, and re-running it per candidate
  # would cost more than the sweep saves.
  [ "$APPLY" -eq 1 ] && [ "$IGNORE_LIVE" -eq 0 ] && [ "$REFUSE_DELETE" -eq 0 ] \
    && refresh_volatile_snapshots

  reason=""
  [ "$IGNORE_LIVE" -eq 0 ] && reason="$(live_reason "$owner" "$root" "$dir" "$oroot")"

  if [ -n "$reason" ]; then
    SHOWN=$((SHOWN + 1)); SKIPPED=$((SKIPPED + 1))
    printf 'skip  %8s  %s\n         (%s)\n' "$(human_gb "$kb")" "$dir" "$reason"
    continue
  fi

  # Rule 1 runs for every candidate, in dry-run too, so the preview cannot list
  # a path the apply would then refuse.
  unsafe="$(safe_to_delete "$dir" "$root")"
  if [ "$unsafe" = "gone" ]; then
    # Not a failure. Another cleaner, or a `cargo clean`, got there first during
    # the multi-minute sizing pass, and the outcome is the one we wanted.
    SHOWN=$((SHOWN + 1)); VANISHED=$((VANISHED + 1))
    printf 'gone  %8s  %s\n' "$(human_gb "$kb")" "$dir"
    continue
  fi
  if [ -n "$unsafe" ]; then
    SHOWN=$((SHOWN + 1)); FAILED=$((FAILED + 1))
    printf 'REFUSED %6s  %s\n         (%s)\n' "$(human_gb "$kb")" "$dir" "$unsafe" >&2
    continue
  fi

  if [ "$APPLY" -eq 1 ] && [ "$REFUSE_DELETE" -eq 1 ]; then
    SHOWN=$((SHOWN + 1)); BLOCKED=$((BLOCKED + 1))
    printf 'hold  %8s  %s\n         (liveness incomplete)\n' "$(human_gb "$kb")" "$dir"
    continue
  fi

  SHOWN=$((SHOWN + 1))
  if [ "$APPLY" -eq 1 ]; then
    # Last look before the destructive call. `rm -rf` on a path that no longer
    # exists succeeds, so without this a directory renamed out from under us
    # between the pre-flight and here was reported as a reclaim of its full
    # size while every byte survived under the new name.
    if [ ! -d "$dir" ]; then
      VANISHED=$((VANISHED + 1))
      printf 'gone  %8s  %s\n' "$(human_gb "$kb")" "$dir"
      continue
    fi
    if rm -rf "$dir"; then
      # Belt and braces. The pre-flight assertion above is the one that can
      # actually prevent the mistake; this only catches an rm that went wider
      # than its argument. It is NOT counted as a reclaim: folding the worst
      # outcome this script guards against into the freed total would let a
      # destroyed worktree read as a successful 0.6G sweep.
      if [ -d "$owner" ]; then
        printf 'rm    %8s  %s\n' "$(human_gb "$kb")" "$dir"
        TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
      else
        echo "         RULE 1 VIOLATED: the owning directory was destroyed: $owner" >&2
        FAILED=$((FAILED + 1))
      fi
    else
      # rm -rf can delete part of a tree and then fail. Counting it, and exiting
      # non-zero, is the difference between a caller seeing a partial deletion
      # and a caller seeing a clean run.
      echo "         FAILED to remove $dir (may be partially deleted)" >&2
      FAILED=$((FAILED + 1))
    fi
  else
    printf 'would rm %6s  %s\n' "$(human_gb "$kb")" "$dir"
    TOTAL_KB=$((TOTAL_KB + kb)); COUNT=$((COUNT + 1))
  fi
done

echo
printf 'summary: %d dirs %s %s, %d skipped as in use' \
  "$COUNT" "$([ "$APPLY" -eq 1 ] && echo removed || echo 'would free')" \
  "$(human_gb "$TOTAL_KB")" "$SKIPPED"
[ "$LOCKED_N" -gt 0 ] && printf ' (%d locked)' "$LOCKED_N"
[ "$VANISHED" -gt 0 ] && printf ', %d already gone' "$VANISHED"
[ "$BLOCKED" -gt 0 ] && printf ', %d held (liveness incomplete)' "$BLOCKED"
[ "$FAILED" -gt 0 ] && printf ', %d FAILED' "$FAILED"
printf '\n'
[ "$APPLY" -eq 0 ] && echo "(dry-run, re-run with --apply to delete)"

[ "$FAILED" -gt 0 ] && exit 1
[ "$BLOCKED" -gt 0 ] && exit 1
exit 0
