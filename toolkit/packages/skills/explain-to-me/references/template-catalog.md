# Template catalog

22 HTML templates: 20 from Thariq Shihipar's "Unreasonable Effectiveness of HTML" set + 2 bespoke (`21-adr.html`, `22-options-paper.html`). All use the same Claude brand palette (ivory / slate / clay / oat / olive), so the choice is about shape, not colour.

## Selection cheat-sheet

★ = visual-first template (diagrams, charts, SVG flows). Prefer ★ when two templates fit.

| Intent the user expressed | Pick | Visual |
|---|---|---|
| "Architecture decision record" / ADR for X | `21-adr.html` | ★ |
| "Options paper" / "trade-off analysis" / "evaluate N options" | `22-options-paper.html` | ★ |
| "Plan to ship feature Z" | `16-implementation-plan.html` | ★ |
| "Diagram a pipeline / flowchart" | `13-flowchart-diagram.html` | ★ |
| "Map this unfamiliar repo / module" | `04-code-understanding.html` | ★ |
| "Inline SVG figures for a blog post" | `10-svg-illustrations.html` | ★ |
| "Teach me concept Y" — pure idea, want to play with it | `15-research-concept-explainer.html` | ★ |
| "Explain how X works" — feature in an existing codebase | `14-research-feature-explainer.html` | |
| "Walk me through this PR" | `03-code-review-pr.html` (reviewer) · `17-pr-writeup.html` (author) | |
| "Compare N code approaches" | `01-exploration-code-approaches.html` | |
| "N visual directions compared" | `02-exploration-visual-designs.html` | ★ |
| "Status update / weekly report" | `11-status-report.html` | ★ |
| "Incident post-mortem" | `12-incident-report.html` | ★ |
| "Component variants / states sheet" | `06-component-variants.html` | ★ |
| "Design system reference" | `05-design-system.html` | ★ |
| "Animation / micro-interaction demo" | `07-prototype-animation.html` | ★ |
| "Clickable multi-screen prototype" | `08-prototype-interaction.html` | |
| "Slide deck (one HTML, no build)" | `09-slide-deck.html` | |
| "Ticket triage / kanban editor" | `18-editor-triage-board.html` | |
| "Feature flag editor with deps" | `19-editor-feature-flags.html` | |
| "Prompt template tuner" | `20-editor-prompt-tuner.html` | |

## Full index

### Decision templates (bespoke)

- **`21-adr.html`** — "ADR-0023 — Move session state to event-sourced store"
  - Status badge (Accepted/Proposed/Superseded), context with forces diagram, 3-column options grid with mini-architecture SVGs + pros/cons + score bars, highlighted decision callout (round seal), large resulting-architecture system diagram, consequences split (positive/negative/neutral), supersession lineage timeline, references.
  - Use for *any decision with options + chosen path + rationale*. Architecture, infra, library choice, design pattern.
  - Augment the resulting-architecture SVG via `/fireworks-tech-graph` when the system is non-trivial.

- **`22-options-paper.html`** — "Real-time delivery for live updates"
  - Draft status badge, problem statement, weighted evaluation criteria with visual weight bars, 4-card options grid with per-criterion score bars and weighted totals, heatmap comparison matrix (criteria × options, color-graded cells), trade-off radar chart (SVG hexagon, 4 overlaid polygons), tentative ranking strip, open-questions cards with owners.
  - Use for *trade-off analysis where the decision hasn't been made yet*. Vendor selection, architecture exploration, infrastructure choices.
  - The radar polygons in the template are illustrative — recompute them from real scores when filling in content. Each axis runs 0–5 from centre to edge.

### Explainers

- **`14-research-feature-explainer.html`** — "How rate limiting works in acme/api"
  - Two-column layout: sticky left nav with section links + cited file paths; right column with TL;DR card, collapsible `<details>` steps, tabbed code blocks, FAQ.
  - Use when the topic is a *concrete feature in a codebase* and you want the reader to jump to file paths.
  - Interactive: tab switching for code samples; details/summary toggles.

