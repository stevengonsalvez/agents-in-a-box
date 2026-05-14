# Template catalog

20 HTML templates from Thariq Shihipar's "Unreasonable Effectiveness of HTML" set. All use the same Claude brand palette (ivory / slate / clay / oat / olive), so the choice is purely about shape, not colour.

## Selection cheat-sheet

| Intent the user expressed | Pick |
|---|---|
| "Explain how X works" — feature in an existing codebase | `14-research-feature-explainer.html` |
| "Teach me concept Y" — pure idea, want to play with it | `15-research-concept-explainer.html` |
| "Map this unfamiliar repo / module" | `04-code-understanding.html` |
| "Walk me through this PR" | `03-code-review-pr.html` (reviewer view) or `17-pr-writeup.html` (author writeup) |
| "Compare N approaches / N designs" | `01-exploration-code-approaches.html` or `02-exploration-visual-designs.html` |
| "Plan to ship feature Z" | `16-implementation-plan.html` |
| "Status update / weekly report" | `11-status-report.html` |
| "Incident post-mortem" | `12-incident-report.html` |
| "Diagram a pipeline / flowchart" | `13-flowchart-diagram.html` |
| "Component variants / states sheet" | `06-component-variants.html` |
| "Design system reference" | `05-design-system.html` |
| "Animation / micro-interaction demo" | `07-prototype-animation.html` |
| "Clickable multi-screen prototype" | `08-prototype-interaction.html` |
| "Slide deck (one HTML, no build)" | `09-slide-deck.html` |
| "Inline SVG figures for a blog post" | `10-svg-illustrations.html` |
| "Ticket triage / kanban editor" | `18-editor-triage-board.html` |
| "Feature flag editor with deps" | `19-editor-feature-flags.html` |
| "Prompt template tuner" | `20-editor-prompt-tuner.html` |

## Full index

### Explainers (the heart of `/explain-to-me`)

- **`14-research-feature-explainer.html`** — "How rate limiting works in acme/api"
  - Two-column layout: sticky left nav with section links + cited file paths; right column with TL;DR card, collapsible `<details>` steps, tabbed code blocks, FAQ.
  - Use when the topic is a *concrete feature in a codebase* and you want the reader to be able to jump to file paths.
  - Interactive: tab switching for code samples; details/summary toggles. No external JS deps.

- **`15-research-concept-explainer.html`** — "Consistent hashing — an interactive explainer"
  - One-column main + right sidebar glossary. Inline SVG demo with sliders (duration, nodes), live-updating readout, hover-linked `<span class="term">` glossary.
  - Use when the topic is *a concept* (algorithm, protocol, idea) where reader benefits from manipulating parameters live.
  - Interactive: SVG that animates from slider input; glossary terms highlight on hover.

### Code & repo

- **`04-code-understanding.html`** — "How authentication flows through acme/web"
  - Box-and-arrow module map; hot path highlighted; click a node to reveal source pointer.
  - Use when reader is *new to a codebase* and needs the mental model first.

- **`03-code-review-pr.html`** — "PR #247 — Review Summary"
  - Diff rendered with margin notes, severity tags (blocker / nit / question), jump links.
  - Use for *reviewer's perspective*.

- **`17-pr-writeup.html`** — "PR #312 — Move notification delivery onto a queue"
  - Motivation, before/after, file-by-file tour. Static.
  - Use for *author's perspective* — convince reviewer this PR is ready.

- **`01-exploration-code-approaches.html`** — three approaches side-by-side, trade-offs inline. Static.

### Design

- **`02-exploration-visual-designs.html`** — four visual directions for the same empty state. Toggle to compare.
- **`05-design-system.html`** — colour swatches, type scale, spacing tokens pulled from a repo. Static reference.
- **`06-component-variants.html`** — every size/state/intent of one component on a single sheet. Toggleable.
- **`07-prototype-animation.html`** — single transition in isolation with duration/easing sliders.
- **`08-prototype-interaction.html`** — four linked screens, click through to feel the flow.

### Reports & timelines

- **`16-implementation-plan.html`** — milestones, data-flow diagrams, mockups, risk matrix. Static, long-form.
- **`11-status-report.html`** — what shipped / slipped + a small chart. Skimmable.
- **`12-incident-report.html`** — minute-by-minute timeline, log excerpts, follow-up checklist. Static.

### Figures & decks

- **`10-svg-illustrations.html`** — sheet of inline SVG diagrams ready to drop into a blog post.
- **`13-flowchart-diagram.html`** — pipeline drawn as a real flowchart; click any step to see what runs.
- **`09-slide-deck.html`** — slides as one HTML file, arrow-key navigation, no build step.

### Editors (interactive tools)

- **`18-editor-triage-board.html`** — drag-and-drop tickets, markdown export.
- **`19-editor-feature-flags.html`** — toggles grouped by area, dependency warnings.
- **`20-editor-prompt-tuner.html`** — editable template + variable slots, samples re-render live.

## Customisation rules

1. **Keep the palette.** All 20 already use Claude's brand tokens. Don't change them unless the user asks.
2. **Replace placeholder content, not structure.** Strings like "acme/api", "PR #247", "rate limiting" are placeholders — swap them for the user's topic.
3. **Update `<title>` and the H1 eyebrow/heading** to match the new topic.
4. **Inject the Claude theme overlay** (`../claude-theme.css` contents) as a second `<style>` tag right after the existing `<style>` block. This adds the brand badge and tightens typography without disturbing per-template layout.
5. **Strip example file paths and cited fixtures** that don't apply to the new topic. Don't invent fake file paths to fill the nav — drop the nav file list instead.
6. **Preserve all `<script>` blocks** verbatim unless you're changing the demo's data shape. The interactivity is the value.

## Anti-patterns

- Don't merge multiple templates into one file — pick one.
- Don't strip the inline `<style>` to "modernise" with Tailwind. The templates work *because* the CSS is self-contained.
- Don't add framework dependencies (React, Vue). Single-file HTML is the point.
- Don't replace inline SVG with `<img>`. The SVGs are tweakable on purpose.
