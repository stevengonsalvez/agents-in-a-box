---
name: git-history-surgery
description: Safely split, reorder, or rewrite already-pushed commits using a throwaway worktree. Isolates the surgery from any existing checkout, verifies byte-identical tree before force-pushing, and uses --force-with-lease to refuse silent clobbers from concurrent work. Use when asked to split a pushed commit into atomic pieces, fix a bulked commit after the fact, or reshape recent history on a shared branch.
---

# Git History Surgery

## When to invoke

Trigger on any of:
- "split this commit into smaller ones" (after a push)
- "rebase this into atomic commits"
- User points out a bulk commit and asks to fix history
- You bulked a multi-concern change into one commit and caught it before/after pushing

## Why a throwaway worktree

Doing `git reset --soft HEAD~1` in an *existing* worktree is risky:
- Another session might be on the same repo in a different worktree/branch. Your reset doesn't affect them, but switching branches there will.
- If the worktree you're in isn't on the branch you think (e.g. ops/main worktree was on `fix/usage-async-load` in a past session), a reset rewrites the *wrong* branch.
- Existing worktrees may carry uncommitted state you'd lose.

**The safe pattern: fresh detached worktree at origin/<branch>, do surgery, push, discard.**

## The pattern

```bash
# 1. Create throwaway worktree at the exact commit you want to rewrite
TMPWT=$(mktemp -d -t history-split-XXXXXX)
git fetch origin
git worktree add --detach "$TMPWT" origin/<branch>
cd "$TMPWT"

# 2. Verify you're where you expect
git log -3 --oneline

# 3. Undo the commit(s) keeping working-tree content
git reset --soft HEAD~1   # or HEAD~N for multiple
git reset                 # unstage so you can stage atomic chunks

# 4. Reset the file(s) to the *pre-change* base, then rebuild with Edits
git checkout HEAD -- <path>

# 5. Apply atomic change → stage → commit → repeat (N times)
#    Each commit = one visual / structural / behavioural concern.
#    Use Edit tool for text files to avoid whitespace drift.

# 6. VERIFY byte-identical rebuild before force-pushing
git diff origin/<branch>..HEAD
# ^^ MUST be empty. If not, you've lost content. STOP and investigate.

# 7. Force-with-lease: refuses if someone pushed since your fetch
git push --force-with-lease=<branch>:<EXPECTED_SHA> origin HEAD:<branch>

# 8. Cleanup
cd ..
git worktree remove --force "$TMPWT"
```

## Critical rules

- **Always `--force-with-lease`, never `--force`** on shared branches. Pass the *expected remote SHA* so it refuses if concurrent pushes landed.
- **Always verify `git diff origin/<branch>..HEAD` is empty** before pushing. A non-empty diff means you lost or added content vs. the original bulked commit — stop and rebuild.
- **One commit per concern.** If you're splitting a README rewrite, commits should be: hero image, section rename, bullet rewrite, showcase, callout — not "docs: update README".
- **Ask before force-pushing to main/master/release.** Even with a lease, protected branches warrant explicit confirmation. For feature branches, proceed.
- **Never edit a commit that someone else authored** unless you have explicit permission — you'll lose their attribution.

## Typical commit split commit messages

Keep them honest about what each commit actually does:
```
docs(readme): replace broken demo.gif with live dashboard hero
docs(readme): add usage analytics hero below the dashboard
docs(readme): rename section to "Terminal UI + CLI"
docs(readme): expand feature highlights with multi-provider + analytics
docs(readme): replace broken screenshot block with 6-panel showcase
docs(readme): add dedicated CLI section with command overview
```

Each message must stand alone — don't write "part 2 of README split" style.

## What this is NOT for

- Merged PRs: don't rewrite history after a merge unless everyone downstream agrees.
- Protected branches without lease authority: don't try.
- Code changes that concurrent sessions might depend on: let the bulk commit stand, fix going forward.

## Failure recovery

If `git diff origin/<branch>..HEAD` is non-empty:
1. Don't push. `git log` to see what's different.
2. Most likely you missed applying part of the final state, or introduced whitespace drift by rewriting via `cat <<EOF` instead of Edit.
3. Fix the drift, re-verify, then push.

If `--force-with-lease` rejects:
1. Someone pushed to the branch since your fetch. Your rewrite is now stale.
2. `git fetch origin`, rebase your atomic commits onto the new tip, re-verify diff, push again with the new lease SHA.

## Why this matters

Shared branches with force-push capability are one shell-slip away from catastrophic history loss. The throwaway-worktree + lease + verify-diff pattern makes the operation idempotent: if any step fails, nothing bad has happened yet. Only the final push is destructive, and the lease gates that.
