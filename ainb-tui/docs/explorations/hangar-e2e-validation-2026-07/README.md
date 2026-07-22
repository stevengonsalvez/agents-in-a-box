# Hangar E2E Validation Campaign (2026-07-20 → 2026-07-22)

## Mission

Prove, with real tmux-driven runs against a real daemon and a real `claude`
CLI (no mocks, no exit-code-only assertions), that Hangar's core workflows
actually work end to end:

1. A genuine GitHub-issue-worthy docs drift, fixed by Hangar itself.
2. Pattern A (solo agent completes an issue).
3. Pattern B (sequential handover — one agent's branch becomes the next
   agent's source branch).
4. Pattern C (squad fan-out — multiple agents work issues in parallel;
   failures get filed as GitHub issues rather than silently patched
   in-loop).
5. The resulting docs fix lands as its own reviewed, CI-green, merged PR.
6. Every Hangar bug found along the way gets its own small PR
   (implement → validate → CI → merge), and the daemon is rebuilt and
   re-verified after each one.

Eleven verification cycles (`cycle-1` .. `cycle-11`) ran across two days.
The first ten each surfaced a real defect (daemon wedges, sandbox
mis-detection, lifecycle FSM gaps, wizard input bugs). Cycle 4 was the
first fully green run and is the one referenced throughout this archive.

## Final audit verdict (verbatim)

This is the closing 6-criteria audit table produced by the final workflow
run (`wf_9ef4fde0-60f`, 2026-07-22). Full JSON: [`final-audit-6-criteria.json`](final-audit-6-criteria.json).

| # | Criterion | Proven | Evidence |
|---|-----------|--------|----------|
| 1 | Real GH issue describing genuine docs drift | ✅ | Issue #453 (closed) traces each claim to merged PRs #431 and #436; separate CLI-reference drift handled by PR #451. |
| 2 | Hangar drove it end-to-end, tmux-verified with side-effect assertions | ✅ | cycle-4 pane dumps (c4-07..c4-13, wizard → post-submit) paired with DB side-effects: `hangar.db` issue rows `state=done`, `run_history` outcome=success, `c4-16-poll.log` shows `running`→`done` via sqlite. |
| 3 | Pattern A + Pattern B both green (HARD gates) | ✅ | Pattern A issue `01KY55VF8G81` done, `source_branch=main`. Pattern B issue `01KY56ZCKCGZ` done, `source_branch` = A's output branch. `git merge-base --is-ancestor` confirms the branch handover; A's branch carries the actual docs commit. |
| 4 | Pattern C (squad fan-out) verified; failures filed as issues, not patched in-loop (SOFT gate) | ✅ | Squad issues present and done/open in DB. The one squad failure (Boards `q:squad` hotkey unreachable) was filed as issue #450 (open), not fixed inline. |
| 5 | Docs-fix branch → PR opened, gate-reviewed, meaningful CI green, merged to main | ✅ | PR #454 merged (`merge_commit 14469460`, confirmed on `origin/main`), docs-only, `Closes #453`. Meaningful CI green (CLI-reference-freshness, Test ubuntu+macos, ainb-hooks); only pre-existing-red checks (Rustfmt, ubuntu hangar-e2e) stayed red. Honest nuance: `ci.yml` is path-filtered to `ainb-tui/**` so freshness didn't literally re-run on the docs-only PR — it's proven green on the immediately preceding main state, which a docs-only edit cannot regress. |
| 6 | Every hangar bug fixed via its own small PR, rebuilt, re-verified | ✅ | 18 merged `fix/hangar-e2e-*`+ related PRs, each its own branch and conventional commit. Re-verified across all 11 cycles; cycle-4 final green. |

## The 18-PR fix campaign

All merged into `main` on `stevengonsalvez/agents-in-a-box`. One-line root
cause per PR:

| PR | Title | Root cause (one line) |
|----|-------|------------------------|
| [#427](https://github.com/stevengonsalvez/agents-in-a-box/pull/427) | capture a multi-line Brief when creating an issue | Wizard only captured a single Brief line; multi-line briefs got truncated. |
| [#428](https://github.com/stevengonsalvez/agents-in-a-box/pull/428) | link an issue to an upstream GitHub/Jira ref (+ dispatch guard) | No way to attach an external tracker ref, and dispatch could fire before one was set. |
| [#430](https://github.com/stevengonsalvez/agents-in-a-box/pull/430) | clone remote-only repo pick in Issues-wizard create/run path | Picking a repo that existed only on the remote (no local clone) silently failed to clone before run. |
| [#431](https://github.com/stevengonsalvez/agents-in-a-box/pull/431) | terminalize pre-run setup faults instead of looping dispatched | A setup fault before the run started left the task stuck retry-looping in `dispatched` instead of failing terminally. |
| [#433](https://github.com/stevengonsalvez/agents-in-a-box/pull/433) | target a named workspace agent from the Issues create wizard (V3-F3) | Wizard couldn't target a specific named agent, only "any agent". |
| [#434](https://github.com/stevengonsalvez/agents-in-a-box/pull/434) | make Issues filter chips (All/Members/Agents/Mine) reachable | Filter chips existed visually but weren't keyboard/mouse reachable. |
| [#435](https://github.com/stevengonsalvez/agents-in-a-box/pull/435) | bound daemon claude credential read so dispatch can't zombie at `running` | An unbounded synchronous macOS Keychain read on the async worker could wedge the daemon forever mid-dispatch (the "zombie-dispatch" defect — see [`daemon-wedge-sample-trimmed.txt`](daemon-wedge-sample-trimmed.txt)). |
| [#436](https://github.com/stevengonsalvez/agents-in-a-box/pull/436) | wedge-proof the running→spawn phase; terminalize on setup timeout | Same class of wedge risk in the running→spawn transition; added a hard setup timeout that terminalizes instead of hanging. |
| [#437](https://github.com/stevengonsalvez/agents-in-a-box/pull/437) | persist runner stderr tail into result on run failure | Run failures produced no diagnostic — stderr was discarded instead of persisted to the result row. |
| [#438](https://github.com/stevengonsalvez/agents-in-a-box/pull/438) | assigning an agent re-dispatches the issue (in-product recovery from `agent_error`) | No in-product way to recover a task stuck in `agent_error`; reassigning an agent now re-dispatches. |
| [#439](https://github.com/stevengonsalvez/agents-in-a-box/pull/439) | paint opaque background under agent-picker modal | Agent-picker modal had a transparent background, making underlying UI bleed through. |
| [#441](https://github.com/stevengonsalvez/agents-in-a-box/pull/441) | default headless OS sandbox OFF on macOS | Headless OS sandbox defaulted ON on macOS where it isn't needed/supported, killing headless `claude` dispatch. |
| [#445](https://github.com/stevengonsalvez/agents-in-a-box/pull/445) | stale sibling daemon binary defeats hangar fixes | A stale daemon binary sitting alongside the freshly built one got picked up at runtime, silently reverting every fix above. |
| [#447](https://github.com/stevengonsalvez/agents-in-a-box/pull/447) | advance issue lifecycle on plain task FSM | Plain (non-issue) task completions weren't advancing the linked issue's lifecycle state. |
| [#449](https://github.com/stevengonsalvez/agents-in-a-box/pull/449) | persist issue_update source/target branch before auto-dispatch | `issue_update` could auto-dispatch before the source/target branch fields were persisted, racing the dispatch against its own metadata. |
| [#451](https://github.com/stevengonsalvez/agents-in-a-box/pull/451) | fix hangar CLI reference + journey docs drift | CLI reference and journey docs had drifted from the actual binary/behavior. |
| [#452](https://github.com/stevengonsalvez/agents-in-a-box/pull/452) | rebase wizard Brief leading-newline guard onto main + unflake macOS render budget | Enter on an empty wizard Brief seeded a leading `\n`, displacing the leading `/name` skill line and breaking headless `claude -p`; also unflaked a macOS render-budget test. (Supersedes PR #448 — see below.) |
| [#454](https://github.com/stevengonsalvez/agents-in-a-box/pull/454) | fix hangar CLI reference + journey docs drift | The actual docs-fix PR produced *by* the Pattern A/B validation run itself, closing issue #453. |

## Pattern outcomes

- **Pattern A (solo)** — green. Issue `01KY55VF8G81` completed solo,
  `source_branch=main`, `run_history` outcome=success.
- **Pattern B (sequential handover)** — green. Issue `01KY56ZCKCGZ`
  consumed Pattern A's output branch as its own `source_branch`;
  `git merge-base --is-ancestor` confirms the baton was actually passed,
  not just referenced.
- **Pattern C (squad fan-out)** — green with one filed (not silently
  patched) failure: the Boards `q:squad` hotkey was unreachable because
  the global key router steals `q` for quit before `AssignSquad` sees it.
  Filed as [issue #450](https://github.com/stevengonsalvez/agents-in-a-box/issues/450)
  (still open) rather than being fixed inline — which is the desired
  behavior for the SOFT gate.

## Open follow-ups

- **[#450](https://github.com/stevengonsalvez/agents-in-a-box/issues/450)** (open) — squad Boards `q:squad` hotkey unreachable; global router steals `q` as quit before `AssignSquad`.
- **Work-product gate** — the Opus PR-gate review that ran during the
  campaign was an internal workflow step (captured in the v4 transcripts
  in this archive) and was never posted as a visible review on the PR
  itself — only the CodeRabbit/Gemini bot reviews are externally visible.
  Worth wiring the internal gate's verdict into an actual PR review or
  comment so it's auditable from GitHub alone.
- **Daemon self-auth credential pattern** — the root cause behind #435/#436
  (a synchronous macOS Keychain call running unbounded on an async worker)
  is a pattern worth grepping for elsewhere in the daemon: any other
  blocking OS-credential call not wrapped in `spawn_blocking` with a
  timeout is a latent wedge.

## Merge-audit leftovers (as of 2026-07-22, not part of this archive's scope — reported to team lead separately)

Six PRs remain genuinely OPEN and unmerged (verified by diffing their
changed files against `main` — none of their content has landed elsewhere):
[#432](https://github.com/stevengonsalvez/agents-in-a-box/pull/432) (zombie-dispatch SetupError finalize),
[#440](https://github.com/stevengonsalvez/agents-in-a-box/pull/440) (resolve provider path to absolute),
[#442](https://github.com/stevengonsalvez/agents-in-a-box/pull/442) (log OS-sandbox posture at startup),
[#443](https://github.com/stevengonsalvez/agents-in-a-box/pull/443) (self-diagnosing agent_error path),
[#444](https://github.com/stevengonsalvez/agents-in-a-box/pull/444) (manual retry re-queues terminal agent_error task),
[#446](https://github.com/stevengonsalvez/agents-in-a-box/pull/446) (persist diagnostic on zero-output run failures).

One PR, [#448](https://github.com/stevengonsalvez/agents-in-a-box/pull/448)
(wizard Brief leading-newline guard), is **stale/superseded**: its exact
fix to `issue_list.rs` was re-landed via merged PR #452, and `main`
already carries the guard verbatim. #448 should be closed as superseded,
not merged.

## Artifact index

| File | What it is |
|------|------------|
| [`final-audit-6-criteria.json`](final-audit-6-criteria.json) | The closing 6-criteria audit verdict, verbatim, from the final workflow run. |
| [`cycle6-v4-FAILURE-ANALYSIS.md`](cycle6-v4-FAILURE-ANALYSIS.md) | Root-cause writeup from cycle 6's v4 failure pass. |
| [`cycle2-v3_result.json`](cycle2-v3_result.json) | Structured V3-cycle failure evidence bundle, cycle 2. |
| [`cycle3-v4_final_result.json`](cycle3-v4_final_result.json) | Structured V4-cycle final result bundle, cycle 3. |
| [`daemon-wedge-sample-trimmed.txt`](daemon-wedge-sample-trimmed.txt) | The macOS `sample` process dump that is the smoking gun for the zombie-dispatch defect (PR #435): the daemon's main thread plus the one `tokio-runtime-worker` blocked in `security_framework::passwords::get_generic_password`. Trimmed from 208KB/1233 lines (18 thread stacks) down to the two load-bearing threads; trim note is inline in the file. |
| [`cycle4-wizard-journey/`](cycle4-wizard-journey/) | The representative green wizard-journey pane-dump sequence, cycle 4, steps c4-07 (new task card) through c4-13 (post-submit) — new-task-card → title → brief → repo-dropdown → repo-selected → agent-at → post-submit. |
| `run-wf_*-journal.jsonl` | Every workflow run's raw journal (structured verdicts, one per audit criterion, across all runs of this campaign), copied verbatim. `run-wf_9ef4fde0-60f-journal.jsonl` is the final green run; the others are earlier cycles that found real defects. |

## Not preserved

Everything else under
`/Users/stevengonsalvez/.claude/jobs/cc83c6a4/tmp/hangar-e2e/` — the
per-cycle `home-c*/` sqlite databases and full working directories, the
`wt/`, `wt-baseline/`, `repro-877m/`, `isohome/`, `scratch-freshness/`
scratch trees, `full_test_run.log` / `main_ubuntu_hangar_e2e.log` /
`store_proto_test_run.log` (raw CI-replica logs), and the hundreds of
intermediate pane dumps from cycles 1, 2, 3, 5–11 — is genuinely ephemeral
(regenerable working state, not evidence) and was intentionally left out
to keep this archive under 1MB. All of it still lives at that path on
this machine if deeper forensics are ever needed, but it is session-scoped
and will not survive a job cleanup.
