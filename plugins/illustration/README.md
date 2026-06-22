# illustration

Mascot-driven explanatory & brand illustration family for Claude Code.

A shared workflow engine — **digest → shot list → per-image generation → QA → deliver** — wrapped by per-character sub-skills. Each mascot carries its own visual DNA, identity anchors, and likeness-locked reference images, so you get a consistent character across articles, social, READMEs, decks, and architecture explainers.

## Featured series — agents-in-a-box in 10 frames

A continuous `illustration:sporthead-alex` sketchnote storyboard explaining agents-in-a-box. A single gold journey-line runs edge-to-edge through every frame with Alex as the guide, so the ten panels read (and animate) as one left-to-right mural — built to be fed to an image-to-video model for a panning walkthrough.

**1 — Agents in a box** · terminal-native, runs local, open source
![1 — intro](assets/agents-in-a-box-series/01-intro.png)

**2 — The problem** · two parallel sessions stomping each other's branches
![2 — problem](assets/agents-in-a-box-series/02-problem.png)

**3 — The TUI** · one terminal cockpit — workspaces, session rows, preview pane
![3 — TUI](assets/agents-in-a-box-series/03-tui.png)

**4 — Isolation** · each session in its own box — git worktree + tmux, survives disconnect
![4 — isolation](assets/agents-in-a-box-series/04-isolation.png)

**5 — Multi-provider** · pick your player — Claude, Codex, Gemini, Copilot, Kiro
![5 — providers](assets/agents-in-a-box-series/05-providers.png)

**6 — Toolkit** · write once, deploy to 11 tools — 86 skills, 37 agents
![6 — toolkit](assets/agents-in-a-box-series/06-toolkit.png)

**7 — Plugins (v2)** · native subprocess plugins, JSON-RPC over stdio, capability-gated
![7 — plugins](assets/agents-in-a-box-series/07-plugins.png)

**8 — Burndown** · token budget + analytics by day / project / model, optimise hints
![8 — burndown](assets/agents-in-a-box-series/08-burndown.png)

**9 — WITR** · process-causality tracing, the interactive process browser (`w`)
![9 — witr](assets/agents-in-a-box-series/09-witr.png)

**10 — Hangar** · the managed-agent fleet — issues → kanban → autopilots (`g`), reflect learns across sessions
![10 — hangar](assets/agents-in-a-box-series/10-hangar.png)

## Skills

| Invoke | Mascot | Look | Best for |
|--------|--------|------|----------|
| `illustration` | — | — | Router: pick a mascot, then hands off |
| `illustration:sporthead-alex` | **Alex** — ShotClubhouse Sport Head (soccer-ball head, black cap, no glasses) | Sketchnote — hand-drawn visual notes (banners/arrows/icons) on white, optional gold accent | ShotClubhouse brand/marketing, sport, explainers, how-it-works |
| `illustration:popa` | **Popa** — pink blob, green sprout, notepad | Crude hand-drawn on soft pastel, deadpan-absurd | Dev/product/system explainers, how-it-works, architecture, friendly social |

## Examples

### `illustration:sporthead-alex` — sketchnotes (gold accent or mono)

<table>
  <tr>
    <td align="center"><img src="skills/sporthead-alex/assets/examples/01-sporthead-gold.png" width="420"><br><sub>“What is a Sport Head?” — gold accent</sub></td>
    <td align="center"><img src="skills/sporthead-alex/assets/examples/02-sporthead-mono.png" width="420"><br><sub>“What is a Sport Head?” — mono</sub></td>
  </tr>
  <tr>
    <td align="center"><img src="skills/sporthead-alex/assets/examples/03-agents-box-gold.png" width="420"><br><sub>“Agents in a box” — gold accent</sub></td>
    <td align="center"><img src="skills/sporthead-alex/assets/examples/04-agents-box-mono.png" width="420"><br><sub>“Agents in a box” — mono</sub></td>
  </tr>
</table>

### `illustration:popa` — hand-drawn pastel

<table>
  <tr>
    <td align="center"><img src="skills/popa/assets/examples/01-idea-pipeline.png" width="270"><br><sub>idea pipeline</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/02-chaos-vs-system.png" width="270"><br><sub>chaos vs system</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/03-urgent-vs-important.png" width="270"><br><sub>urgent vs important</sub></td>
  </tr>
  <tr>
    <td align="center"><img src="skills/popa/assets/examples/04-architecture-tour.png" width="270"><br><sub>architecture tour</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/05-three-states.png" width="270"><br><sub>three states</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/06-idea-to-prod-route.png" width="270"><br><sub>idea → prod route</sub></td>
  </tr>
</table>

> These are the bundled calibration examples (also in each skill's `assets/examples/`). They show the style only — the skills invent a fresh composition per request and don't reuse them.

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

## Credits

Inspired by **[Ian Xiaohei Illustrations](https://github.com/helloianneo/ian-xiaohei-illustrations)** by Ian ([@helloianneo](https://github.com/helloianneo)) — the original hand-drawn 16:9 "body illustration" skill, starring the character 小黑.

English port of the original: **[ian-illustrations-port](https://github.com/stevengonsalvez/ian-illustrations-port)** (instructions and examples translated to English; 小黑 rendered as "Blot").

This `illustration` plugin reimagines that approach as a multi-mascot family — its own characters (Alex, Popa), styles, and shared workflow engine.
