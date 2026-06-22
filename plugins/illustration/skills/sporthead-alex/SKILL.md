---
name: illustration:sporthead-alex
description: Generate sketchnote illustrations starring Alex, the trademarked ShotClubhouse "Sport Head" mascot (soccer-ball head, black cap, no glasses). One-page hand-drawn visual notes — banners, framed ideas, arrows, icons, checklists — hosted by an Alex doodle. Use for ShotClubhouse brand/marketing and for explainers/how-it-works/architecture. Black ink on white with an optional single gold accent (or pure mono).
user-invocable: true
---

# illustration:sporthead-alex — ShotClubhouse Sport Head (sketchnote)

Generate **sketchnotes** — one-page hand-drawn visual summaries — hosted by **Alex**, ShotClubhouse's trademarked Sport Head character. Each note captures an idea with a hand-lettered banner, framed points, arrows, icons and checklists, with a small Alex doodle walking you through it. *Take your shot, own it, make impact.*

## Read first

1. `${CLAUDE_PLUGIN_ROOT}/references/workflow-engine.md` — the shared digest → shot list → generate → QA → deliver process.
2. `${CLAUDE_PLUGIN_ROOT}/references/composition-patterns.md` — structure types, format presets, the likeness rule.
3. `references/alex-ip.md` — Alex's anchors, personality, forbidden moves.
4. `references/alex-style-dna.md` — the sketchnote visual law (gold vs mono accent).
5. `references/prompt-template.md` — the generation prompt template.
6. `references/qa-checklist.md` — pass/fail and iteration.
7. `assets/alex-ref/` — Alex's source art, passed as the likeness reference on every generation (mandatory).
8. `assets/examples/` — calibration only (2×2: brand/dev × gold/mono); never copy these compositions.

## Core positioning

An Alex image is a lively hand-drawn page that captures one idea visually: a banner title, a few framed points connected by arrows, bullets and spot icons, and Alex as the recurring doodle host (soccer-ball head, cap, no glasses). Black ink on clean white, with at most a single gold accent (or pure mono). Not a comic, not a pastel scene — a sketchnote.

## Workflow (engine + Alex specifics)

Follow the five shared steps. Alex specifics:

- **Likeness is mandatory:** pass `assets/alex-ref/alex-source.png` every time with "this is Alex; use the reference ONLY for his design; draw him as a small sketchnote doodle; plain ball head, no eyewear; do not letter instruction words into the image."
- **No glasses, ever** — and never letter the words "no glasses" into the page.
- **Accent per image:** GOLD (single gold highlight on banners + ball, default for brand) or MONO (pure black & white, best for docs/notes).
- **Keep it a note:** banner title, framed ideas, arrows, bullets, icons, short hand-lettered labels only — no paragraphs.
- **On-brand content:** Sport Head ID, four pillars (club/coach/athlete/parent), street-to-stadium, "TAKE YOUR SHOT".

## Output style

Pre-generation strategy: short and punchy. Delivery report: how many, what each is for, save paths, which are solid vs optional. Let the notes carry it.
