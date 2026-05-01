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

## Recipe: Swap a squash-merge for a true merge commit (post-merge correction)

**Scenario:** A PR was already squash-merged but it should have been a merge commit (e.g., the project prefers to preserve atomic per-concern commits in main's history). The squash commit sits on the default branch.

**Trick:** the squash commit's tree IS the merged result. Reuse it. Build a new commit object that points to the same tree but has two parents (pre-squash main tip + feature branch tip). Force-push to swap. No actual re-merge happens, so there's no chance of conflicts and no working-tree changes.

```bash
# 1. Identify SHAs
SQUASH=$(git rev-parse origin/main)              # current main tip (the squash)
PRE_SQUASH=$(git rev-parse $SQUASH^)             # main before the squash
FEATURE=$(git rev-parse <feature-branch>)        # feature branch tip (must still exist locally or on remote)
TREE=$(git rev-parse $SQUASH^{tree})             # merged tree (already correct)

# 2. Build the merge commit object via plumbing — no working tree touched
MERGE=$(git commit-tree $TREE -p $PRE_SQUASH -p $FEATURE \
  -m "Merge pull request #N from <branch>" \
  -m "<body>")

# 3. Sanity check — trees MUST match. If they don't, abort.
[ "$(git rev-parse $SQUASH^{tree})" = "$(git rev-parse $MERGE^{tree})" ] \
  || { echo "TREE MISMATCH — abort"; exit 1; }

# 4. Force-push with lease scoped to the squash SHA
#    Refuses if anyone else has moved main since the squash.
git push --force-with-lease=main:$SQUASH origin $MERGE:refs/heads/main

# 5. Verify
git fetch origin main
git log --pretty=format:"%h %p %s" -1 origin/main   # should show two parents
```

**Why this works:** `git commit-tree` is plumbing that creates a commit object pointing at any tree with any parents, no merge driver involved. Since the squash already produced the correct merged tree, you're just relabeling that tree as a merge commit with the right parentage. The byte-identical-tree assertion is the safety check that proves you haven't lost or added content.

**When to use:**
- Project convention is merge-commits, but a squash slipped through.
- You want to preserve the feature branch's atomic commits as parents reachable from main.
- The squashed PR landed within the last few commits and nothing else has been built on top.

**When NOT to use:**
- Other commits have landed on main on top of the squash (`origin/main^` is no longer the pre-squash SHA). The lease will reject anyway, but the operation is no longer simply reversible — you'd need to rebuild the chain.
- The feature branch was deleted from remote *and* local, with no reflog. You can't reconstitute a merge without parent2.
- The squash combined changes from multiple feature branches (rare, but the recipe assumes 1:1).

**Branch protection:** Default-branch force-push usually requires admin override. Coordinate or use `gh api` with admin token if needed. Per `feedback_force_push_docs`, in this repo history-surgery on main is acceptable when the trees match and the lease holds.
