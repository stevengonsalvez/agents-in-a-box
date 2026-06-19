# Alex generation prompt template

Generate each image separately. **Always pass the reference image** — likeness cannot be described in words alone:

```bash
uv run generate_image.py --prompt "..." --filename out.png \
  -i assets/alex-ref/alex-source.png
```

```text
The character in the reference image is Alex, the trademarked Sport Head mascot for ShotClubhouse. Use the reference ONLY for his character design and proportions — NOT its colours, background, or rendering. Alex: a soccer-ball head with classic pentagon panels and ABSOLUTELY NO glasses or sunglasses (plain ball), a black baseball cap worn forward, a simple t-shirt, brown-skinned arms, relaxed jogger sweatpants, simple sneakers, often a small football at his feet. Do NOT put glasses on him.

Art style: crude, wobbly HAND-DRAWN pen line art on a soft flat pastel {cream|mint|lavender|blush} background. Slightly uneven sketchy lines, sketchbook feel, lots of calm empty white space, only a few short handwritten labels. Cute but deadpan, charming like a witty napkin sketch. NOT a polished comic, NOT bold-ink graphic-novel, NOT cel-shaded, NOT 3D, NOT photorealistic. Draw Alex with simple, slightly rounded cute proportions; he is the most saturated thing in the frame, his soccer-ball head kept gold-and-black. Deadpan-absurd humour from the situation, never goofy faces.

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

Handwritten labels:
{label 1} / {label 2} / {optional tagline: take your shot}

Color use:
Gold for Alex's soccer-ball head and key highlights. Green for flow/paths/success. Soft coral for warnings only. Sky blue for system/secondary notes. Soft graphite for line work. Restrained overall.

Constraints:
One core idea. Alex is the actor, not decoration. Keep generous calm white space, subject 40–60% of canvas, at most 5–8 short handwritten labels. No title in the top-left. Do not copy the bundled examples; invent a fresh composition. Crude hand-drawn pastel — cute but deadpan, never bold-comic.
```

## Edit prompts

Fix an anchor:

```text
Edit the provided image in the same crude hand-drawn pastel style. Alex is missing/wrong: {glasses must be removed / black cap / gold-black ball head / football at feet}. Correct it without changing anything else: composition, labels, colours, line style, aspect ratio.
```

Make Alex matter more:

```text
Regenerate with the same idea and layout but make Alex the one performing the central action, not standing beside the scene. Keep it crude hand-drawn pastel, deadpan, sparse, no glasses.
```
