# Popa IP

## Character definition

Popa is the fixed visual IP for popa-illustration. Source: the mascot from `stevengonsalvez/popajob` (see `assets/popa-ref/` — character reference only; the target render is crude hand-drawn, not 3D).

Popa appears in every image. Popa is not a sticker or a logo — Popa is the earnest worker actually operating the system the image explains.

## Non-negotiable anchors (every image)

1. **Green sprout on head** — the silhouette signature. One or two leaves, slightly askew.
2. **Pink chubby pear-shaped body** — wider at the bottom, stubby arms and legs, slightly uneven hand-drawn outline.
3. **Big glossy black eyes, rosy cheeks, tiny smile** — the face stays calm and pleasant. No expression comedy.
4. **Notepad + pen** — present in EVERY scene. In hand when possible; if both hands are busy, the notepad is tucked under an arm, lying open on the floor, clipped to the sprout, or balanced on something nearby.

## Likeness rule (MANDATORY)

Words alone produce a generic pink blob, not Popa. Every generation MUST pass the character reference images as model inputs:

- `assets/popa-ref/popa_square.png` (full body, sprout)
- `assets/popa-ref/tiny_popa2.png` (notepad + pen pose)

with an instruction like: "The pink character in the reference images is Popa. Draw THIS exact character — same proportions, same face, same sprout, same cheeks — translated into crude hand-drawn pen line art." Never generate Popa from a text description alone.

## Personality

- Very earnest, slightly bureaucratic. Treats absurd jobs as standard procedure.
- Deadpan: never winks at the camera, never panics, never mugs.
- The comedy IS the contrast: an adorable creature filing serious paperwork about a ridiculous machine.
- Diligent, mildly proud of small things.

## The notepad gag

A recurring micro-joke: Popa documents the absurd work. When an image has room for it, one tiny handwritten line on or near the notepad, e.g.:

- "day 47: box still empty"
- "logged: 1 ticket closed"
- "note: lever sticky"
- "all normal"

Max one gag line per image. It must not compete with the structural labels.

## Common duties

Let Popa carry the core action:

- Cranking, weighing, sorting, stamping, filing, sealing, patching.
- Operating a judgment lever, guarding a gate, lowering things into boxes.
- Walking a route with a clipboard, inspecting pits, taking inventory.
- Catching falling items, threading strings between pins, watering a system like a plant.
- Giving a deadpan tour of an architecture block.

## One background micro-joke (optional)

At most ONE per image, small and quiet: a snail coworker, a "no. 1 blob" mug, a tiny framed certificate, one confetti piece for a tiny victory. Skip it when the frame is busy.

## Forbidden

- No expression comedy: no sweat drops, no anime panic, no heart eyes, no shouting.
- No mascot-poster framing (Popa centered, beaming, arms up, sparkles).
- No clothing or accessories beyond the notepad/pen and job-specific tools.
- Never let Popa just stand beside the diagram watching.
- Never drop an anchor (sprout, pink, face, notepad) for style reasons.

## Litmus test

Remove Popa from the image. If the core metaphor still fully works, Popa was decoration — rewrite the prompt so Popa is the one doing the work.
