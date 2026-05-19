# agents-in-a-box — website design brief

> **For Claude design** (or any human/AI designer briefed off this doc).
> **Output expected:** a complete visual + interaction design for `stevengonsalvez.github.io/agents-in-a-box`. See §12 for deliverable shape.
> **Status:** locked via interview 2026-05-18. Treat decisions in §0 as fixed unless explicitly renegotiated.

---

## 0. Locked decisions (do not relitigate)

| Axis | Value |
|---|---|
| Commercial model | **Open source only** — no auth, no paid tier, no signup |
| Primary audience | **Solo / indie developers** — terminal power-users |
| Visual direction | **Terminal / CRT premium** (Charm-grade polish) |
| Scope | **Marketing landing + full docs hub.** No dashboard yet. |
| Hero demo format | **asciinema recording** (real TUI replay) |
| Docs framework | **Astro Starlight** |
| Logo | **ASCII block art** (existing `AGENTS / IN A / BOX` lockup) |
| Tone | **Hacker-honest blunt.** Drop articles, drop fluff. |
| Primary CTA | **Copyable install command** (`brew install ainb`) |
| Domain | `stevengonsalvez.github.io/agents-in-a-box` (GitHub Pages) |
| Palette | **Pure TUI** — `#191923` bg, `#6495ED` border, `#FFD700` gold, `#DCDCE6` text |
| Reference | **charm.sh** — match polish, deviate via pure-ASCII identity |
| Flourishes | Scanlines + blinking cursor + boot sequence + vim keybinds + ASCII dividers (all behind `prefers-reduced-motion` opt-in) |

---

## 1. Product (what is agents-in-a-box)

A **terminal-native ecosystem for managing AI coding agents**. Three components, one monorepo:

1. **`ainb` — a Rust TUI + CLI.** Spawns and supervises AI coding sessions (Claude Code, Codex, Gemini, Copilot, Kiro, raw shell, SSH). Every session gets a dedicated git worktree and a persistent tmux session. Built-in burndown analytics, usage tracking, swarm orchestration.

2. **Toolkit — portable skills, agents, workflows.** 86 skills and 37 specialised agents written once, deployable to 9 different AI coding tools (Claude Code, Codex, Copilot, Gemini, Hermes, nanoclaw, Amazon Q, Cursor, Cline, Roo, Clawdhub). The bootstrap engine handles per-tool packaging.

3. **Plugin system (v2 subprocess ABI).** The TUI host loads native-binary plugins over framed JSON-RPC/stdio. Plugins own screens, CLI subcommands, snapshot publishers, statusline segments. Capability-gated (deny-by-default). Two reference plugins shipped: `burndown` (analytics) + `session-reader` (data backend).

A fourth, supporting component:

4. **Knowledge system (`reflect` / `recall`).** Two-tier learning capture and retrieval — fast QMD vector search + nano-graphrag entity graph. The `reflect` CLI lives in `reflect-kb/` and is installed via `uv tool install`.

The monorepo also hosts a **Claude Code plugin** (`plugins/reflect/`) distributed via `.claude-plugin/marketplace.json` — distinct from ainb v2 plugins; same name, different runtime. The website **must** disambiguate.

---

## 2. Audience

**Primary:** solo / indie developers running personal AI coding workflows. Already comfortable in tmux + Neovim + the terminal. Treats their dev environment as a craft. Skims, doesn't read. Suspicious of marketing pages with stock photography.

**Implicit secondary:** small engineering teams. Not the marketing target; they self-select in from the docs.

**Anti-audience:** non-technical buyers. The site should make no concessions to non-developers.

---

## 3. Value proposition

**One-liner (hero):**
> Run agents. Worktree per session. No bullshit.

**Three-line (sub-hero):**
> Terminal-native ecosystem for managing AI coding agents.
> One TUI. Every model. Isolation by default.
> Open source. Built in Rust. Installs in a single command.

**Differentiators (use these to fill the three-column value-prop row):**

