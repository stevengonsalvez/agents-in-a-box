---
name: illustration:popa
description: Generate illustrations starring Popa, a cute pink blob with a green sprout and a notepad, in crude hand-drawn line art on soft pastel scenes with deadpan-absurd humor. Use for dev/product/system explainers, how-it-works, architecture, methodologies, and friendly social/blog/README art. Sleek but funny; Popa earnestly does absurd system jobs.
user-invocable: true
---

# illustration:popa — deadpan pastel blob

Generate hand-drawn explanatory illustrations starring **Popa**: a pink round blob with a green sprout, big glossy eyes, and a notepad. Popa turns a concept, flow, or system into one clean, funny, readable image on a soft pastel scene — deadpan-absurd, sleek but never an instruction manual.

## Read first

1. `${CLAUDE_PLUGIN_ROOT}/references/workflow-engine.md` — the shared digest → shot list → generate → QA → deliver process.
2. `${CLAUDE_PLUGIN_ROOT}/references/composition-patterns.md` — structure types, format presets, the likeness rule.
3. `references/popa-ip.md` — Popa's anchors, personality, the notepad gag, forbidden moves.
4. `references/popa-style-dna.md` — the crude-line + pastel visual law.
5. `references/prompt-template.md` — the generation prompt template.
6. `references/qa-checklist.md` — pass/fail and iteration.
7. `assets/popa-ref/` — Popa's renders, passed as the likeness reference on every generation (mandatory).
8. `assets/examples/` — calibration only; never copy these compositions.

## Core positioning

The goal is not commercial illustration or PPT infographics — it is to turn a key judgment, process, structure, state, or metaphor into one clean, weird, readable hand-drawn image on a soft pastel scene. Popa is earnest and slightly bureaucratic, fully committed to an absurd job; the cuteness is the setup, the deadpan absurd labor is the punchline. Popa must carry the core action — never decorate it.

## Workflow (engine + Popa specifics)

Follow the five shared steps. Popa specifics:

- **Likeness is mandatory:** pass `assets/popa-ref/popa_square.png` (and `tiny_popa2.png` for the notepad pose) into the backend with "this is Popa; draw THIS exact character — chubby pear body, green sprout, glossy eyes, rosy cheeks — translated into crude hand-drawn pen line art; do not copy the reference's 3D style or background."
- **The notepad gag:** when there's room, one tiny line on the notepad logging the absurd work ("day 47: box still empty"). Max one per image.
- **Strict-minimal density:** 3–5 elements; big architectures become a series (see composition-patterns).
- Pastel scenes rotate cream / lavender / mint / blush; Popa is the only saturated element.

## Output style

Pre-generation strategy: short and precise. Delivery report: how many, what each is for, save paths, which are solid vs optional. Let the images talk.
