---
name: disk-space-cleaner
description: |
  Reclaim disk space by finding and deleting regenerable build/dependency
  directories (node_modules, target, .next, dist, build, .gradle, .turbo)
  and optionally running `cargo clean` and Docker prune. Dry-run by default,
  age-gated, and protects git-locked worktrees plus recently-touched work so
  active sessions are never clobbered. Use when disk is low, when `df` shows
  the volume near full, or when the user says "clean disk", "free up space",
  "disk-space-cleaner", or "what's eating my disk".
version: "0.1.0"
user-invocable: true
---

# disk-space-cleaner

Free disk by deleting only **regenerable** build/dependency dirs. Never
touches source. Dry-run first, always.

## Golden rules

1. **Diagnose before deleting.** Show the user where space went.
2. **Dry-run, then apply.** The script deletes nothing without `--apply`.
3. **Only regenerable dirs.** `node_modules` (reinstall), `target` /
   `.next` / `dist` / `build` / `.gradle` / `.turbo` (rebuild). Source is
   never a target.
4. **Protect active work.** Skip anything modified in the last N days
   (default 14) and anything under a git worktree marked `locked`.
5. **Confirm the big irreversible ones** (Docker volumes, `sudo rm` of OS
   installer leftovers) with the user before running.

## Step 1 — diagnose

Find the biggest consumers. Hidden dirs (dotfiles, `~/Library` on macOS)
are the usual culprits.

```bash
df -h .                                        # how full, how much free
du -xh -d 1 "$HOME" 2>/dev/null | sort -rh | head -20
du -xh -d 2 <big-dir> 2>/dev/null | sort -rh | head -25   # drill down
```

Common hogs: Rust `target/` (tens of GB of debug artifacts), per-worktree
`node_modules`/`target`, Docker.raw, ML model caches (`~/.cache/huggingface`,
`~/.cache/uv`), emulator images (`~/.android/avd`), iOS simulators.

## Step 2 — sweep (this skill's script)

```bash
# dry-run: list what WOULD be deleted, nothing removed
scripts/clean.sh ~/some/dir ~/another/dir

# apply after reviewing the list
scripts/clean.sh ~/some/dir --apply

# tune age gate + include cargo clean on the newest Cargo project per root
scripts/clean.sh . --older-than 21 --cargo-clean --apply
```

Flags: `--older-than N` (days, default 14), `--names a,b,c` (dir names to
target), `--max-depth N`, `--cargo-clean`, `--no-protect-locked`, `--apply`.
Run `scripts/clean.sh --help` for the full list.

The script prints `would rm <size> <path>` per dir and a total, protects
locked worktrees (`skip (locked)`), and only removes dirs older than the age
gate — so anything you're actively building is left alone.

## Step 3 — extras (opt-in, ask first)

These are handy but not in the script because they need judgment or a
password:

```bash
docker system prune -a -f                      # unused images/containers/build cache
docker volume prune -f                          # ONLY if the user confirms volume data is disposable
uv cache clean;  npm cache clean --force;  pnpm store prune;  yarn cache clean
brew cleanup -s --prune=all
xcrun simctl delete unavailable                 # dead iOS simulators (macOS)
sudo rm -rf "/System/Volumes/Data/macOS Install Data"   # OS installer leftovers (needs sudo)
```

For Rust specifically, a per-project `cargo clean` (or `--cargo-clean` on the
script) is the single biggest instant win — `target/debug` is routinely the
fattest deletable dir on the machine.

## Safety notes

- A `build/` directory is *usually* generated but can occasionally be
  source-controlled. Review the dry-run list before `--apply`; drop it from
  `--names` if a project keeps source there.
- Deleting `node_modules`/`target` in a worktree where an agent session is
  mid-build breaks that build. The age gate handles this (recent = active),
  but when in doubt, check for running sessions first.
- Everything this skill deletes is rebuilt by `cargo build`,
  `npm/pnpm/yarn install`, or the next build — no permanent loss.
