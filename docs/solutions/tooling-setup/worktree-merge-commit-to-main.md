---
id: lrn-worktree-merge-main-ba7c57
title: Worktree merge commit to main
category: tooling-setup
key_insight: When main is occupied by another worktree, merge on a temporary branch from origin/main, validate, then push HEAD to main.
scope: project
confidence: medium
learning_type: workflow-pattern
source_episodes:
  - mouse-first-tui-merge-2026-05-24
superseded_by: null
provenance:
  branch: feat/ui
  merge_commit: ba7c57c
---

## Problem

`main` may already be checked out in another worktree, and that worktree may be dirty or far behind. A direct checkout from the feature worktree fails because Git prevents one branch from being checked out by multiple worktrees.

## Solution

From the clean feature worktree, fetch remote refs and create a temporary integration branch from `origin/main`. Merge the feature branch there with `--no-ff`, run validation, sync issue metadata, then push the merge commit directly to `main` with `git push origin HEAD:main`.

After push, verify `origin/main` points at the merge commit, switch back to the feature branch, and delete the temporary integration branch.

## Anti-Pattern

Do not force checkout `main` over an existing worktree, and do not merge inside a dirty primary worktree. Avoid destructive worktree cleanup unless explicitly requested.

## Context

The mouse-first TUI work was merged while the primary `main` worktree had untracked project files and was behind remote. The safe path used a temporary branch from `origin/main`, validated the merge commit, pushed `HEAD:main`, and restored the feature worktree.