| Column | Headline | Body |
|---|---|---|
| 1 | **TUI built for power-users** | 115 modules of typed, tested, async Rust. Vim-style nav, tmux-persistent sessions, git-worktree isolation. Multi-provider: Claude · Codex · Gemini · Copilot. |
| 2 | **Toolkit that deploys everywhere** | 86 skills, 37 agents. Write once, deploy to 9 AI tools. Bootstrap engine handles per-tool packaging. |
| 3 | **Plugins that compose** | Native-binary plugins over JSON-RPC. Capability-gated. Reference plugins ship in-tree. Authoring docs included. |

---

## 4. Voice & tone

**Hacker-honest blunt.** Read like a competent operator wrote it for other competent operators. No salesy adjectives, no "delightful experiences", no "empower developers".

**Mechanics:**
- Drop articles where idiomatic (`> Run agents.` not `> Run your agents.`)
- Short sentences. Full stops do the work.
- Numbers, not adjectives (`115 modules`, not `extensive`).
- Address the reader as `you` only when it adds information.
- Imperative mood for CTAs (`Install.`, `Read the docs.`, not `Get started today!`)
- Allow dry humour. Forbid emoji except in the rare technical context (file-tree icons OK; sparkles never).

**Copy bank — feel free to lift verbatim or remix:**

```
HERO:
  Run agents.
  Worktree per session.
  No cross-contamination.
  No bullshit.

INSTALL CTA:
  $ brew install ainb
  $ ainb

  That's it.

THREE-COL HEADERS:
  > Built for the terminal.
  > Built once. Deployed everywhere.
  > Built to compose.

PLUGIN TEASER:
  Native binaries. JSON-RPC over stdio.
  Deny-by-default capabilities.
  Your plugin can't reach the network unless you say it can.

KNOWLEDGE TEASER:
  Capture what you learn.
  Retrieve it across sessions.
  GraphRAG underneath, /reflect on top.

FINAL CTA:
  Open source. MIT.
  Install in a single command.
  $ brew install ainb
  Or read the docs first.

ERROR PAGES (404):
  > segfault: 404
  > the page you wanted is not in this worktree.
  > [ go home ]   [ docs ]
```

**Never write:**
- "Revolutionise your workflow"
- "Game-changing AI"
- "Delightful developer experience"
- "Empower your team"
- "Built with love by Steven"
- Emoji-as-bullet (✨ 🚀 💡)

---

## 5. Visual direction

### 5.1 Mood

**CRT premium.** Treat the site like a darkened terminal pane that someone with taste has polished. Sharp typographic hierarchy. Generous negative space. Heavy use of monospace. Box-drawing characters as structural elements (not decoration).

