# Alex generation prompt template

Generate each image separately. **Always pass the reference image** — likeness cannot be described in words alone:

```bash
uv run generate_image.py --prompt "..." --filename out.png \
  -i assets/alex-ref/alex-source.png
```

```text
The character in the reference image is Alex, the trademarked Sport Head mascot for ShotClubhouse. Draw THIS exact character with full likeness — a confident young person whose HEAD is a glossy golden-yellow and black soccer ball (classic pentagon panels), gold-tinted aviator sunglasses across the front of the ball, a black baseball cap worn forward with a small gold emblem, a black t-shirt with thin gold sleeve trim, brown-skinned arms, relaxed purple jogger sweatpants, and yellow-and-black high-top sneakers. Keep every anchor exact. Do NOT copy the reference's background or pose — only the character design.

Art style: bold comic-book / graphic-novel illustration. Thick confident black ink outlines, flat cel-shaded color with crisp shadow shapes, vibrant saturated palette (gold, black, purple with electric accent lighting), dynamic streetwear energy, urban-meets-stadium mood. Clean and readable, not photorealistic, not 3D, not pencil-sketch. Confident deadpan-cool street-hero attitude — never goofy.

Register:
{faithful comic for hero/social/marketing  OR  clean comic explainer for articles/how-it-works}

Aspect:
the canvas MUST be {16:9 / 1:1 / 4:5 / 21:9}

Theme:
{the one idea or feeling this image must land}

Structure type:
{hero/brand / workflow / system fragment / before-after / role state / concept metaphor / method layering / map route / mini comic panels}

Composition:
{concrete scene: where Alex is, what he is doing/embodying, the setting, how elements/flow read}

Suggested elements:
{element 1} / {element 2} / {element 3}

Labels (bold comic/graffiti hand):
{label 1} / {label 2} / {optional tagline: TAKE YOUR SHOT / OWN IT / MAKE IMPACT}

Color use:
Gold for Alex's head and hero accents. Black for ink/kit/structure. Purple as signature accent. Electric rim-light for drama. Green for pitch/verified when needed.

Constraints:
One clear focal action. Alex is the actor/hero, not decoration. Keep ShotClubhouse colorways (gold/black/purple). For explainers keep it readable — few elements, breathing room, bold labels. No title in the top-left. Do not copy the bundled examples; invent a fresh composition.
```

## Edit prompts

Fix an anchor:

```text
Edit the provided image. Alex is missing/wrong: {the gold aviators / black cap / purple joggers / yellow-black sneakers / soccer-ball head colors}. Correct it in the same bold comic style without changing anything else: composition, labels, colors, line style, aspect ratio.
```

Push the energy:

```text
Regenerate with the same idea and layout but make Alex more heroic and intentional — stronger silhouette, more dynamic stance, bolder rim-light. Keep it clean comic, on-brand gold/black/purple, deadpan-cool not goofy.
```
