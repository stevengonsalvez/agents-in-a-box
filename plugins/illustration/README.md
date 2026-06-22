# illustration

Mascot-driven explanatory & brand illustration family for Claude Code.

A shared workflow engine — **digest → shot list → per-image generation → QA → deliver** — wrapped by per-character sub-skills. Each mascot carries its own visual DNA, identity anchors, and likeness-locked reference images, so you get a consistent character across articles, social, READMEs, decks, and architecture explainers.

## Skills

| Invoke | Mascot | Look | Best for |
|--------|--------|------|----------|
| `illustration` | — | — | Router: pick a mascot, then hands off |
| `illustration:alex` | **Alex** — ShotClubhouse Sport Head (soccer-ball head, black cap, no glasses) | Sketchnote — hand-drawn visual notes (banners/arrows/icons) on white, optional gold accent | ShotClubhouse brand/marketing, sport, explainers, how-it-works |
| `illustration:popa` | **Popa** — pink blob, green sprout, notepad | Crude hand-drawn on soft pastel, deadpan-absurd | Dev/product/system explainers, how-it-works, architecture, friendly social |

## How it works

- Shared engine: `references/workflow-engine.md`
- Shared structures, format presets, and the **likeness rule**: `references/composition-patterns.md`
- Each mascot: `skills/<mascot>/` with `SKILL.md`, `references/` (IP, style DNA, prompt template, QA), and `assets/` (`<mascot>-ref/` likeness images + `examples/` calibration).

**Likeness rule:** characters cannot be reliably produced from text alone — every generation passes the mascot's reference image(s) into the image backend (e.g. nano-banana-pro / Gemini 3 Pro Image, or `image_gen`).

## Add a mascot

Create `skills/<mascot>/` following the layout above (SKILL.md `name: illustration:<mascot>`). The plugin exposes it automatically — no further registration.

## Install

```
/plugin install illustration@agents-in-a-box
```
