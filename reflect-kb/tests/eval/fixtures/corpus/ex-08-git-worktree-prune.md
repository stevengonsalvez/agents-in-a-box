---
name: ex-08-git-worktree-prune
title: "Stale git worktree refs break new worktree creation"
category: git
tags:
  - git
  - worktree
confidence: high
created: "2026-04-20"
key_insight: "After deleting a worktree dir manually, run git worktree prune before reusing the branch."
---
## Learning

'fatal: <branch> is already checked out' from a deleted dir means stale administrative files under .git/worktrees.

**How to apply:** After deleting a worktree dir manually, run git worktree prune before reusing the branch.
