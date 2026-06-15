---
name: explain-to-me
description: |
  Produce a self-contained, richly styled HTML explainer for any topic the
  user asks about. Picks the right template from a bundled set of 22
  visual patterns (feature explainer, concept explainer, module map, PR
  review, ADR, options paper / trade-off analysis, system diagram, flow-
  chart, status report, slide deck, prototype, editor, etc.), fills it
  with real content, augments with inline diagrams via sister skills
  (/fireworks-tech-graph for architecture / flow / sequence diagrams,
  /graphify for knowledge graphs), applies a Claude-brand polish layer,
  and publishes to
  here.now at a topic-slug URL so the link is shareable immediately.
  Local-only output is available with --local. Use when Stevie says
  "/explain-to-me", "explain-to-me X", "make me an explainer for X",
  "give me an HTML explainer", "render this as a webpage", "ADR for X",
  "options paper for X", or asks for a rich visual writeup. The skill
  picks the template, names the choice up-front, and reaches for
  diagrams whenever the content shape needs them.
argument-hint: "[topic — e.g. 'how rate limiting works in our api'] [--local]"
---

# /explain-to-me — rich HTML explainers

## What it does

Takes a topic (a feature, a concept, a decision, a plan, an incident, …)
and produces a single self-contained HTML file styled in Claude's brand,
using whichever bundled template best fits the topic's shape. By default
it then publishes the file via `/here-now` and returns the shareable URL.

All 22 templates use Claude's palette (`#FAF9F5` ivory, `#141413` slate,
`#D97757` clay, `#E3DACC` oat, `#788C5D` olive). The skill applies
[`assets/claude-theme.css`](assets/claude-theme.css) on top for typography +
brand badge.

## Trigger

- `/explain-to-me <topic>` — primary
- `/explain-to-me <topic> --local` — skip the here.now publish, just write the file
- `/explain-to-me` (no args) — ask the user what to explain
- Natural language: "explain X to me as a webpage", "make an HTML
  explainer for X", "render this concept as HTML", "ADR for choosing X",
  "options paper on Y", "give me a visual writeup of Z"

## Flow

### 1. Lock the topic and intent

If `$ARGUMENTS` is non-empty, use it as the topic. Otherwise ask one
question via `AskUserQuestion`: "What should I explain? One line."

Then classify the topic against this table — this drives template
choice. **Pick exactly one template; do not merge.**

Templates are tagged **★** when they are visual-first (diagrams, charts,
SVG flows). When two templates fit, prefer the **★** one unless the
topic is intrinsically prose-heavy.

| Topic shape | Template | Visual |
|---|---|---|
| Architecture / design decision with options + rationale | `21-adr.html` | ★ |
| Options paper / trade-off analysis (no decision yet) | `22-options-paper.html` | ★ |
| Implementation plan with milestones + diagrams | `16-implementation-plan.html` | ★ |
| Pipeline / process flowchart | `13-flowchart-diagram.html` | ★ |
| Mental model of an unfamiliar repo/module | `04-code-understanding.html` | ★ |
| Inline SVG figure sheet | `10-svg-illustrations.html` | ★ |
| Abstract concept / algorithm with interactive demo | `15-research-concept-explainer.html` | ★ |
| Concrete feature in a codebase ("how X works in repo Y") | `14-research-feature-explainer.html` | |
| PR review (reviewer perspective) | `03-code-review-pr.html` | |
| PR writeup (author perspective) | `17-pr-writeup.html` | |
| N approaches compared (code) | `01-exploration-code-approaches.html` | |
| N visual directions compared | `02-exploration-visual-designs.html` | ★ |
| Weekly / sprint status | `11-status-report.html` | ★ |
| Incident post-mortem | `12-incident-report.html` | ★ |
| Component variant matrix | `06-component-variants.html` | ★ |
| Design system reference | `05-design-system.html` | ★ |
| Animation / micro-interaction demo | `07-prototype-animation.html` | ★ |
| Multi-screen clickable prototype | `08-prototype-interaction.html` | |
| Slide deck (arrow-key, one file) | `09-slide-deck.html` | |
| Ticket triage / kanban | `18-editor-triage-board.html` | |
| Feature flag editor | `19-editor-feature-flags.html` | |
| Prompt template tuner | `20-editor-prompt-tuner.html` | |

