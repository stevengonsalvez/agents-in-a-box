---
name: disk-space-cleaner
description: |
  Reclaim disk by deleting regenerable build and dependency directories
  (target, node_modules, .next, build, .turbo, .venv), biggest first, skipping
  any whose owning directory shows a live process, a live tmux session, or a
  git worktree lock. Dry-run by default, and the dry-run runs every check the
  apply does. Only the build directory is ever removed, never the worktree or
  repo containing it. Use when disk is low, when `df` shows the volume near
  full, or when the user says "clean disk", "free up space",
  "disk-space-cleaner", or "what's eating my disk".
version: "0.3.0"
user-invocable: true
---

# disk-space-cleaner

Free disk by deleting only **regenerable** build and dependency directories.
Never touches source, never removes a worktree.

## The three rules

**1. Only the build directory, never its parent.** Clearing a worktree's
`target/` must leave the worktree itself, with all its source and any
uncommitted work, exactly where it was. The script asserts this **before** the
`rm`: the candidate must still be a directory, must live under its scan root,
and its basename must be one of the target names. A check that runs after the
deletion cannot prevent anything.

**2. Decide by liveness, not by age.** A `target/` rebuilt an hour ago by a
session that has since finished is safe to clear. One untouched for a month can
belong to a session running right now. mtime cannot tell those apart, so it is
not used as a safety signal at all.

An age gate looks prudent and is actively misleading: on a busy machine every
large build directory is recently touched, so an age-gated sweep skips exactly
the directories worth reclaiming while offering no protection to the ones in
use.

**3. Fail closed.** A signal that could not be collected is not the same as a
signal that came back empty. If `ps`, `tmux`, `lsof` or `git` is missing or
unreadable, `--apply` refuses and reports `held (liveness incomplete)` rather
than deleting blind. This is re-evaluated after every re-read, not decided once
at startup: a `ps` that succeeds before the sizing pass and fails during it must
stop the run, not ride the verdict it had before. An installed tool that exits
with an error counts as unreadable; only the exit status that genuinely means
"nothing found" is treated as an empty answer. `--ignore-live` overrides,
deliberately and loudly.

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
`--top N`, `--apply`, `--no-protect-locked`, `--ignore-live`.

Output marks each candidate `would rm` / `rm`, `skip` with the reason it is
considered in use, `hold` when liveness could not be established, `gone` when
the directory disappeared before the delete, or `REFUSED` when the pre-flight
assertion rejected the path.

Exit status is `1` if any deletion failed, any candidate was refused, or any
candidate was held; `2` for a bad argument; `0` otherwise. A partially-deleted
tree counts as a failure: `rm -rf` can remove part of a directory and then
error, and a caller checking `$?` must not read that as a clean run. A directory
that simply vanished is *not* a failure — another cleaner or a `cargo clean`
got there first, and the outcome is the one that was wanted.

The pre-flight refuses a path that is not under its scan root, whose basename
does not match the target names, or that is a **mountpoint** — another
filesystem borrowing the path, whose contents `rm -rf` would happily descend
into and destroy.

Counts are validated: `--top one` used to make the cap test print "integer
expression expected", evaluate false, and delete everything. Bad numbers now
exit 2.

## How "in use" is decided

A candidate is skipped if any of these hold for its owning directory:

| signal | why it matters |
|---|---|
| the owner is, or sits under, a git worktree marked `locked` | an explicit human "do not touch" that outlives the session which set it |
| a build process has the owner's path, or the candidate's own path, in its command line | a build is running; clearing `target/` breaks it mid-compile |
| a tmux **session name** contains a path component of the owner | an agent session is live in that worktree |
| any process has its cwd inside the owner | something is working there |

The lock is asked of the **candidate's own directory**, by looking for a
`locked` file in its gitdir. Enumerating from the scan roots instead meant
`git -C <worktrees parent> worktree list` exited 128 with "not a git
repository" for the canonical sweep-the-parent invocation, and the protection
was silently off in exactly the case it exists for. Reading the gitdir also
avoids parsing `worktree list --porcelain`, whose path field is
whitespace-delimited and truncated any locked worktree whose path contained a
space.

A lock is a human decision, not a liveness signal, so **`--ignore-live` does not
lift it**. Only `--no-protect-locked` does.

