---
name: disk-space-cleaner
description: |
  Reclaim disk by deleting regenerable build and dependency directories
  (target, node_modules, .next, dist, build, .gradle, .turbo, .venv), biggest
  first, skipping any whose owning directory shows a live process. Dry-run by
  default. Only the build directory is ever removed, never the worktree or repo
  containing it. Use when disk is low, when `df` shows the volume near full, or
  when the user says "clean disk", "free up space", "disk-space-cleaner", or
  "what's eating my disk".
version: "0.2.0"
user-invocable: true
---

# disk-space-cleaner

Free disk by deleting only **regenerable** build and dependency directories.
Never touches source, never removes a worktree.

## The two rules

**1. Only the build directory, never its parent.** Clearing a worktree's
`target/` must leave the worktree itself, with all its source and any
uncommitted work, exactly where it was. The script verifies the owning
directory still exists after each deletion rather than assuming it.

**2. Decide by liveness, not by age.** This is the correction that matters. A
`target/` rebuilt an hour ago by a session that has since finished is safe to
clear. One untouched for a month can belong to a session running right now.
mtime cannot tell those apart, so it is not used as a safety signal at all.

An age gate looks prudent and is actively misleading: on a busy machine every
large build directory is recently touched, so an age-gated sweep skips exactly
the directories worth reclaiming while offering no protection to the ones in
use.

## Step 1: see where the space went

```bash
df -h /System/Volumes/Data                       # macOS data volume
du -xh -d 1 "$HOME" 2>/dev/null | sort -rh | head -20
du -sh ~/.agents-in-a-box/worktrees/by-name/*/*/target 2>/dev/null | sort -rh | head
```

Usual suspects: Rust `target/` (tens of GB each, and one per worktree),
`node_modules`, Python `.venv`, Docker.raw, `~/Library/Caches`.

## Step 2: sweep

```bash
# dry-run, biggest first, nothing removed
scripts/clean.sh ~/.agents-in-a-box/worktrees/by-name

# only large ones, and only the top few
scripts/clean.sh ~/some/dir --min-size 2000 --top 5

# apply after reading the list
scripts/clean.sh ~/some/dir --apply
```

Flags: `--min-size MB` (default 500), `--names a,b,c`, `--max-depth N`,
`--top N`, `--apply`, `--ignore-live`.

Output marks each candidate `would rm` / `rm`, or `skip` with the reason it is
considered in use.

## How "in use" is decided

A candidate is skipped if any of these hold for its owning directory:

| signal | why it matters |
|---|---|
| a `cargo` or `rustc` process has the path in its command line | a build is running; clearing `target/` breaks it mid-compile |
| a tmux session name contains a path component below the scan root | an agent session is live in that worktree |
| any process has its cwd inside the directory | something is working there |

Only components **below the scan root** are used for the tmux test. Components
at or above it (a username, `worktrees`, the repo name shared by every
worktree) appear in unrelated session names and would mark everything as in
use.

`lsof -d cwd` is only collected when `--apply` is passed, because it is slow on
a near-full disk and a dry-run does not need it to be authoritative.

## Two traps this script had to be fixed for

Both were found by dry-running against real worktrees and checking the verdicts
against reality. Keep them in mind if you edit the liveness logic.

**`ps` shows the checker.** A probe whose own command line contains both
`cargo` and a worktree path matches itself, reporting a live build that does not
exist. That false positive protects directories forever and is invisible unless
you verify. The snapshot therefore filters out grep-family processes and the
script's own PID. When checking liveness by hand, exclude `grep` or you will
measure yourself.

**`grep -q` under `pipefail` loses.** `grep -q` exits on first match, the
upstream process in the pipeline takes SIGPIPE, and with `pipefail` set the
whole pipeline reports failure, so the check silently never fires. Use counting
greps (`grep -c`, test `-gt 0`) in any pipeline whose result gates a deletion.

## Step 3: extras, ask first

Not in the script because they need judgement or a password:

```bash
docker system prune -a -f                  # unused images, containers, build cache
docker volume prune -f                     # ONLY if volume data is disposable
uv cache clean; npm cache clean --force; pnpm store prune; yarn cache clean
brew cleanup -s --prune=all
xcrun simctl delete unavailable            # dead iOS simulators
```

On macOS, `~/Library/Caches` regrows to several GB continuously. Clearing
everything under it except `com.apple.*` is safe and reliably reclaims space.

Note that `docker system prune` frees space *inside* Docker.raw but does not
shrink that file on the host, so `df` will not move. Reclaiming it needs Docker
Desktop's own disk-space tooling.

## Safety notes

- A `build/` directory is usually generated but can occasionally be
  source-controlled. Review the dry-run list; drop it from `--names` if a
  project keeps source there.
- `.venv` is in the default names because Python virtualenvs are large and
  regenerable, but recreating one is slower than a rebuild. Drop it from
  `--names` if that trade is wrong for you.
- Everything this skill deletes is rebuilt by `cargo build`,
  `npm/pnpm/yarn install`, or the next build. No permanent loss.
- **Uncommitted work is not protected by anything here.** The script preserves
  it by never touching the worktree, but if a worktree holds uncommitted changes
  the real protection is committing them.
