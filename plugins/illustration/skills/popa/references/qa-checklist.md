# QA Checklist — intricate Popa sketchnote

## Must pass

- Correct aspect for the format preset (16:9 / 1:1 / 4:5 / 21:9). Verify dimensions, don't eyeball.
- Soft flat pastel background (cream/mint/lavender/blush) — not white, not saturated, not textured.
- Crude hand-drawn sketchnote line style — banner + boxes/clouds + arrows + bullets + spot icons. No 3D, no comic cel-shading, no vector polish, no photoreal.
- **Intricate / dense**: many small linked elements, busy page, little dead space — "lots of scribblings in one note".
- **Text integrity**: every label is legible and is one of the exact words you specified. NO gibberish, NO invented/stray text. This is a hard gate.
- Popa anchors present: green sprout, pink blob body, glossy eyes, notepad — and Popa is the host of the action, not decoration.
- Clear hierarchy despite density: one banner, grouped clusters, readable flow.
- Colour discipline: green = flow/links, coral = warnings only, blue = system notes only, pink = Popa/highlights.

## Failure signals

Fix (prefer an edit) when:

- **Any garbled or stray text** — top priority; never ship gibberish.
- Aspect drifted; background went white/saturated/textured.
- Style drifted to 3D/comic/vector/mascot-poster.
- Too sparse / not intricate enough (big empty areas, only a couple of elements).
- An anchor missing (sprout or notepad), or Popa is mugging (sparkles/panic).
- A top-left format-title appeared.
- Too similar to a bundled example.

## How to iterate

- **Garbled/stray text** (most common): run a text-only **edit** with the exact correct words per region — do NOT full-regenerate (it re-garbles and loses the composition). See prompt-template *Edit prompts*.
- Too sparse: edit to add more linked boxes/notes/icons + connectors in the empty areas; specify the new labels explicitly.
- Too cluttered to read: drop a few low-value tags; enlarge remaining labels.
- Anchor missing: local edit.
- Off-model Popa: pass the reference images again and restate the anchors.

## Delivery judgment

A strong Popa sketchnote is busy and rewarding — many little linked ideas, Popa in the thick of it — and reads cleanly the whole way through. If any word is gibberish, it fails, no matter how good the art is.