Build processes matched: `cargo`, `rustc`, `node`, `npm`, `pnpm`, `yarn`,
`next`, `vite`, `webpack`, `tsc`, `esbuild`, `turbo`, `gradle`, `java`,
`python`, `pip`, `uv`, `go build`, `go run`. The default target names are not
all Rust, so a Rust-only pattern left every other build undetectable.

Both the owner path and the candidate path are matched, because they catch
different invocations. `cd <worktree> && cargo build` has no path in its own
argv at all; the `rustc` calls underneath it carry
`--out-dir .../target/debug/deps`, which names the candidate rather than the
owner.

For the tmux test, components **below the scan root** are used. Components at
or above it (a username, `worktrees`, the repo name shared by every worktree)
appear in unrelated session names and would mark everything as in use. When the
owner *is* the scan root, which is what `clean.sh <worktree>` and the default
`.` produce, there are no components below it, so the root's own basename is
used instead: the operator named that directory explicitly.

Components shorter than **3 characters** are ignored, since one or two
characters match almost any session name by accident. Real worktree names like
`api`, `web`, `ainb` and `hangar` are therefore covered.

Every check runs in dry-run and under `--apply` alike, `lsof` and the rule-1
pre-flight included. A preview produced with one of the guards switched off is
not a preview of anything, and a preview that lists a path the apply will refuse
is worse than none.

Paths are matched in several spellings, because the processes being searched for
do not use ours. The canonical physical path, the logical path (macOS `/tmp` is a
symlink to `/private/tmp`, so a live `node /tmp/wt/server.js` contains no
`/private/tmp/wt`), and the spelling the operator typed on the command line are
all tested.

Under `--apply` the `ps` and `tmux` pictures are re-read immediately before each
deletion. Sizing dozens of multi-GB trees takes minutes, and a build started
inside that window is invisible to a snapshot taken before it.

**Known limit:** `lsof` is *not* re-read per candidate. It is the slow one, and
per-candidate reruns would cost more than the sweep saves, so a shell that `cd`s
into a worktree during the sizing pass is not detected unless it also shows up
in `ps` or tmux. **Known limit:** the mountpoint check compares device ids, so
it catches a genuinely separate volume but not an APFS firmlink, which shares
one.

## Traps this script had to be fixed for

Keep them in mind if you edit the liveness logic.

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

**A delimited text record splits on the data.** Candidates were once written to
a temp file as `kb<TAB>dir<TAB>root` and read back with `IFS=$'\t' read`. A path
containing a TAB or a newline field-split on the way back in, `dir` became a
truncated prefix, and the `rm -rf` ran against the source directory while the
real build directory survived. Reproduced, confirmed. Paths now live in bash
arrays and only the sort keys, two integers, ever round-trip through text.

The same class bit twice more, so treat any path-carrying text channel as
hostile. `git worktree list --porcelain` is whitespace-delimited, so an awk
field-split truncated locked worktrees whose path contained a space; the lock is
now read from the gitdir with nothing to parse. And `du -sk` echoes the path it
sized, so a path with a newline made its output span lines and `cut -f1`
returned a multi-line "size" that then rode the integer-only sort channel and
could order a 1 KiB candidate ahead of a 10 GiB one. Sizes are validated as
plain integers before they are trusted.

**A fixed-string exclusion is a fail-open filter.** The process snapshot drops
this script's own subshells, and matching the bare string `clean.sh` also
dropped any unrelated live build whose command line happened to contain it, on
the one signal that matters most. It matches the script's absolute path now.

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

- **`dist` and `.gradle` are not default targets.** `dist/` is routinely a
  deployed artifact directory with no source, no package.json and nothing local
  able to rebuild it; `~/.gradle` is a tool home holding `jdks/`, `native/` and
  the daemon registry rather than project output. Both are still available
  through `--names` when you know your layout.
- A `build/` directory is usually generated but can occasionally be
  source-controlled. Review the dry-run list; drop it from `--names` if a
  project keeps source there.
- `.venv` is in the default names because Python virtualenvs are large and
  regenerable, but recreating one is slower than a rebuild. Drop it from
  `--names` if that trade is wrong for you.
- **Most of what this deletes is rebuilt by the next build, but not all of it,
  and the script cannot tell which is which.** It matches directory names, not
  provenance. Read the dry-run list before applying, and treat any hit outside
  a project you recognise as suspect.
- **Uncommitted work is not protected by anything here.** The script preserves
  it by never touching the worktree, but if a worktree holds uncommitted changes
  the real protection is committing them.
