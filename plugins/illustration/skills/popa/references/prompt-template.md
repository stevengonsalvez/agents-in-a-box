# Image generation prompt template — intricate Popa sketchnote

Generate each image separately. **Pass the character references on every call** (likeness can't be described in words):

```bash
uv run generate_image.py --prompt "..." --filename out.png \
  -i assets/popa-ref/popa_square.png -i assets/popa-ref/tiny_popa2.png
```

## The text-integrity workflow (do this every time)

1. **Write the label list first** — the exact, short words the image is allowed to contain (banner + each box/tag). Nothing else may appear as text.
2. Put that list into the prompt as a hard rule (the `ALLOWED TEXT` block below).
3. After generating, **read the image and check every label**. If any is garbled or any stray text appeared, **fix it with an edit pass** (see *Edit prompts*) — do NOT full-regenerate a good composition.

```text
The pink creature in the two reference images is Popa. Draw THIS exact character — chubby pear proportions, big glossy black eyes, rosy cheeks, green sprout, small notepad — translated into crude hand-drawn pen line art (do NOT copy the 3D render style of the references, only the character design).

Generate one standalone {ASPECT} illustration. The canvas MUST be {ASPECT}. Do not copy the reference background or layout.

Style — INTRICATE SKETCHNOTE:
Crude, wobbly hand-drawn pen line art on a soft flat pastel {cream|mint|lavender|blush} background. A bold hand-lettered banner title; many small ideas in hand-drawn boxes, clouds and tags, wired together with arrows and dotted connectors; bullet lists with little stars/checkmarks; small spot icons scattered through. Busy and packed — "lots of scribblings in one note" — but with clear left-to-right hierarchy and legible lettering. No 3D, no comic cel-shading, no photoreal, no vector-flat, no mascot poster.

Popa (the host):
Popa appears in the scene with its notepad, doing or pointing at the core action — deadpan-earnest, the only fully-saturated (pink) element.

Banner title:
{the one banner headline}

Clusters (each a small boxed/cloud idea, a few words max):
{cluster 1} / {cluster 2} / {cluster 3} / {cluster 4} / {cluster 5} / {cluster 6}

Icons & connectors:
{icon 1} / {icon 2} / {icon 3}; arrows linking {A→B}, {B→C}

Color:
Pink only for Popa + key highlights. Green for the flow-line, arrows, links, success. Soft coral only for warnings. Sky blue only for system/secondary notes. Graphite for line work.

ALLOWED TEXT (CRITICAL):
The ONLY words allowed anywhere in the image are exactly these, spelled exactly and legibly, and NOTHING else:
{banner} ; {label 1} ; {label 2} ; {label 3} ; … (list every word that may appear)
Do NOT invent, scribble, or letter any other words, letters, numbers or instruction text anywhere. If a spot would need other text, draw a plain icon instead. Keep every label large enough to read.

Layout:
Optionally a {green flow-line / path} entering from the left edge and exiting the right edge to carry the eye. Busy but organised. No title in the top-left corner.
```

## Edit prompts (use these to fix, not regenerate)

Fix garbled / stray text (preferred over regenerating):

```text
Edit the provided sketchnote. Change ONLY the text so every label is clean and legible — keep all art, layout, colours and positions identical. The labels must read exactly: {list the exact correct words per region}. Remove any stray or gibberish text and any words not in this list. Do not alter the artwork.
```

Fix a missing anchor:

```text
Edit the provided image. Popa is missing {the green sprout / the notepad}. Add it in the same crude hand-drawn style; change nothing else.
```

Add more density (if too sparse):

```text
Edit the provided sketchnote to be busier — add a few more small linked boxes/notes/icons and connector arrows in the empty areas, in the same hand-drawn pastel style. Keep all existing labels exactly as they are and add only these new labels: {list}. No gibberish.
```
