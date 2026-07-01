# Composition patterns & format presets (shared)

Shared across all mascots. Mascot style/IP is layered on top per sub-skill.

## Format presets

State the aspect explicitly in every prompt and verify output dimensions.

| Use case | Aspect | Adjustments |
|----------|--------|-------------|
| Article/blog body | 16:9 | default |
| Social post (LinkedIn/X/IG) | 1:1 or 4:5 | bigger labels, one punchline allowed |
| README/website hero | 21:9 or 16:9 | extra quiet space on one side for overlay text |
| Slides | 16:9 | keep top ~15% calm for titles |
| Architecture explainer | 16:9 series | overview frame + zoom-ins (see below) |

## Structure types

Pick one per image; don't mix.

- **Workflow** — input → process → output; arrows for the main flow.
- **System fragment** — 3–5 core modules; mascot performs one key action.
- **Before/after** — chaos vs order, manual vs automatic, scattered vs converged.
- **Role state** — 2–4 small mascot states, one short label each.
- **Concept metaphor** — one memorable object/machine, few inputs, one output.
- **Method layering** — stacked layers (not a formal pyramid).
- **Map route** — one winding path, few nodes, mascot walking it.
- **Mini comic panels** — 2–4 scenes, one action each.
- **Hero / brand** — single bold subject + tagline space (marketing).

## Series pattern — split big topics across frames

Don't cram a whole system into one frame; give each topic its own (intricate) frame in a series:

1. **Overview frame** — the handful of mega-blocks as friendly objects; Popa as guide.
2. **Per-topic frames** — one intricate sketchnote per block/feature that matters (each densely drawn, busy but legible).
3. Optional **flow frame** — the one critical path as a map route.

Within a frame, be intricate (see the style DNA); across frames, split topics so no single page tries to explain everything. Each frame stands alone; number them in the shot list and keep one pastel tone so they tile.

## Likeness rule (MANDATORY — applies to every mascot)

The character cannot be reliably produced from a text description. Always pass the mascot's reference image(s) (`assets/<mascot>-ref/`) into the image backend with: "this is <Mascot>; draw THIS exact character — same anchors — translated into <mascot style>; do not copy the reference background or layout." Then verify the identity anchors survived in QA.

## Anti-copy rule

The bundled `assets/examples/` are for style calibration only — never reuse their compositions, objects, or labels unless the user explicitly says "reproduce this one." Invent a fresh metaphor per source.
