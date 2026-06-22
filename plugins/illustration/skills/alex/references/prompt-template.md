# Alex generation prompt template — sketchnote

Generate each image separately. **Always pass the reference image** — likeness cannot be described in words alone:

```bash
uv run generate_image.py --prompt "..." --filename out.png \
  -i assets/alex-ref/alex-source.png
```

```text
One recurring doodle character is Alex, the trademarked Sport Head mascot for ShotClubhouse. Use the reference image ONLY for his design — a soccer-ball head with pentagon panels and a PLAIN surface with NO eyewear, a black baseball cap worn forward, simple t-shirt, jogger sweatpants, sneakers, a small football nearby. Draw him as a small hand-drawn doodle inside the note. IMPORTANT: do NOT letter any instruction words such as "no glasses" anywhere in the image.

Art style: SKETCHNOTE / visual note-taking — a one-page hand-drawn visual summary. Hand-drawn black ink on a clean white background. Bold hand-lettered headline words in ribbon/banner shapes; ideas inside hand-drawn boxes, frames and speech-clouds connected by arrows and dotted lines; bullets with stars, checkmarks and numbered circles; small spot icons and doodles; clear visual hierarchy with breathing room. Use ONLY short hand-lettered words and labels — NO paragraphs.

Accent:
{GOLD: black ink on white plus a SINGLE gold/yellow accent on banners and Alex's ball head  |  MONO: pure black and white only, no colour}

Aspect:
the canvas MUST be {16:9 / 1:1 / 4:5 / 21:9}

Topic / headline:
{the banner title — the one idea the note captures}

Note contents (keep each to a few words):
{key point 1} / {key point 2} / {key point 3} / {a checklist or numbered steps} / {a star callout or tagline}

Icons & doodles:
{icon 1} / {icon 2} / {icon 3}  (e.g. ball, ID card, trophy, gears, whistle)

Alex's action:
{where Alex appears and what he's doing — pointing, kicking, presenting the ID}

Constraints:
One clear page with strong hierarchy. Alex appears as the recurring doodle host, not a giant centerpiece. At most one accent colour (gold) or none (mono). Short hand-lettered labels only — no paragraphs. No top-left format title. Do not copy the bundled examples; compose a fresh note.
```

## Edit prompts

Fix an anchor / leak:

```text
Edit the provided sketchnote in the same hand-drawn ink style. {Remove Alex's glasses / remove the stray lettered words 'no glasses' / fix the banner title text}. Keep everything else the same: layout, labels, icons, arrows, accent, aspect ratio.
```

De-clutter:

```text
Regenerate the same note with fewer elements and shorter labels — clearer hierarchy, more white space, one banner title and 4-6 framed points max.
```
