---
name: ship-it
description: >
  End-to-end ship loop: atomic commits, PR, tiered review (lite|heavy), fix
  every finding, re-review until zero remain, then merge-commit. lite runs a
  single /review pass; heavy spins up a dynamic Workflow with diff-aware
  review personas plus a Codex cross-model peer. Use when Stevie says
  "/ship-it", "ship it", "ship this branch", or wants the full
  commit-review-fix-merge pipeline run autonomously.
---

# ship-it

Conductor skill. Runs in the main loop (interactive steps stay interactive).
The only expensive part, heavy review fan-out, runs as a dynamic Workflow.

```text
/commit ──▶ gh pr create ──▶ REVIEW (lite|heavy) ──▶ fix ALL ──▶ re-REVIEW
                                                        ▲            │
                                                        └── loop ────┘ until 0
                                                                     │
                                                     CI green ──▶ gh pr merge --merge
```

## Arguments

`/ship-it [lite|heavy] [pr-number]`

- `lite` (default): single-pass PR review. Cheap, fast.
- `heavy`: ce-style multi-persona fan-out + Codex cross-model peer.
- `pr-number`: skip commit/PR-create, start at the review loop on that PR.

## Model routing (hard rules)

| Role | Model |
|------|-------|
| Conductor, commit/PR/merge mechanics | session model |
| Review agents (both tiers) | **Opus, always** (Fable only if Stevie says so in chat) |
| Codex peer (heavy only) | Codex, **high reasoning effort** (put the literal `--effort high` in the peer prompt, see Step 3) |
| Applying mechanical fixes | Sonnet (`fast-worker`) for well-specified edits; main loop for judgment calls |

Review is the safety net: never route review to Sonnet or Haiku. lite's
single pass runs inline only when the session model is Opus-class (Opus or
Fable); on a Sonnet/Haiku session, spawn a `code-reviewer` agent with
`model: opus` instead of reviewing inline.

## Step 1: Commit

Invoke the `/commit` skill (Skill tool). It owns cleanup, atomic staging by
named paths, signed commits, push. Do not reimplement any of it here.
If the working tree is already clean and the branch is pushed, skip ahead.

## Step 2: Pull request

```bash
BRANCH=$(git branch --show-current)
git fetch origin
# Sync against the PR's OWN base, not a hardcoded main (stacked PRs exist).
BASE=$(gh pr view --json baseRefName --jq .baseRefName 2>/dev/null || echo main)
git merge "origin/$BASE"                         # MERGE, never rebase a pushed PR branch
gh pr list --head "$BRANCH" --state open --json number,url   # reuse existing PR if open
gh pr create --fill                              # otherwise create
```

Sync first: reviewing a branch that is far behind base wastes the whole loop
on conflicts at merge time. Resolve merge conflicts before the first review
pass. Roll new commits into an existing open PR for this branch, never open
a second one. PR body: summary, test evidence, no AI attribution.

## Step 3: Review (tier switch)

### lite
Invoke the `/review` skill on the PR number. One pass. Collect findings as a
severity-tagged list (P0 blocker, P1 major, P2 minor, P3 nit).

### heavy
Spin up a dynamic Workflow (Workflow tool). Template:

- **Persona selection is diff-aware.** Always run: `correctness`,
  `project-standards`. Add only when the diff touches the area:
  `security` (auth/input/secrets), `tests` (test files or runtime behavior),
  `performance` (hot paths, queries), `data-migration` (schema/persisted
  formats), `api-contract` (public interfaces, wire types).
- Each persona = one `agent()` on Opus (`model: 'opus'`), **schemaless**
  (returns markdown findings text; schemas on advisory agents trip
  StructuredOutput failures and abort the run).
- Codex peer: one `agent()` using `agentType: 'codex:codex-rescue'`,
  independent, not shown the personas' output. `agentType` alone does NOT set
  reasoning effort. `codex-rescue` is a thin forwarder to
  `codex-companion.mjs task`, which takes `--effort <none|minimal|low|medium
  |high|xhigh>`; it leaves effort unset unless the prompt explicitly asks,
  and it strips `--effort <value>` out of the task text as a routing control.
  So `codexPrompt` must contain the literal token `--effort high` (NOT
  `codex exec -c model_reasoning_effort=...`, which this agent never runs and
  would pass through as prose).
- Synthesis: conductor (not another agent) merges persona + Codex findings,
  dedupes by file:line, tags P0-P3, promotes confidence when two reviewers
  agree, discards unverifiable style noise.