Full per-template detail lives in [`references/template-catalog.md`](references/template-catalog.md).
Read it only when the topic doesn't cleanly match, or you need to know
which interactive elements a template ships with.

### 1.5 Announce the choice

Before generating anything, tell Stevie which template you picked and
why, in one line:

> Picking `21-adr.html` — you described a decision with options and
> rationale; this template gives you status badge, options cards with
> pros/cons + score bars, decision callout, and resulting architecture
> diagram.

This is a transparency step. If the choice is wrong, Stevie can redirect
before you spend tokens generating content.

### 2. Gather content

Treat the template as the *shape* of the answer. For each named region
in the template (TL;DR, steps, options, scores, FAQ, glossary, timeline,
consequences, etc.), produce real content for the user's topic. Pull
from:

- Files in the current repo (for code/feature/PR/ADR templates)
- The user's prior conversation
- Your own knowledge of the concept

Do not invent file paths or commit hashes. If a region of the template
expects a concrete artifact you don't have, drop the region rather
than fake it.

### 3. Augment with visual sister skills

Many templates have a *big diagram* slot. If the topic is technical and
the diagram would carry real weight, generate one inline via a sister
skill rather than hand-drawing SVG:

| Diagram need | Reach for | Output |
|---|---|---|
| Architecture · data flow · sequence · agent/memory · concept map | `/fireworks-tech-graph` | SVG + PNG (drop SVG inline) |
| Knowledge graph from code/docs/papers — clustered, communities | `/graphify` | HTML / JSON / SVG |
| Data-driven infographic — stats, comparisons, processes, timelines as a designed poster-style figure | `/infographic-creator` (AntV) | SVG (drop inline) |

For small bespoke SVG (decorative icons, hero glyphs, simple
illustrations) — author the inline SVG directly. You're capable of it
and it keeps the file self-contained.

Workflow:

