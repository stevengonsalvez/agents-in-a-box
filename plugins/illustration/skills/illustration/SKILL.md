---
name: illustration
description: Generate mascot-driven explanatory illustrations. Use when the user wants illustrations/diagrams/explainer images/shot lists/hero art for an article, post, README, deck, or brand, and wants them in a specific mascot's style. Router for the illustration family — picks a character then hands off to its sub-skill. Mascots: illustration:sporthead-alex (ShotClubhouse Sport Head — sketchnote visual notes) and illustration:popa (pink blob with a notepad — crude hand-drawn pastel).
user-invocable: true
---

# Illustration family

Generate explanatory and brand illustrations in a chosen mascot's voice and style. All mascots share one workflow engine; each brings its own look, character, and likeness references.

## Pick a mascot

- **`illustration:sporthead-alex`** — Alex, the trademarked ShotClubhouse "Sport Head" (soccer-ball head, black cap, no glasses). Sketchnote style: hand-drawn visual notes (banners, framed ideas, arrows, icons) on white with an optional gold accent. Best for: ShotClubhouse brand/marketing, sport, plus general explainers/how-it-works.
- **`illustration:popa`** — Popa, a cute pink blob with a green sprout and a notepad. Crude hand-drawn on soft pastel, deadpan-absurd humor. Best for: dev/product/system explainers, how-it-works, architecture, friendly social.

If the user named a mascot, invoke that sub-skill directly. If not, infer from context (sport/ShotClubhouse/brand → `alex`; dev/system/funny → `popa`) or ask.

## Shared engine

Every mascot follows `${CLAUDE_PLUGIN_ROOT}/references/workflow-engine.md` (digest → shot list → per-image generation → QA → deliver) and `${CLAUDE_PLUGIN_ROOT}/references/composition-patterns.md` (structure types, format presets, the likeness rule). The sub-skill supplies the visual DNA, IP anchors, and reference images.

## Adding a new mascot

Create `skills/<mascot>/` with `SKILL.md` (name `illustration:<mascot>`), `references/<mascot>-ip.md`, `references/<mascot>-style-dna.md`, `references/prompt-template.md`, `references/qa-checklist.md`, and `assets/<mascot>-ref/` (likeness images) + `assets/examples/` (calibration). Register nothing else — the plugin exposes it automatically.
