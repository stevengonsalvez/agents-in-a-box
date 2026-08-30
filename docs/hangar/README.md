# Hangar

ainb's managed-agents control plane — where the box opens and agents launch.

Hangar took ainb from a local CLI/TUI to a managed-agents platform: assign tasks to agents,
track lifecycle, compound skills, surface progress in real time.

**Status: shipped.** All 35 `agents-in-a-box-e38` child beads are closed. The live surface is
39 RPC methods, 13 TUI screens, 16 CLI noun-groups and 23 migrations
(see [`proofs/multica-comparison.md`](proofs/multica-comparison.md) for the reconciliation).

Most of this directory is the build record: the plan, the phase docs and the research that
produced it. Read those as history. The pages that describe what Hangar *does today* are
[`architecture.md`](architecture.md), [`tui-keybindings.md`](tui-keybindings.md) and
[`pull-pipeline.md`](pull-pipeline.md).

> Two numbers below are pre-e38 estimates, kept because they record what was believed at the
> time: the "~45% already exists" reuse figure and the "~16 weeks across 8 phases" build
> estimate. `architecture.md`'s "17 RPC methods / 35 features" is the same vintage.

## Contents

```
docs/hangar/
├── README.md                ← you are here
├── pull-pipeline.md         ← role-gated pull pipeline (shipped; how squads execute)
├── hangar-plan.html         ← rich HTML explainer (open in browser)
├── diagrams/                ← 6 SVG architecture + flow diagrams
│   ├── 01-multica-arch.svg
│   ├── 02-task-lifecycle.svg
│   ├── 03-user-journey.svg
│   ├── 04-ainb-current.svg
│   ├── 05-ainb-target.svg
│   └── 06-roadmap.svg
└── research/                ← 6 deep-dive reports on Multica (3,220 lines)
    ├── 01-codebase-archaeology.md
    ├── 02-community-feedback.md
    ├── 03-architecture-review.md
    ├── 04-distinguished-engineer-critique.md
    ├── 05-ux-design-analysis.md
    └── 06-ainb-capability-inventory.md
```

## TL;DR

- **Subject of research:** [Multica](https://github.com/multica-ai/multica) — open-source managed-agents platform, 31k stars in ~4 months.
- **What we're building:** Hangar — ainb's version of the same control plane shape, reusing existing ainb primitives (plugin host v2, reflect-kb, swarm, worktrees, beads).
- **Reuse estimate:** ~45% of Hangar already exists inside ainb.
- **Build estimate:** ~16 weeks across 8 phases (P0 → P7), see `diagrams/06-roadmap.svg`.
- **Three open decisions before P0:** see §10 of `hangar-plan.html`.

## How to read

0. Using the shipped TUI? `tui-keybindings.md` documents every screen's keys,
   including the [task-failure taxonomy](tui-keybindings.md#task-failures)
   (what each `failure_reason` means and whether a retry helps).
1. `hangar-plan.html` is the original synthesized proposal, with inline diagrams. Historical.
2. Drill into individual reports under `research/` for evidence and citations.
3. The diagrams in `diagrams/` are also usable standalone.

## Provenance

Research produced 2026-05-22 by 6 parallel sub-agents:
- `code-archaeologist` — codebase structure
- `web-search-researcher` — community + market reception
- `architecture-reviewer` — staff-architect-grade design review
- `distinguished-engineer` — risk and bet analysis
- `ui-designer` — UX teardown
- `project-analyst` — ainb capability inventory + gap mapping

Diagrams generated via `/fireworks-tech-graph`. Final HTML assembled via `/explain-to-me` with the `16-implementation-plan` template + Claude theme overlay.

<!-- skip-path probe, branch is disposable -->