1. Decide which template region needs the diagram (e.g. ADR step 04
   "Resulting architecture"; concept-explainer's hero figure;
   implementation-plan's data-flow block).
2. Invoke the sister skill with a tight prompt describing exactly the
   diagram you want (boxes, arrows, labels). Tell it to return an
   inlineable SVG when possible.
3. Replace the template's placeholder SVG with the generated one.
   Keep the `viewBox` and outer `<svg>` wrapper sizing so the layout
   doesn't shift.
4. Cite the generator at the bottom of the section (e.g. "diagram
   generated via /fireworks-tech-graph").

Use this only when the diagram is load-bearing. Don't replace the small
mini-architecture SVGs in ADR option cards — those are intentionally
sketch-like to read at a glance.

### 4. Render

1. **Copy** the chosen template to `./explainers/<slug>.html` (create
   `./explainers/` if missing). `<slug>` is hyphen-case of the topic
   and identifies the *local file only*. The here.now URL is
   server-assigned (see §5) — they do not match.
2. **Update** the `<title>`, the `.eyebrow` text, and the `h1`.
3. **Replace** placeholder content (acme/*, ADR-0023, PR #247, "rate
   limiting" strings, sample names) with the user's real topic.
4. **Strip** any region that you couldn't fill — better to ship a
   shorter explainer than a fake one. Don't invent file paths to fill
   the nav rail; if there are none, delete the `nav .files` block.
5. **Inject the Claude theme** by running
   `scripts/inject_theme.py <output.html>` from the skill directory.
   The script inserts `assets/claude-theme.css` as a second `<style>`
   block (marked `data-claude-theme="injected"`) right after the
   template's existing `</style>`, and is idempotent on re-runs. The
   overlay only touches typography, focus states, and adds the brand
   badge — layout untouched.
6. **Preserve** every `<script>` block verbatim unless changing the
   demo's data shape.

### 5. Publish to here.now (default)

Unless the user passed `--local`, publish the file via the `/here-now`
skill (`~/.claude/skills/here-now/scripts/publish.sh`).

**Critical: you do not get to choose the URL on a first publish.**
The `publish.sh --slug <slug>` flag means *"update an existing publish
at this slug"*, not *"create a new publish with this URL"*. If you
pass `--slug` on a first publish, the server returns `Not found`
because there's nothing at that slug to update yet. Server-assigned
three-word slugs (e.g. `woody-mortar-9dmd`) are the only path for new
publishes via this CLI.

Procedure:

1. Invoke `publish.sh` with the file path and **no `--slug` flag**:
   ```
   bash ~/.claude/skills/here-now/scripts/publish.sh \
     ./explainers/<slug>.html \
     --title "<page title>" \
     --description "<one-line summary>" \
     --client claude-code
   ```
2. Capture the returned URL (a server-assigned three-word subdomain).
3. If the user explicitly asks for a custom-URL publish, do the
   sequence: publish first to get the auto-slug + claim token, then
   re-invoke `publish.sh --slug <new-slug> --claim-token <token>` to
   rename. Most users don't care; don't volunteer this dance unless
   asked.
4. If `/here-now` is unavailable in the current environment, fall
   back to local-only mode and surface the path. Tell Stevie what
   happened — don't pretend you published.

### 6. Hand off

Report to Stevie in this exact shape:

> **Explainer ready.**
> - Template: `21-adr.html` — *why this one*
> - Local: `./explainers/<slug>.html`
> - Live: `https://<server-slug>.here.now`  *(or "skipped, --local")*
> - Diagrams: from /fireworks-tech-graph  *(omit line if none)*

If the returned URL is a three-word auto-slug (e.g.
`woody-mortar-9dmd.here.now`) and the topic would benefit from a
memorable URL, mention that a re-publish with `--slug` + claim token
can rename it — but don't do it automatically.

## Customisation rules

- **Palette is fixed.** The templates *are* the Claude theme. Don't
  rewrite the CSS variables unless the user explicitly asks for a
  different look.
- **One template per output.** Do not merge two templates into one
  page; pick the better fit and commit.
- **No framework, no build.** Static HTML + inline `<style>` +
  inline `<script>`. If the user wants React, that's a different
  skill (`frontend-engineer`).
- **Strip, don't pad.** A 3-section explainer that's all real beats
  a 7-section one that's half fake.
- **Visual over textual.** When the topic admits a diagram, generate
  one via a sister skill rather than describing the architecture in
  prose.

## Anti-patterns (and the fix)

- **Picking a template by aesthetic, not by topic shape.** The reader
  gets a pretty page that doesn't fit the content. → Re-read the
  selection table and pick the closest shape; the styling is identical
  across all 22 anyway.
- **Inventing file paths / line numbers / commit hashes** to fill
  citations the template expects. → Drop the citation region.
- **Skipping the theme overlay** because "it already looks Claude-y".
  The overlay adds the brand badge and the focus ring — keep it.
- **Skipping the publish step** silently. → If `/here-now` fails, say
  so explicitly. Don't return only a local path when Stevie expected
  a URL.
- **Passing `--slug` on a first publish.** That flag means *update an
  existing publish at this slug*, not *choose a URL*. The server
  returns `Not found` and the agent often misreads it as a real
  failure. → Omit `--slug` on first publish; let the server assign a
  three-word slug. Only use `--slug` together with `--claim-token`
  for a deliberate rename.
- **Hand-drawing a complex SVG architecture diagram** when
  `/fireworks-tech-graph` could produce a cleaner one. → Delegate.
- **Putting the output inside the toolkit repo.** Always write to the
  user's current working directory under `./explainers/`.

## Output location

- Default: `./explainers/<slug>.html` relative to the user's cwd
  (where `<slug>` is the local file slug). Published to a
  server-assigned URL `https://<three-word-slug>.here.now/` — not
  derived from the local filename.
- `--local`: skip the publish; just leave the file at the path above.
- If `./explainers/` is awkward (e.g. the cwd is read-only), put the
  file in `$CLAUDE_JOB_DIR` (or `/tmp`) and tell the user the
  absolute path.