- **`15-research-concept-explainer.html`** — "Consistent hashing — an interactive explainer"
  - One-column main + right sidebar glossary. Inline SVG demo with sliders, live-updating readout, hover-linked `<span class="term">` glossary.
  - Use when the topic is *a concept* (algorithm, protocol, idea) where reader benefits from manipulating parameters live.

### Code & repo

- **`04-code-understanding.html`** — "How authentication flows through acme/web"
  - Box-and-arrow module map; hot path highlighted; click a node to reveal source pointer.
  - Use when reader is *new to a codebase* and needs the mental model first. Augment with `/fireworks-tech-graph` for a richer system map.

- **`03-code-review-pr.html`** — diff with margin notes, severity tags, jump links. Reviewer perspective.
- **`17-pr-writeup.html`** — motivation, before/after, file-by-file tour. Author perspective.
- **`01-exploration-code-approaches.html`** — three approaches side-by-side, trade-offs inline. Static.

### Design

- **`02-exploration-visual-designs.html`** — four visual directions for the same empty state. Toggle to compare.
- **`05-design-system.html`** — colour swatches, type scale, spacing tokens. Static reference.
- **`06-component-variants.html`** — every size/state/intent of one component on a single sheet. Toggleable.
- **`07-prototype-animation.html`** — single transition in isolation with duration/easing sliders.
- **`08-prototype-interaction.html`** — four linked screens, click through to feel the flow.

### Reports & timelines

- **`16-implementation-plan.html`** — milestones, data-flow diagrams, mockups, risk matrix. Visual-heavy, long-form.
- **`11-status-report.html`** — shipped/slipped + small chart. Skimmable.
- **`12-incident-report.html`** — minute-by-minute timeline, log excerpts, follow-up checklist.

### Figures & decks

- **`10-svg-illustrations.html`** — inline SVG diagram sheet ready to drop into a blog post.
- **`13-flowchart-diagram.html`** — pipeline drawn as a flowchart; click any step to see what runs. Augment with `/fireworks-tech-graph`.
- **`09-slide-deck.html`** — slides as one HTML file, arrow-key navigation, no build step.

### Editors (interactive tools)

- **`18-editor-triage-board.html`** — drag-and-drop tickets, markdown export.
- **`19-editor-feature-flags.html`** — toggles grouped by area, dependency warnings.
- **`20-editor-prompt-tuner.html`** — editable template + variable slots, samples re-render live.

## Customisation rules

1. **Keep the palette.** All 22 already use Claude's brand tokens. Don't change them unless the user explicitly asks.
2. **Replace placeholder content, not structure.** Strings like "acme/api", "ADR-0023", "PR #247", "rate limiting", scores, weights — swap for the user's real topic.
3. **Update `<title>` and the H1 eyebrow/heading** to match the new topic.
4. **Inject the Claude theme overlay** (`scripts/inject_theme.py`) after rendering.
5. **Strip example file paths and cited fixtures** that don't apply to the new topic. Don't invent fake file paths to fill the nav rail; drop the rail instead.
6. **Preserve all `<script>` blocks** verbatim unless you're changing the demo's data shape. The interactivity is the value.
7. **Recompute the radar polygons** in `22-options-paper.html` from real per-criterion scores. The supplied polygon points are illustrative.
8. **Replace heavy diagrams** (ADR resulting-architecture, flowchart, code-understanding module map) with output from `/fireworks-tech-graph` when the topic warrants it.

## Anti-patterns

- Don't merge multiple templates into one file — pick one.
- Don't strip the inline `<style>` to "modernise" with Tailwind. The templates work *because* the CSS is self-contained.
- Don't add framework dependencies (React, Vue). Single-file HTML is the point.
- Don't replace inline SVG with `<img>`. The SVGs are tweakable on purpose.
- Don't fake scores in `22-options-paper.html`. If you can't score honestly, ask the user.
