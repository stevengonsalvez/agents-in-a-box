# V4 Pattern B (handover) — cycle 6 — FAILED

## Verdict
HARD FAIL. The core "branch is the baton" assertion is false: V3's commits are
NOT reachable from V4's dispatched branch. Two chained hangar bugs found.

## Setup (verified before V4 ran)
- V3 (HGR-3, task `01KY4MKWWFDFWXR5RK8PXQYMPQ`) completed `done` at
  2026-07-22T10:11:06Z, branch `ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ`, 2 commits
  (`a77ea901` CLI reference regen, `c9d52b1b` keybindings doc fix), based on
  main tip `fa3f83a3`. Confirmed via `git log`/`git worktree list` in the
  shared clone — this branch legitimately existed, locally, before V4 was
  created.

## V4 wizard input (screenshots: v4-05..v4-14)
- Title: "Review the docs fix c6"
- Brief: diff/verify/REVIEW.md/commit/no-push instructions (verbatim in
  `v4-sqlite-description.txt`)
- Linked: https://github.com/stevengonsalvez/agents-in-a-box/issues/429
- Repo: agents-in-a-box (same clone)
- **Source: `ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ`** — confirmed correctly typed
  and STABLE in the wizard field before Enter (`v4-14-preSubmit-full.txt`)
- Agent: reviewer (claude-opus-4-6)
- Created issue `01KY4NE0A6GE44Q2QAF5KX1HXT` (HGR-4), task
  `01KY4NE0CGYP8W9VCCS4ZSKDZN`.

## Bug A (root cause): source_branch override dropped at dispatch
- `agent_task_queue.source_branch` for task `01KY4NE0CGYP8W9VCCS4ZSKDZN` is
  NULL/empty (`v4-sqlite-task.log`), despite the wizard sending a non-empty
  `Source` through `hangar/issue_update` + `hangar/issue_run` (both carry
  `source_branch` per `fire_issue_dispatch` in
  `crates/ainb-plugin-hangar/src/plugin.rs:2685-2748`).
- Consequence: `provision_worktree()` in
  `crates/ainb-hangar-daemon/src/workdir_provision.rs` took the `None` branch
  (`git worktree add <path> -b <branch>` with NO start-point → HEAD/main),
  not the `Some(src)` branch that would `resolve_start_point` against
  `ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ`.
- Direct proof: the new branch `ainb/01KY4NE0CGYP8W9VCCS4ZSKDZN` in the shared
  clone points at `fa3f83a3` (main tip) — the SAME commit as `main`, zero V3
  commits present.
  ```
  git merge-base --is-ancestor ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ \
      ainb/01KY4NE0CGYP8W9VCCS4ZSKDZN   # exit 1 — NOT an ancestor
  ```
- Suspects for where the value is actually lost (client→daemon RPC path was
  read end-to-end and looks correct on both sides at the source level; the
  drop must be either a runtime race between `issue_update`/`issue_run` and
  `run_card`'s read of `CardParityRepo::get_issue_branches`, or something in
  the RPC dispatch/queueing not caught by static reading):
  - `crates/ainb-plugin-hangar/src/plugin.rs` `fire_issue_dispatch` (~2685)
  - `crates/ainb-hangar-daemon/src/rpc/mod.rs` `handle_issue_run` (~2840) and
    `run_card` (~3082, esp. the `source_branch_override.map(...).or(card_source)`
    at ~3150)
  - `crates/ainb-hangar-store/src/repo/card_parity.rs`
    `set_task_source_branch_in_tx` (write) / `get_issue_branches` (read)

## Bug B (masking / silent-loss consequence)
The reviewer agent, working in the wrongly-based worktree, discovered via
`git log/diff main..ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ` that the branch it was
told to review had the expected diff (readable from the shared clone by ref,
regardless of its own checkout), then self-corrected with
`git checkout ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ` INSIDE its own worktree
directory, and committed its wording-cleanup + `REVIEW.md` there — i.e. onto
**V3's original branch**, not the branch hangar allocated for this task
(`ainb/01KY4NE0CGYP8W9VCCS4ZSKDZN`).

Result:
- `ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ` now legitimately has 4 commits including
  a real `REVIEW.md` (verified: `git show ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ:REVIEW.md`)
  — the review WAS done correctly, content-wise.
- But hangar's own bookkeeping never saw it: no `"run branch recorded"` log
  line fired for this task (compare V3's daemon log, which has one), and
  `"run workdir torn down","outcome":"Removed"` fired instead of `KeptDirty`
  — because relative to whatever branch the worktree ended up on, the tree
  was clean. `agent_task_queue.branch` is empty for this task.
- The task nonetheless finalized `status=done`, `result.exit_code=0`, with a
  confident, well-formatted "Verdict: PASS" summary — a textbook silent
  success-looking failure. Trusting `done` alone would have missed this
  entirely (fail-closed doctrine justified).

## Artifact-level proof (per spec)
- New worktree branched FROM V3's branch: **FALSE** (branched from main).
- REVIEW.md present in a commit: **TRUE, but on the wrong branch**
  (`ainb/01KY4MKWWFDFWXR5RK8PXQYMPQ`, not `ainb/01KY4NE0CGYP8W9VCCS4ZSKDZN`).
- Task terminal state: `done` (misleading — see Bug B).

## Evidence files (this directory)
- v4-01..v4-15: tmux pane captures of the full wizard drive
- v4-sqlite-issue.log, v4-sqlite-task.log, v4-sqlite-description.txt
- v4-task-result-raw.json: full session transcript (200 stream-json lines)
- v3r-*, v3-sqlite-*: V3 setup/completion evidence (source branch provenance)

## Recommendation
Route to fix loop as a single opus-triaged bug (both A and B are one causal
chain — fixing A likely also prevents B, since the agent would never need to
self-correct with a branch checkout if provisioned correctly). Suggested
branch name: `fix/hangar-e2e-6-source-branch-override-dropped-at-dispatch`.
A regression test should assert: `issue_run` with `source_branch` set →
`agent_task_queue.source_branch` persisted non-null AND the provisioned
worktree's HEAD is an ancestor-descendant of the given source ref, not of
plain HEAD.