Workflow skeleton (adapt persona list to the diff before launching):

```js
export const meta = {
  name: 'ship-it-heavy-review',
  description: 'Diff-aware persona fan-out + Codex peer for a PR',
  phases: [{ title: 'Review' }],
}
// args may arrive as a JSON string depending on how the host serialises the
// Workflow args input; guard or personas.map explodes on the first line.
const A = typeof args === 'string' ? JSON.parse(args) : args
const personas = A.personas // [{key, prompt}], chosen by conductor
const thunks = personas.map(p => () =>
  agent(p.prompt, { label: `review:${p.key}`, phase: 'Review', model: 'opus' }))
thunks.push(() => agent(A.codexPrompt,
  { label: 'review:codex-peer', phase: 'Review', agentType: 'codex:codex-rescue' }))
const out = await parallel(thunks)   // codex runs alongside personas, no barrier between them
return { personas: out.slice(0, personas.length).filter(Boolean),
         codex: out[personas.length] ?? null }   // codex may be null (skipped/dead); synthesis must tolerate that
```

Every persona prompt must include: repo path, PR diff scope (`gh pr diff N`),
"report findings only, file:line, one line each, severity P0-P3, no praise,
no scope creep".

## Step 4: Fix loop

Fix **every** finding, all severities, P3 nits included. No deferral, no
"follow-up issue" unless Stevie explicitly reclassifies a finding.

Per iteration:
1. Apply fixes. Mechanical, well-specified edits go to `fast-worker`
   (Sonnet); judgment calls stay in the main loop. Sonnet output gets a quick
   conductor sanity pass before commit (advisor rule).
2. Commit fixes via `/commit` (atomic, signed), push.
3. Re-review. **Round 1 only** runs the full tier (fan-out for heavy).
   Rounds 2+ run a single verify-pass instead: one Opus agent that (a)
   checks each prior finding is actually fixed and (b) reviews only the fix
   delta (`git diff <pre-fix-sha>..HEAD`). Full fan-out again only if a fix
   touched files outside the original review scope. This keeps the loop from
   multiplying heavy-tier cost by iteration count.
4. Zero findings: exit loop. Findings remain: iterate.

Guards:
- Max 5 iterations, then stop and escalate to Stevie with the survivors.
- Same finding survives 2 fix attempts: stop, surface it, do not silently
  switch strategy.
- A finding that is wrong (reviewer misread) may be rebutted once with
  evidence; if it reappears next round, escalate instead of re-rebutting.

## Step 5: Merge

Pre-merge, verify independently (do not collapse to one signal):

```bash
gh pr checks <N>                 # CI per-job
gh pr view <N> --json mergeable,mergeStateStatus,reviewDecision
```

- CI red from THIS PR's code: go back to Step 4.
- CI red that is pre-existing drift: prove it (git history + clean local
  test run on base), then present merge options honestly, do not force a
  cleanup commit into this PR.
- `mergeable` is `UNKNOWN`: GitHub is still recomputing after the last push.
  This is NOT green. Poll (a few seconds apart, ~5 tries) until it resolves;
  merge only on a literal `MERGEABLE`.
- `mergeable` is `CONFLICTING` (or `mergeStateStatus` is `DIRTY`): re-run the
  Step 2 sync against the PR's own base branch, resolve, push, re-check;
  never merge a conflicting PR.
- `reviewDecision` is `CHANGES_REQUESTED`: treat the requested changes as
  findings, back to Step 4.
- `reviewDecision` is `REVIEW_REQUIRED`: a human approval the conductor
  cannot self-grant. Stop and escalate to Stevie; do not merge past it.
  (Empty string means no review gate is configured, which IS green.)
- All gates green (CI + `mergeable == MERGEABLE` + review decision) and zero
  findings:

```bash
gh pr merge <N> --merge     # merge commit, NEVER squash
```

Honor the repo's landing contract before/after merge: if the project tracks
work in beads (AGENTS.md mandates it here), run `bd sync`, close or update
the beads this PR lands, and note the PR URL on them. Skip only in repos
without an issue-tracking contract.

Report: PR URL, merge commit SHA, iterations used, findings fixed per round.

## When NOT to use

- Review-only, no shipping intent: use `/review` or `/code-review` directly.
- Uncommitted WIP mid-feature: finish or use `/commit` alone.
- Repo without a GitHub remote: nothing to PR against, stop and say so.
