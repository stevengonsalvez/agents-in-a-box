# Image generation prompt template

Generate each image separately. Replace the variables; never collage multiple images into one.

**MANDATORY: pass the character references as input images on every call** (likeness cannot be described in words):

```bash
uv run generate_image.py --prompt "..." --filename out.png \
  -i assets/popa-ref/popa_square.png -i assets/popa-ref/tiny_popa2.png
```

```text
The pink creature in the two reference images is Popa. Draw THIS exact character — same chubby pear proportions, same big glossy black eyes, same rosy cheeks, same green sprout — translated into crude hand-drawn pen line art (do NOT copy the 3D render style of the references, only the character design).

Generate one standalone {ASPECT} illustration. The canvas MUST be {ASPECT} — do not drift the aspect ratio. Do not copy the background or layout of the reference images.

Visual DNA:
Crude, wobbly hand-drawn pen line art on a soft flat pastel {cream|lavender|mint|blush} paper-tone background. Slightly uneven lines, sketchbook feel. Lots of calm empty space. Sparse handwritten English labels. Sleek but funny product-sketch energy. No 3D render, no clay style, no vector illustration, no PPT infographic look, no gradients-heavy rendering, no photo texture, no realistic UI, no mascot poster.

Recurring IP character required:
Popa, a small pink round blob creature with a little green sprout on its head, big glossy black eyes, a tiny calm smile, stubby arms and legs, drawn in the same crude hand-drawn line style with pink fill. Popa carries a small notepad and pen — in hand, tucked under an arm, or lying open nearby. Popa must perform the core conceptual action with deadpan earnestness, like a diligent worker who thinks this absurd job is completely normal. The humor comes from the situation, never from Popa's expression.

Theme:
{theme}

Structure type:
{Workflow / system fragment / before-after contrast / role state / concept metaphor / method layering / map route / mini comic panels / architecture overview / architecture zoom-in}

Core idea:
{the one idea this image expresses}

Composition:
{concrete scene: where Popa is, what absurd-but-coherent job it is doing, the main low-tech object, how information flows}

Suggested elements:
{element 1} / {element 2} / {element 3}

Handwritten labels:
{label 1} / {label 2} / {label 3} / {label 4} / {optional 5}

Optional notepad gag (one tiny line on the notepad, skip if busy):
{e.g. "day 47: box still empty"}

Color use:
Pink only for Popa and key highlights. Green for the sprout and main flow/paths/arrows/success. Soft coral only for warnings/problems/painful results. Sky blue only for system state or secondary notes. Soft graphite for line work and structure.

Constraints:
One image explains only one core structure. Main subject 40-60% of the canvas; keep at least 30% calm space. At most 5-8 short handwritten labels. No title in the top-left corner. Never write the structure type on the image. Do not copy prior examples; invent a fresh visual metaphor for this content. Clear but not instructional, funny but not childish, cute but deadpan.
```

## Edit prompts

Fix a missing anchor:

```text
Edit the provided image. Popa is missing {the green sprout / the notepad}. Add it in the same crude hand-drawn style without changing anything else: same composition, labels, colors, line style, aspect ratio.
```

Make Popa matter more:

```text
Regenerate with the same core meaning and layout, but make Popa the one performing the central action — operating, carrying, logging — not standing beside the diagram. Keep it pastel, sparse, hand-drawn, deadpan.
```

De-clutter:

```text
Regenerate with the same metaphor but remove all but {N} elements and at most 5 short labels. More calm space. One structure only.
```
