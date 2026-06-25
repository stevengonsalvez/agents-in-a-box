---
name: illustration:popa
description: Generate intricate hand-drawn sketchnote illustrations starring Popa, a cute pink blob with a green sprout and a notepad, on soft pastel scenes. One busy page of many linked boxes, notes, arrows and icons — busy but fully legible. Use for dev/product/system explainers, how-it-works, architecture, walkthroughs, and friendly social/blog/README art.
user-invocable: true
---

# illustration:popa — intricate sketchnote blob

Generate **intricate hand-drawn sketchnotes** starring **Popa**: a pink round blob with a green sprout and a notepad. Popa turns a concept, flow, or system into one busy, packed, readable sketchnote on a soft pastel scene — a banner, many linked boxes/notes/arrows/icons, Popa scribbling in the middle — *lots of scribblings in the same note*, but every word legible.

## Read first

1. `${CLAUDE_PLUGIN_ROOT}/references/workflow-engine.md` — the shared digest → shot list → generate → QA → deliver process.
2. `${CLAUDE_PLUGIN_ROOT}/references/composition-patterns.md` — structure types, format presets, the likeness rule.
3. `references/popa-ip.md` — Popa's anchors, personality, the notepad gag, forbidden moves.
4. `references/popa-style-dna.md` — the intricate-sketchnote visual law (incl. the text-integrity rule).
5. `references/prompt-template.md` — the generation prompt template.
6. `references/qa-checklist.md` — pass/fail and iteration.
7. `assets/popa-ref/` — Popa's renders, passed as the likeness reference on every generation (mandatory).
8. `assets/examples/` — calibration only; never copy these compositions.

## Core positioning

The goal is to turn a topic — a process, system, argument, or set of features — into one **intricate hand-drawn sketchnote**: a banner, many small linked ideas in boxes/clouds/tags, arrows and spot icons, with Popa hosting the action. Busy and packed ("lots of scribblings in the same note"), but with clear hierarchy and **legible text — no gibberish, ever**. Popa is earnest and deadpan; the cuteness is the setup, the situation is the punchline. Popa hosts the note, never just decorates it.

## Workflow (engine + Popa specifics)

Follow the five shared steps. Popa specifics:

- **Likeness is mandatory:** pass `assets/popa-ref/popa_square.png` (and `tiny_popa2.png` for the notepad pose) into the backend with "this is Popa; draw THIS exact character — chubby pear body, green sprout, glossy eyes, rosy cheeks — translated into crude hand-drawn pen line art; do not copy the reference's 3D style or background."
- **Intricate density:** pack the page with many linked boxes, notes, arrows and icons — busy, not sparse. Big topics can also become a multi-frame series (see composition-patterns).
- **Text integrity (hard rule):** decide the exact short labels first and allow ONLY those words in the image; never let the model invent text. If a finished frame has a garbled label, fix it with a text-only **edit**, not a regenerate. See `references/prompt-template.md`.
- **The notepad gag:** when there's room, one tiny legible line on the notepad. Max one per image.
- Pastel background: cream by default (or mint / lavender / blush); for a series keep one tone across frames so they tile. Popa is the only saturated element.

## Output style

Pre-generation strategy: short and precise. Delivery report: how many, what each is for, save paths, which are solid vs optional. Let the images talk.
