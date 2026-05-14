---
name: explain-to-me
description: |
  Produce a self-contained, richly styled HTML explainer for any topic the
  user asks about. Picks the right template from a bundled set of 20
  patterns (feature explainer, concept explainer, module map, PR review,
  status report, slide deck, flowchart, prototype, editor, etc.), fills
  it with real content, and applies a Claude-brand polish layer. Output
  is one HTML file the user can open directly — no build step, no
  framework. Use when Stevie says "/explain-to-me", "explain-to-me X",
  "make me an explainer for X", "give me an HTML explainer", "explain
  this as a webpage", "render this concept as HTML", or asks for a rich
  visual writeup of how something works. The skill picks the template;
  the user does not need to know which one exists.
argument-hint: "[topic — e.g. 'how rate limiting works in our api']"
---

# /explain-to-me — rich HTML explainers

## What it does

Takes a topic (a feature, a concept, a PR, a plan, an incident, …) and
produces a single self-contained HTML file styled in Claude's brand,
using whichever bundled template best fits the topic's shape.

The 20 templates already use Claude's palette (`#FAF9F5` ivory,
`#141413` slate, `#D97757` clay, `#E3DACC` oat, `#788C5D` olive).
This skill picks one, fills it, and overlays
[`assets/claude-theme.css`](assets/claude-theme.css) for typography +
brand badge.

## Trigger

- `/explain-to-me <topic>` — primary
- `/explain-to-me` (no args) — ask the user what to explain
- Natural language: "explain X to me as a webpage", "make an HTML
  explainer for X", "render this concept as HTML"

## Flow

### 1. Lock the topic and intent

If `$ARGUMENTS` is non-empty, use it as the topic. Otherwise ask one
question via `AskUserQuestion`: "What should I explain? One line."

Then classify the topic against this table — this drives template
choice. **Pick exactly one template; do not merge.**

| Topic shape | Template |
|---|---|
| Concrete feature in a codebase ("how X works in repo Y") | `14-research-feature-explainer.html` |
| Abstract concept / algorithm / protocol | `15-research-concept-explainer.html` |
| Mental model of an unfamiliar repo/module | `04-code-understanding.html` |
| PR review (reviewer perspective) | `03-code-review-pr.html` |
| PR writeup (author perspective) | `17-pr-writeup.html` |
| N approaches compared (code) | `01-exploration-code-approaches.html` |
| N visual directions compared | `02-exploration-visual-designs.html` |
| Implementation plan with milestones | `16-implementation-plan.html` |
| Weekly / sprint status | `11-status-report.html` |
| Incident post-mortem | `12-incident-report.html` |
| Pipeline / process flowchart | `13-flowchart-diagram.html` |
| Component variant matrix | `06-component-variants.html` |
| Design system reference | `05-design-system.html` |
| Animation / micro-interaction demo | `07-prototype-animation.html` |
| Multi-screen clickable prototype | `08-prototype-interaction.html` |
| Slide deck (arrow-key, one file) | `09-slide-deck.html` |
| Inline SVG figure sheet | `10-svg-illustrations.html` |
| Ticket triage / kanban | `18-editor-triage-board.html` |
| Feature flag editor | `19-editor-feature-flags.html` |
| Prompt template tuner | `20-editor-prompt-tuner.html` |

Full per-template detail (interactivity, when each shines, anti-patterns)
lives in [`references/template-catalog.md`](references/template-catalog.md).
Read that file *only* when the topic doesn't cleanly match the table
above, or when you need to know which interactive elements a template
ships with.

### 2. Gather content

Treat the template as the *shape* of the answer. For each named region
in the template (TL;DR, steps, FAQ, glossary, timeline, etc.), produce
real content for the user's topic. Pull from:

- Files in the current repo (for code/feature/PR templates)
- The user's prior conversation
- Your own knowledge of the concept

Do not invent file paths or commit hashes. If a region of the template
expects a concrete artifact you don't have, drop the region rather
than fake it.

### 3. Render

1. **Copy** the chosen template to `./explainers/<slug>.html` (create
   `./explainers/` if missing). `<slug>` is hyphen-case of the topic.
2. **Update** the `<title>`, the `.eyebrow` text, and the `h1`.
3. **Replace** placeholder content (acme/*, PR #247, "rate limiting"
   strings, sample names) with the user's real topic.
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

### 4. Hand off

Tell the user the path and one of:

- `open ./explainers/<slug>.html` (macOS)
- `xdg-open ./explainers/<slug>.html` (Linux)
- Drag into a browser tab

Do not open a dev server. These are single-file HTML — file:// works.

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

## Anti-patterns (and the fix)

- **Picking a template by aesthetic, not by topic shape.** The reader
  gets a pretty page that doesn't fit the content. → Re-read the
  selection table and pick the closest shape; the styling is identical
  across all 20 anyway.
- **Inventing file paths / line numbers / commit hashes** to fill
  citations the template expects. → Drop the citation region.
- **Skipping the theme overlay** because "it already looks Claude-y".
  The overlay adds the brand badge and the focus ring — keep it.
- **Putting the output inside the toolkit repo.** Always write to the
  user's current working directory under `./explainers/`.

## Output location

- Default: `./explainers/<slug>.html` relative to the user's cwd.
- If `./explainers/` is awkward (e.g. the cwd is read-only), put the
  file in `$CLAUDE_JOB_DIR` (or `/tmp`) and tell the user the
  absolute path.
