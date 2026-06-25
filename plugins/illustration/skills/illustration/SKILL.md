---
name: illustration
description: Generate intricate hand-drawn sketchnote illustrations starring Popa (a pink blob with a green sprout and a notepad). Use when the user wants illustrations/diagrams/explainer images/shot lists for an article, post, README, deck, system, or idea. Alias of illustration:popa.
user-invocable: true
---

# Illustration

Generate explanatory illustrations starring **Popa** as intricate hand-drawn sketchnotes. This bare skill is an alias for **`illustration:popa`** — hand it straight there.

- **`illustration:popa`** — Popa, a cute pink blob with a green sprout and a notepad. Intricate hand-drawn sketchnotes: a banner, many linked boxes/notes/arrows/icons on a soft pastel scene — busy but legible. Best for dev/product/system explainers, how-it-works, architecture, walkthroughs, friendly social.

## Engine

Popa follows `${CLAUDE_PLUGIN_ROOT}/references/workflow-engine.md` (digest → shot list → per-image generation → QA → deliver) and `${CLAUDE_PLUGIN_ROOT}/references/composition-patterns.md` (structure types, format presets, the likeness rule). The `popa` sub-skill supplies the visual DNA, IP anchors, and reference images.

## Adding a mascot

To add another character later, create `skills/<mascot>/` with `SKILL.md` (name `illustration:<mascot>`), `references/` (`<mascot>-ip.md`, `<mascot>-style-dna.md`, `prompt-template.md`, `qa-checklist.md`), and `assets/<mascot>-ref/` + `assets/examples/`. The plugin exposes it automatically.
