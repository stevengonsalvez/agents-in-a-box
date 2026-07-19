#!/usr/bin/env bash
# disk-space-cleaner: reclaim disk by deleting regenerable build/dependency dirs.
# Deletes NOTHING by default (dry-run). Pass --apply to actually remove.
# No hardcoded paths, no secrets. Everything is passed in or defaulted safely.
set -euo pipefail

# ---- defaults ----
ROOTS=()
NAMES=(node_modules target .next dist build .gradle .turbo)
OLDER_THAN=14          # days; only touch dirs whose mtime is older than this
MAXDEPTH=8
APPLY=0
CARGO_CLEAN=0          # opt-in: run `cargo clean` in the newest rust project per root
PROTECT_LOCKED=1       # skip anything under a `locked` git worktree

usage() {
  cat <<'EOF'
Usage: clean.sh [ROOT ...] [options]

Finds regenerable build/dependency directories and lists them (dry-run).
Add --apply to delete. Recently-touched and git-locked-worktree dirs are
always protected, so active work is never clobbered.

Positional:
  ROOT ...            One or more directories to scan (default: current dir).

Options:
  --older-than N      Only consider dirs untouched for N+ days (default: 14).
  --names a,b,c       Comma-separated dir names to target
                      (default: node_modules,target,.next,dist,build,.gradle,.turbo).
  --max-depth N       How deep to search (default: 8).
  --cargo-clean       Also run `cargo clean` in the newest Cargo project per root.
  --no-protect-locked Do NOT skip git worktrees marked `locked` (not recommended).
  --apply             Actually delete. Without this, only prints what it would do.
  -h, --help          Show this help.

Only these dir NAMES are ever removed. Source files are never touched.
A named `build/` dir can occasionally be source-controlled output — review the
dry-run list before running --apply.
EOF
}

# ---- parse args ----
while [ $# -gt 0 ]; do
  case "$1" in
    --older-than) OLDER_THAN="$2"; shift 2 ;;
    --names) IFS=',' read -r -a NAMES <<< "$2"; shift 2 ;;
    --max-depth) MAXDEPTH="$2"; shift 2 ;;
    --cargo-clean) CARGO_CLEAN=1; shift ;;
    --no-protect-locked) PROTECT_LOCKED=0; shift ;;
    --apply) APPLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage; exit 2 ;;
    *) ROOTS+=("$1"); shift ;;
  esac
done
[ ${#ROOTS[@]} -eq 0 ] && ROOTS=(".")

# ---- collect locked git worktree paths to protect ----
# Ask git itself (--porcelain) for worktrees marked `locked`, per root that is
# a repo. This covers the "active agent session on a locked worktree" case when
# you point the sweep at a repo. Active work inside *nested* repos (roots like
# $HOME) is instead covered by the age gate below — recent = active = skipped.
LOCKED=()
if [ "$PROTECT_LOCKED" -eq 1 ]; then
  for r in "${ROOTS[@]}"; do
    while IFS= read -r wt; do
      [ -n "$wt" ] && LOCKED+=("$wt")
    done < <(git -C "$r" worktree list --porcelain 2>/dev/null \
               | awk '/^worktree /{p=$2} /^locked/{print p}' || true)
  done
fi

is_protected() {
  local path="$1"
  for l in "${LOCKED[@]:-}"; do
    [ -n "$l" ] && case "$path" in "$l"/*|"$l") return 0 ;; esac
  done
  return 1
}

# ---- build the find name filter ----
NAME_ARGS=()
for i in "${!NAMES[@]}"; do
  [ "$i" -gt 0 ] && NAME_ARGS+=(-o)
  NAME_ARGS+=(-name "${NAMES[$i]}")
done

human() { du -sh "$1" 2>/dev/null | cut -f1; }

TOTAL_KB=0
COUNT=0
echo "== disk-space-cleaner =="
echo "roots: ${ROOTS[*]}"
echo "targets: ${NAMES[*]} | older-than: ${OLDER_THAN}d | apply: $([ "$APPLY" -eq 1 ] && echo yes || echo NO/dry-run)"
[ "${#LOCKED[@]}" -gt 0 ] && echo "protected (locked worktrees): ${#LOCKED[@]}"
echo

for r in "${ROOTS[@]}"; do
  while IFS= read -r -d '' d; do
    is_protected "$d" && { echo "skip (locked)  $(human "$d")  $d"; continue; }
    sz_kb=$(du -sk "$d" 2>/dev/null | cut -f1 || echo 0)
    TOTAL_KB=$((TOTAL_KB + sz_kb))
    COUNT=$((COUNT + 1))
    if [ "$APPLY" -eq 1 ]; then
      echo "rm  $(human "$d")  $d"
      rm -rf "$d"
    else
      echo "would rm  $(human "$d")  $d"
    fi
  done < <(find "$r" -maxdepth "$MAXDEPTH" \( -type d \( "${NAME_ARGS[@]}" \) \) \
             -prune -mtime "+${OLDER_THAN}" -print0 2>/dev/null || true)
done

# ---- optional cargo clean on the root-most cargo project per root ----
# Shortest Cargo.toml path == the top-level workspace, whose target/ is the big
# one; a `cargo clean` there wipes the whole workspace's build output.
if [ "$CARGO_CLEAN" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
  for r in "${ROOTS[@]}"; do
    proj="$(find "$r" -maxdepth "$MAXDEPTH" -name Cargo.toml -type f \
              -exec dirname {} \; 2>/dev/null \
              | awk '{print length, $0}' | sort -n | head -1 | cut -d' ' -f2- || true)"
    if [ -n "$proj" ]; then
      if [ "$APPLY" -eq 1 ]; then
        echo "cargo clean in $proj"
        ( cd "$proj" && cargo clean 2>&1 | tail -1 ) || true
      else
        echo "would cargo clean in $proj"
      fi
    fi
  done
fi

echo
printf 'summary: %d dirs, ~%s\n' "$COUNT" \
  "$(awk -v k="$TOTAL_KB" 'BEGIN{printf "%.1f GB", k/1024/1024}')"
[ "$APPLY" -eq 0 ] && echo "(dry-run — re-run with --apply to delete)"
