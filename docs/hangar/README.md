# Hangar

ainb's managed-agents control plane — where the box opens and agents launch.

Hangar is the proposed evolution of ainb from a local CLI/TUI into a managed-agents platform: assign tasks to agents, track lifecycle, compound skills, surface progress in real time. It's the ainb-flavored answer to what Multica does for Claude Code.

## Contents

```
docs/hangar/
├── README.md                ← you are here
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
1. Start with `hangar-plan.html` — the synthesized proposal with inline diagrams.
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
