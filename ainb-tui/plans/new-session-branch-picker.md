# Spec: New-Session Base Branch Picker

**Generated from:** interview (2026-06-03)
**Version:** 1.0

## Executive Summary

The new-session Configure screen shows `Branch: main → agents/xxxx` where the
source segment is display-only. This feature makes the source selectable: a
popup picker listing remote + local branches, with two actions per branch —
base a fresh `agents/xxx` branch off the pick, or check out the picked branch
itself in the worktree.

## Flow

```
┌────────────┐   ┌──────────────────────────┐   ┌─────────────────────────────┐
│ Configure  │──▶│ Branch row, src focused  │──▶│ Popup: branch list + filter │
│ screen     │   │ Enter opens picker       │   │ cached refs → async refresh │
└────────────┘   └──────────────────────────┘   └──────────────┬──────────────┘
                                                               │
                                  ┌────────────────────────────┼───────────────┐
                                  ▼                            ▼               │
                       Enter = base-off               c = checkout direct      │
                       worktree add -b agents/xx      worktree add <path> br   │
                       <path> <picked-ref>            (tracking branch)        │
                                  │                            │               │
                                  └────────────┬───────────────┘   Esc = close ┘
                                               ▼
                                      session created
```

## Decisions Made (interview, all axes)

| Axis | Decision | Rationale |
|------|----------|-----------|
| Semantics | Dual action: Enter = new `agents/xxx` based off pick; `c` = checkout picked branch itself | Covers fresh-work AND continue-existing-PR cases |
| Scope | Both local repos and star-launched remote repos | One consistent UX |
| UI placement | Popup on Branch row source segment, fuzzy filter (like repo picker) | No new Configure row; familiar pattern |
| List contents | Remote (`origin/*`) + local branches, sectioned, default-first, dedup where local tracks remote | Literal ask = remote; local adds offline value |
| Collision | Rows show `⚠ in use` (branch checked out in another worktree); `c` on such a row = inline error, stays in picker | Git hard-blocks double checkout; fail visible, no surprise fallback |
| Fetch timing | Open instantly from cached `refs/remotes/*`; background fetch refreshes list in place with spinner | Offline-safe, no popup hang on slow network |

## Scope

### In Scope
- Popup branch picker in Configure screen (Branch row source segment)
- Base-off semantics: `git worktree add -b <agents/xxx> <path> <picked-ref>`
- Checkout-direct semantics:
  - remote pick, no local branch: `git worktree add --track -b <name> <path> origin/<name>`
  - local branch exists, not checked out: `git worktree add <path> <name>`
- In-use detection from existing worktrees (reuse `existing_branches` machinery)
- Cached-first listing + async refresh for local repos (`refs/remotes/*` on disk)
  and remote repos (`list_remote_branches()` already exists, remote_repo_manager.rs:82)
- Branch row display updates:
  - base-off: `origin/feature-x → agents/ab12cd34`
  - checkout: `feature-x (checkout)` — no generated name
- Worktree-name inline edit unchanged for base-off mode

### Out of Scope
- Attaching to an existing session that already has the branch (deferred)
- Branch creation from arbitrary SHAs/tags
- Multi-remote support (origin only)

## Technical Requirements

### Touched components

| Component | File | Change |
|-----------|------|--------|
| Configure state | `crates/ainb-core/src/components/new_session/configure.rs` | `BranchPickerState`, `BaseSelection { ref_name, mode }`, render popup, key handling |
| Events wiring | `crates/ainb-core/src/app/events.rs` | populate cached entries on popup open; async refresh message; thread `BaseSelection` into session creation |
| Local git | `crates/ainb-core/src/git/worktree_manager.rs` | worktree-add with explicit start point; checkout-direct variant |
| Remote git | `crates/ainb-core/src/git/remote_repo_manager.rs` | worktree off picked `origin/<branch>` (generalise `create_worktree_off_remote_default`) |
| Branch listing | git layer | local: `refs/remotes/*` + local heads via git2 (cached, offline); refresh via fetch/ls-remote |

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| Picked branch checked out in another worktree, `c` pressed | Inline error in popup, no action |
| Picked branch checked out elsewhere, Enter pressed | Fine — base-off creates new branch |
| Checkout-direct of remote branch with no local counterpart | Create local tracking branch in worktree |
| Checkout-direct where local branch exists and is behind origin | Check out local branch as-is (no auto-ff) — user's branch state preserved |
| Refresh fails (offline) | Keep cached list, show non-blocking warn, popup stays usable |
| No remote configured (local-only repo) | Local section only, no spinner |
| Filter matches nothing | Empty-state line, Esc still closes |
| Default branch | Always sorted first, marked `(default)` |

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Async refresh races popup close | Low | Med | Guard refresh result application on popup-open + repo identity |
| Checkout-direct branch-name collisions with stale local branches | Med | Low | Pre-check local branch state; in-use markers from worktree list |
| Big repos: ls-remote slow | Low | Med | Cached-first design; spinner only on the refresh, never blocking |

## Implementation Notes — Priority Order

1. Git layer: branch enumeration (cached + refresh) and worktree-add variants
2. Configure state + popup render + key handling
3. Events wiring: open/refresh/selection → session creation (local + remote paths)
4. Behavioural tests (git layer in temp repos; picker state transitions)