**Reference site:** [`charm.sh`](https://charm.sh). Match the polish and pacing; **diverge** by:
- Going further into ASCII as a structural element (Charm uses some ASCII but leans gum-gloss for marketing copy).
- Locking to the pure TUI palette (Charm uses pink/teal accents; we stay cornflower + gold).
- Slightly grittier — subtle scanline overlay, blinking cursors, faint CRT halation on hero.

### 5.2 Palette (locked)

```
BG_DEEP       #0F0F18     (page background, deepest)
BG_PANEL      #191923     (cards, modal surfaces — matches TUI DARK_BG)
BG_RAISED     #1F1F2D     (hover state, raised surfaces)
BG_HIGHLIGHT  #28283C      (selected rows — matches TUI LIST_HIGHLIGHT_BG)

BORDER        #6495ED     (cornflower — primary border, links, focus rings)
ACCENT_GOLD   #FFD700     (titles, CTAs, hero accents)
ACCENT_GREEN  #64C864     (success / selection — matches TUI SELECTION_GREEN)

TEXT_PRIMARY  #DCDCDE6    (body)
TEXT_MUTED    #78788C     (secondary, captions, code dimmers)
TEXT_FAINT    #4A4A5C     (disabled / scanline base)

SYNTAX_KEYWORD #FFD700
SYNTAX_STRING  #64C864
SYNTAX_COMMENT #78788C
SYNTAX_NUMBER  #6495ED
```

No light mode in v1. (Defer to a future iteration.)

### 5.3 Typography

**Mono everything by default.** Two faces:

| Use | Font | Notes |
|---|---|---|
| Body, headings, code | **JetBrains Mono** | Variable. Use weights 400 / 500 / 700. Ligatures on for code blocks, OFF for prose (the `=>` and `==` ligatures look weird in headings). |
| Display / hero / large numbers | **IBM Plex Mono** | Heavier presence at large sizes. Weight 700. Used only for hero ASCII block + section labels. |

Fallback stack: `'JetBrains Mono', 'IBM Plex Mono', 'SF Mono', 'Menlo', monospace`.

**No sans-serif body font.** Resist the temptation; it breaks the identity.

**Scale (rem, mobile-first):**
```
h1 / hero       4.0 → 6.0     (responsive clamp; ASCII art replaces at >900px)
h2              2.25
h3              1.5
h4              1.125
body            0.9375        (0.9375rem = 15px at 16-base; reads tight, intentional)
caption / meta  0.8125
code            0.875
```

Line-height: 1.45 for body, 1.2 for headings, 1.1 for ASCII blocks.

### 5.4 Logo / brand mark

**ASCII block art** — the existing lockup from the README. Use it at **hero** scale and only there. Everywhere else (nav, footer, favicon), use the plain wordmark `ainb` or `agents-in-a-box` in Plex Mono 700.

```
   █████╗  ██████╗ ███████╗███╗   ██╗████████╗███████╗
  ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝██╔════╝
  ███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   ███████╗
  ██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   ╚════██║
  ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   ███████║
  ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝   ╚══════╝
            IN  A  BOX
```

Render as a `<pre>` with `aria-label="agents-in-a-box"`. At <900px viewport, swap for the wordmark + tagline. Never render as an image (kills accessibility + crispness).

### 5.5 Spacing & rhythm

- 8px base grid.
- Section vertical rhythm: 96px desktop, 64px mobile.
- Content max-width: 1100px on landing, 760px in docs body.
- Generous whitespace; never crowd. CRT premium ≠ retro-clutter.

### 5.6 Box-drawing as structural element

ASCII rules between sections:

```
═══════════════════════════════════════════════════════════
```

```
┌──────────────────────────────────────────────────────────┐
│  Section content here.                                   │
└──────────────────────────────────────────────────────────┘
```

```
                         » » »
```

Use sparingly — one rule per section transition is plenty. Don't pad every paragraph.

---

## 6. Site map

```
/                              landing page
/docs/                         docs hub (Starlight)
  ├── getting-started/
  │   ├── install.md
  │   ├── first-session.md
  │   └── quickstart.md
  ├── tui/
  │   ├── overview.md
  │   ├── cli.md                  ← full CLI reference (was ainb-tui/docs/CLI.md)
  │   ├── keyboard-shortcuts.md
  │   ├── architecture.md
  │   └── faq.md
  ├── toolkit/
  │   ├── overview.md             ← was toolkit/README.md
  │   ├── skills.md
  │   ├── agents.md
  │   └── bootstrap.md
  ├── plugins/                    ← v2 subprocess plugin system
  │   ├── overview.md             ← what is a plugin; disambiguates Claude-Code plugins
  │   ├── user-guide.md           ← was docs/plugins.md
  │   ├── authoring.md            ← was docs/plugin-authoring.md
  │   ├── spec-v2.md              ← was docs/plugin-spec/v2.md
  │   └── changelog.md
  ├── knowledge/
  │   ├── overview.md             ← was docs/how-reflection-works.md
  │   └── reflect-cli.md
  ├── contributing/
  │   ├── building.md
  │   ├── ci-cd.md
  │   └── release-process.md
  └── reference/
      ├── architecture.md         ← whole-monorepo box diagram
      └── glossary.md
/changelog/                    flat list, auto-generated from CHANGELOG.md
/404                           themed 404 page
```

**Routing rules:**
- Landing is its own custom page (not a Starlight doc) — full bleed, dark BG, hero asciinema, marketing-grade.
- `/docs/*` lives inside Starlight with its default left-sidebar nav + on-page TOC.
- Top nav (everywhere): `Docs · Plugins · Toolkit · GitHub` — gold on hover, cornflower-underline on focus.

---

## 7. Page-by-page brief

### 7.1 Landing (`/`)

Sections in order, top to bottom:

#### A. Boot sequence + hero (full viewport)

On first visit only (sessionStorage'd):

```
[ booting agents-in-a-box ]
[OK] tmux session manager
[OK] git worktree isolation
[OK] tokio runtime
[OK] 115 modules loaded
[OK] 86 skills registered
[OK] 37 agents online
[OK] 2 plugins discovered
[ ready ]
```

~1.4s total. Skip button (top-right, plain text, `[ skip ▸ ]`) sets the sessionStorage flag and reveals the hero immediately. **Respect `prefers-reduced-motion`** — skip the boot sequence entirely on reduced-motion.

Hero settles into:

```
   █████╗  ██████╗ ███████╗███╗   ██╗████████╗███████╗
  ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝██╔════╝
  ███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   ███████╗
  ██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   ╚════██║
  ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   ███████║
  ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝   ╚══════╝
            IN  A  BOX

  Run agents. Worktree per session. No bullshit.

  $ brew install ainb_
  ───────────────────────────────────────────────────

  [ Read the docs → ]    [ ★ Star on GitHub ]
```

- Underscore in `ainb_` is the **blinking block cursor**.
- The `$ brew install ainb` block is **click-to-copy** with a brief gold flash on copy.
- Beneath: an **asciinema player** showing a real recorded TUI session (~30s loop, autoplay muted, no controls visible by default — hover reveals scrubber). Width ~900px, centred.

#### B. Three-column value prop

```
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  TUI         │  │  TOOLKIT     │  │  PLUGINS     │
│              │  │              │  │              │
│  115 mods    │  │  86 skills   │  │  v2 ABI      │
│  4 providers │  │  9 tools     │  │  JSON-RPC    │
│  tmux-bound  │  │  37 agents   │  │  cap-gated   │
│              │  │              │  │              │
│  [ docs → ]  │  │  [ docs → ]  │  │  [ docs → ]  │
└──────────────┘  └──────────────┘  └──────────────┘
```

Each card is a `<a>` to the relevant `/docs/<slug>/overview` page. Hover: BG goes from `#191923` → `#1F1F2D`, border goes from `#6495ED` (60% opacity) → `#6495ED` (100%).

#### C. Feature showcase (screenshots)

Three rows × two columns (six tiles total). Reuse the existing screenshots from `docs/assets/screenshots/`:

| Row | Left | Right |
|---|---|---|
| 1 | `home.png` — Unified dashboard | `agent-picker.png` — Agent + model picker |
| 2 | `new-session.png` — Start a session | `setup.png` — Guided setup wizard |
| 3 | `burndown.png` — Burndown analytics | `stats-projects.png` — Per-project attribution |

Each tile: thumbnail (clickable to lightbox), gold bold caption, single-line muted description. Match existing README copy.

#### D. Plugin ecosystem teaser

```
═══════════════════════════════════════════════════════════
                  Plugins, the right way.
═══════════════════════════════════════════════════════════

  Native binaries. JSON-RPC over stdio. Deny-by-default
  capabilities. Your plugin can't reach the network
  unless you say it can.

  Two reference plugins ship in-tree:
  ▸ burndown        — full analytics screen + `ainb usage` CLI
  ▸ session-reader  — pure-publisher data backend

  [ Plugin user guide → ]    [ Authoring guide → ]    [ v2 spec → ]
```

Beneath: a code excerpt from a `manifest.toml`, syntax-highlighted, copy-button on hover.

#### E. Toolkit grid (9 tools)

Box-drawn grid of supported AI tools:

```
┌─────────────────┬─────────────────┬─────────────────┐
│  Claude Code    │  Codex          │  Copilot        │
├─────────────────┼─────────────────┼─────────────────┤
│  Gemini CLI     │  Amazon Q       │  Cursor         │
├─────────────────┼─────────────────┼─────────────────┤
│  Cline          │  Roo            │  Clawdhub       │
└─────────────────┴─────────────────┴─────────────────┘
```

Each cell: tool name in gold-bold, install command in muted code (`node bootstrap.js --tool=<key>`), link to deep-dive in `/docs/toolkit/`.

#### F. Architecture diagram

A single ASCII box diagram:

```
┌─────────────────────────────────────────────────────────┐
│                     ainb TUI host                       │
│   ┌──────────┐  ┌──────────┐  ┌────────────────────┐   │
│   │ tmux/PTY │  │ git/wkt  │  │ plugin runtime     │   │
│   └────┬─────┘  └────┬─────┘  └──────┬─────────────┘   │
│        │             │               │                 │
└────────┼─────────────┼───────────────┼─────────────────┘
         │             │               │ JSON-RPC/stdio
         ▼             ▼               ▼
   ┌──────────┐ ┌─────────────┐ ┌──────────────────────┐
   │  Claude  │ │  worktrees  │ │  burndown · session- │
   │  Codex   │ │  per session│ │  reader · yours      │
   │  Gemini  │ └─────────────┘ └──────────────────────┘
   │  Copilot │
   └──────────┘

           ┌────────────────────────────────────┐
           │  Toolkit (86 skills · 37 agents)   │
           │  Deploys to 9 AI coding tools      │
           └────────────────────────────────────┘

           ┌────────────────────────────────────┐
           │  Knowledge: reflect-kb (GraphRAG)  │
           │  /reflect  /recall  /ingest        │
           └────────────────────────────────────┘
```

Render as `<pre>`. Each labelled box is a `<a>` link to its docs section (use CSS to add an underline-on-hover on the substring).

#### G. Knowledge system pitch

```
                         » » »

  Capture what you learn.
  Retrieve it across sessions.

  GraphRAG underneath. /reflect on top.
  170+ learnings indexed and growing.

  [ How it works → ]
```

#### H. Stats counter (CRT flicker)

A horizontal row of large numbers with flicker animation (faint opacity wobble between 0.95–1.0 at irregular intervals, ~0.3Hz; respects `prefers-reduced-motion`).

```
  115            86             37            2
  modules        skills         agents        plugins
```

Numbers are live — read from a small JSON file in the repo (`stats.json`) that gets updated by a CI job. v1 can ship with hardcoded numbers.

#### I. Final CTA strip

```
═══════════════════════════════════════════════════════════

  Open source. MIT.
  Installs in a single command.

  $ brew install ainb
  ──────────────────────────────

  [ Read the docs → ]   [ ★ GitHub ]   [ Releases ]

═══════════════════════════════════════════════════════════
```

#### J. Footer

Three columns: **Docs** / **Project** / **Maintainer**. Plain text links. Gold on hover, no underline. Plus a sub-footer with `MIT · 2024–2026 · stevengonsalvez` and the blinking cursor easter egg.

### 7.2 Docs hub (`/docs/`)

Starlight defaults with custom theme:
- Left sidebar: tree nav matching §6 site map. Section headers in gold-bold.
- Top bar: search (Pagefind), GitHub link, version dropdown (initially just `latest`).
- Body: 760px max-width, JetBrains Mono throughout, code blocks with cornflower-bordered top-bar showing language + copy button.
- Right rail: on-page TOC, "Edit this page on GitHub" link at bottom.
- Footer per page: `← Previous · Next →` nav.

### 7.3 Individual doc pages

Markdown rendering rules:
- `# h1` → gold, no underline.
- `## h2` → cornflower, with a 1px cornflower bottom border 25% width.
- Inline `code` → gold on `#28283C` background, 2px padding, 4px border-radius.
- Code blocks → `#0F0F18` background, cornflower 1px border, top bar (`#191923`) with language label in muted text + copy button in cornflower.
- Tables → cornflower header row (text gold-bold), zebra-striped body (`#191923` / `#1F1F2D`), cornflower outer border.
- Blockquotes → cornflower left border 3px, body text in muted-gray italic.
- Admonitions (`> [!note]`, `> [!warning]`, `> [!tip]`) → boxed, icon-less (use `[NOTE]`, `[WARNING]`, `[TIP]` as gold-bold labels in the top-left).

### 7.4 404 page

```
                  ╔════════════════════╗
                  ║                    ║
                  ║  segfault: 404     ║
                  ║                    ║
                  ╚════════════════════╝

   The page you wanted is not in this worktree.

   $ ainb recover list_

   [ go home ]   [ docs ]   [ github ]
```

---

## 8. Interactive flourishes

All of the below MUST respect `prefers-reduced-motion: reduce` — disable animations, instantly render final state.

### 8.1 Scanlines overlay

A fixed-position `<div>` overlaid on the entire page at `pointer-events: none; z-index: 1000`. Linear-gradient stripe pattern, ~1% opacity, 2px line every 4px:

```css
background-image: repeating-linear-gradient(
  to bottom,
  rgba(220, 220, 230, 0.015) 0,
  rgba(220, 220, 230, 0.015) 1px,
  transparent 1px,
  transparent 4px
);
```

Toggle in settings (saved to localStorage). Default: ON on dark, OFF if reduced-motion.

### 8.2 Blinking block cursor

Pure CSS, no JS. After hero install command, after the wordmark in the footer, after `404` heading. 1.0Hz, square wave (not sine — keep it CRT-honest):

```css
@keyframes blink { 0%, 49% { opacity: 1; } 50%, 100% { opacity: 0; } }
.cursor { animation: blink 1s steps(2) infinite; }
```

### 8.3 Boot sequence (first visit only)

Plain JS, sessionStorage-gated. Spawns lines one at a time with a 60ms stagger. Skip button visible from frame 1. **Hard cap: 1.6s total** including final fade-in. After completion, the hero is fully interactive.

Lines (final):
```
[ booting agents-in-a-box ]
[OK] tmux session manager
[OK] git worktree isolation
[OK] tokio runtime
[OK] 115 modules loaded
[OK] 86 skills registered
[OK] 37 agents online
[OK] 2 plugins discovered
[ ready ]
```

Repeat visitor: skip entirely, render hero directly.

### 8.4 Vim keybinds (everywhere)

Global JS handler with progressive enhancement:

| Key | Action |
|---|---|
| `j` / `↓` | scroll down one section |
| `k` / `↑` | scroll up one section |
| `gg` | scroll to top |
| `G` | scroll to bottom |
| `/` | focus the docs search input |
| `?` | toggle the keybind help overlay |
| `Esc` | close the help overlay / dismiss modals |
| `gh` | navigate to GitHub |
| `gd` | navigate to `/docs/` |

The `?` help overlay is a centred modal with a box-drawn border showing all bindings. Plain text, no graphics.

Don't capture these when an `<input>` / `<textarea>` is focused. Document them visibly in a footer microcopy line: `? help · j/k scroll · / search`.

### 8.5 ASCII dividers

Use `═══...═══`, `─── » » » ───`, and `┌─...─┐ │ └─...─┘` as section breaks. CSS-rendered for consistent length; use `::before` / `::after` pseudo-elements or `<hr>` styled with `border` + custom content.

---

## 9. Tech stack constraints

| Concern | Choice | Notes |
|---|---|---|
| Hosting | **GitHub Pages** | Free, fast. Build via GitHub Action. |
| Framework | **Astro** | Static-site, zero-JS by default. Islands for the boot sequence, vim handler, asciinema player. |
| Docs UI | **Starlight** (`@astrojs/starlight`) | Use the Astro Starlight base theme, override CSS variables for the palette, supply custom components for code blocks. |
| Search | **Pagefind** (Starlight default) | Static, ~120kb runtime, no server. |
| Asciinema | **`asciinema-player` v3** | Bundle as a self-hosted island; load only on the landing page. |
| Type system | TypeScript everywhere | Strict mode. |
| Lint | `eslint` + `prettier` | Defaults. |
| CI | GitHub Actions | `lint → build → deploy` on push to `main`. |
| Analytics | **None in v1.** | Add Plausible (self-hosted) later if needed. No GA. |

**Bundle budget:**
- Landing page LCP < 1.5s on Fast 3G simulated, < 500ms on broadband.
- JS budget: < 60kb compressed (excluding asciinema-player, which is ~30kb).
- Image budget: every screenshot in `webp` + `avif`, lazy-loaded below the fold.

---

## 10. Asset list

| Asset | Source | Notes |
|---|---|---|
| Hero asciinema cast | Record in worktree | ~30s loop: `ainb` launch → spawn session → switch to burndown → exit. No real keystrokes typed; use `asciinema rec` with pre-baked input. |
| Screenshots (×6) | `docs/assets/screenshots/` | Already exist: `home.png`, `agent-picker.png`, `new-session.png`, `setup.png`, `burndown.png`, `stats-projects.png`. Generate `webp` + `avif` derivatives. |
| ASCII hero block | Reuse from README | Render as `<pre>` with `aria-label`. |
| Favicon | New asset | 32×32 + 180×180 + SVG. Black background, gold `[ainb]` wordmark or a small `▮` cursor. |
| OG image | New asset | 1200×630. ASCII hero on dark bg, palette colours visible. |
| Stats JSON | `website/public/stats.json` | Hardcoded for v1 (`{"modules":115,"skills":86,"agents":37,"plugins":2}`). Wire to CI later. |
| `manifest.toml` excerpt | Pull from `docs/plugin-spec/v2.md` | Used in plugin teaser section. |

---

## 11. Accessibility

Non-negotiable:
- **Contrast.** Body text `#DCDCE6` on `#0F0F18` = 14.7:1 (AAA). Muted `#78788C` on `#191923` = 4.6:1 (AA for body, AAA-fail — never use muted on body, only metadata).
- **`prefers-reduced-motion: reduce`** disables: boot sequence, scanlines, blinking cursor, stats flicker, asciinema autoplay.
- **`prefers-contrast: more`** raises body to `#FFFFFF` and bumps muted to `#A0A0B0`.
- **Keyboard nav.** Tab order matches visual order. Focus rings: 2px solid cornflower with 2px offset.
- **Skip link.** "Skip to content" at the very top of `<body>`, visually hidden until focused.
- **ARIA.** All `<pre>` ASCII art carries an `aria-label` summarising what it depicts. `<button>` for copy buttons, never `<div>`.
- **Reading order.** Test with VoiceOver and NVDA. Boot sequence is `aria-hidden`.
- **No layout shift.** Reserve space for the asciinema player + boot sequence so CLS = 0.

---

## 12. Deliverables expected from Claude design

Produce, in this order:

1. **`design.md`** — a short design rationale doc explaining the colour, type, and layout choices in your own words. (Sanity check the brief was understood.)
2. **Hero mock** — full-page, full-fidelity, **as HTML+CSS** (not Figma) at 1440×1024 + 375×812. Static HTML file viewable in a browser. Include the boot-sequence frame and the settled hero frame.
3. **Landing mock** — full long-page HTML+CSS, scrollable, covering all sections A–J.
4. **Docs mock** — one example doc page (use the future `/docs/plugins/overview` content) rendered through the Starlight-themed CSS. HTML+CSS file.
5. **Component palette** — a single HTML page showing every reusable component (buttons, code blocks, tables, callouts, cards) in all states (default / hover / focus / disabled).
6. **404 mock** — HTML+CSS file.
7. **CSS variables file** — `tokens.css` with the full palette + typography scale + spacing scale as CSS custom properties, ready to drop into Astro.
8. **A short walkthrough of any place you intentionally deviated** from this brief, with reasoning.

Do **not** produce Figma files. We want shippable HTML/CSS from the start so the design doubles as the implementation starting point.

---

## 13. Open questions for the designer

Flag these in your `design.md` if you have opinions:

1. Hero asciinema width — fixed 900px or fluid clamp?
2. Whether to add a single subtle accent illustration anywhere (an ASCII rendering of the burndown chart? The TUI sidebar?) or stay strictly typographic.
3. Whether the boot sequence should reveal lines one-at-a-time or all-at-once with a stagger fade.
4. The favicon direction (`▮` vs `[ainb]` vs custom).
5. Whether the docs left-sidebar should have collapsible groups or always-expanded.

---

## 14. What's not in scope (v1)

- Dashboard / authed surface.
- Light mode.
- Blog. (Use the changelog as a stand-in.)
- Plugin marketplace UI. (Defer until ainb-host install flow re-lands.)
- i18n.
- Custom domain (we're on `github.io`).
- A web-playground that runs ainb in-browser.
- Analytics tracking.

---

## 15. Reference & inspiration

**Match the polish of:**
- [`charm.sh`](https://charm.sh) — closest analogue for stack + vibe
- [`bun.sh`](https://bun.sh) — install-first hero pattern
- [`astro.build`](https://astro.build) — docs framework reference
- [`ghostty.org`](https://ghostty.org) — terminal-tool premium

**Avoid the patterns of:**
- Hero illustrations with cartoon mascots.
- Pricing tables. (We have no pricing.)
- "Trusted by" logo bars with fake-looking enterprise logos.
- Modal popups for newsletter signup.
- Cookie banners that aren't legally required.

---

**End of brief.** Questions → open an issue on the repo or DM Stevie.
