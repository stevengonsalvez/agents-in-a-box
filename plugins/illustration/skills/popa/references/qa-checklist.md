# QA Checklist

## Must pass

- Correct aspect for the format preset (16:9 / 1:1 / 4:5 / 21:9). Verify dimensions, don't eyeball.
- Soft flat pastel background (cream/lavender/mint/blush), not white, not saturated, not textured.
- Crude hand-drawn line style — no 3D, no vector polish.
- All four Popa anchors present: green sprout, pink blob body, glossy eyes + tiny smile, notepad somewhere in scene.
- Popa carries the core action, not decoration (litmus test: remove Popa, metaphor should collapse).
- Fresh metaphor — no example composition reuse.
- One core structure only; 3-5 elements.
- Labels: ≤8, short, readable English.
- Color discipline: green = flow, coral = warnings only, blue = system notes only, pink = Popa/highlights only.
- It's funny: there is an absurd-but-coherent situation, and optionally one notepad gag or one background micro-joke (never both crowding).

## Failure signals

Regenerate or locally edit when:

- Aspect drifted from the preset.
- Background went white, saturated, gradient-heavy, or textured.
- Style drifted to 3D/clay/vector/mascot-poster.
- An anchor is missing (most common: sprout or notepad).
- Popa is mugging — sweat drops, panic, sparkles, heart eyes.
- Looks like PPT, course slide, or formal flowchart.
- More than one micro-joke, or the gag competes with structure.
- Too many elements, arrows, or nodes.
- Top-left type-title appeared.
- Too similar to an `assets/examples/` composition.

## How to iterate

- Too plain: give Popa a stranger (but coherent) job; add the notepad gag.
- Too crowded: cut to one action + 3-5 elements + ≤5 labels.
- Too cute/sugary: strip sparkles and poses; emphasize deadpan diligence, bureaucratic calm.
- Too stiff/diagram-y: remove borders and grids; replace one box with a low-tech object.
- Anchor missing: prefer local edit (see prompt-template edit prompts).
- Wrong text: local edit first; many errors → regenerate with fewer labels.

## Delivery judgment

A high-quality Popa image makes the reader smile in the first second, understand the structure in the next, and want to repost it. If the first glance reads "corporate explainer" or "mascot ad," it fails.
