# Multica UX & Design Analysis

**Purpose**: Teardown of Multica's full UX surface to inform ainb design direction.
**Date**: 2026-05-22
**Sources read**: README, CLAUDE.md, `packages/ui/`, `packages/views/`, `server/cmd/multica/`, `apps/desktop/`, `apps/mobile/`, marketing site.

---

## 1. First-Run / Onboarding UX

### Install path

Three install methods, ordered by friction:

```
brew install multica-ai/tap/multica    ← zero friction
curl ... | bash                        ← moderate
irm ... | iex  (Windows PowerShell)   ← explicit platform
```

After install, one command does everything:

```
multica setup
```

The setup wizard runs these steps in sequence:
1. Detects existing config, confirms reset if present
2. Writes `server_url` / `app_url` to config file, prints path
3. (Self-host only) probes `/health` endpoint with 2s timeout
4. Launches browser-based OAuth login
5. Saves token, starts daemon
6. Emits: `"✓ Setup complete! Your machine is now connected to Multica."`

Notably: no bubbletea/charm UI library. The CLI uses Cobra + `text/tabwriter` + `fmt.Printf`. Output is plain text with Unicode checkmarks (`✓`) and warning symbols (`⚠`). Zero color codes in standard output.

### Web onboarding flow (6 steps)

```
┌──────────┐   ┌────────┐   ┌──────┐   ┌──────────┐   ┌───────────┐   ┌─────────┐
│ Welcome  │──▶│ Source │──▶│ Role │──▶│ Use Case │──▶│ Workspace │──▶│ Runtime │
└──────────┘   └────────┘   └──────┘   └──────────┘   └───────────┘   └─────────┘
  not persisted   persisted   persisted   persisted       creation       CLI detect
```

Welcome screen: two-column editorial hero, serif 5xl/6xl headline with one word in brand color italics, mock stacked card illustration (cards with slight rotations: `-translate-x-5 -rotate-[1.2deg]`). Uses `animate-onboarding-enter` (opacity-only, 0.4s). Three CTAs: primary "Start Exploring", secondary "Download Desktop", ghost "Skip to Existing".

Runtime step: polls every 2s, 5-second timeout before showing empty state with skip option. WebSocket `daemon:register` event invalidates immediately on connection. Shows skeleton cards during scan, then runtime provider logo + online/offline badge when found.

### Desktop onboarding divergence

Desktop skips the CLI install instructions step (has bundled daemon). Pre-workspace flows are WindowOverlay state, not routes — `push('/workspaces/new')` translates to an overlay, not a page navigation.

### First task (step 4 in Getting Started)

```
Create issue (board or CLI) → assignee picker shows agents + humans →
agent auto-picks up → daemon spawns CLI → WebSocket streams progress to UI
```

---

## 2. Primary User Journeys

### Journey 1: Assign a task to an agent

**Path**: Issues board → create card → open assignee picker → select agent avatar → save

The assignee picker is polymorphic: `assignee_type` (member|agent) + `assignee_id`. Agents render with distinct styling from CLAUDE.md: "purple background, robot icon." Once assigned, the `AgentLiveCard` appears as a sticky frosted-glass banner at the top of the issue detail:

```
┌─────────────────────────────────────────────────────────────────┐
│  bg-background/55  backdrop-blur-md   sticky top-4  z-10        │
│  [avatar] alice-agent  is working   7m 17s  10 tool calls  [X]  │
└─────────────────────────────────────────────────────────────────┘
```

Banner updates via WebSocket `task:message` events. Shows `Loader2 animate-spin` for running, `Clock` static for queued.

### Journey 2: Monitor a running agent

**Path**: Sidebar → Agents → row with sparkline → click row → Agent Detail (Activity tab default)

The Activity tab is the default because "most visits to this page are diagnostic." The agent detail page uses a responsive 2-column grid: 320px inspector left, flexible overview pane right.

The `AgentPresenceIndicator` shows 3-state dot: online (brand), unstable (amber — only if runtime is offline; if online, brief queuing is muted gray), offline (muted). Workload chip shows "working X tasks / queued Y" or "idle."

`AgentLivePeekCard` on hover shows three live signals: workload, current issue (lazy-loaded identifier), last terminal task timestamp.

### Journey 3: Review an agent's PR

**Path**: Issue detail → sidebar "Pull Requests" section (conditional, GitHub-integrated) → external link or inline list

The issue detail sidebar is progressively disclosed: core properties always visible (status, assignee, project), optional ones (priority, dates, labels) behind "+ Add property" popover. PR section only appears when `has_prs`. Comments from agents in the activity feed are delete-only (no edit offered — "agents own their own outputs").

The execution log section shows past runs sorted: failed first (needs attention), cancelled, completed. Hover reveals action buttons (transcript, cancel, retry) via absolute-positioned overlay with left-fading gradient backdrop so underlying status text is dimmed not cut.

### Journey 4: Build and publish a skill

**Path**: Sidebar → Skills → New Skill → SKILL.md editor → file tree sidebar → "Used By" agents → save

The skill detail page has a two-panel layout: left file tree (collapsible, with count badge, add/delete inline), right file viewer with syntax highlighting via Shiki. `SKILL.md` is protected from deletion.

Conflict detection uses a seeded key `${wsId}:${skill.id}@${skill.updated_at}`. Remote changes show conflict banners.

Skills filter chips: All / Used / Unused / Mine.

### Journey 5: Monitor workspace-wide agent activity

**Path**: Issues header → "X working" chip (top-right) → filter to scoped issues → dashboard → KPI tiles

The `WorkspaceAgentWorkingChip` shows avatar stack (up to 3) + count badge + "Working" label when agents are active. Toggling it filters the issue board to only show issues with running tasks.

Dashboard KPIs: Cost / Tokens / Run Time / Tasks. Agent leaderboard sortable by all four. Trend chart with 1d/7d/30d/90d/180d ranges and daily/weekly dimension toggle.

---

## 3. Information Architecture

Top-level objects and their nav positions:

```
Sidebar (left, 256px default)
├── PERSONAL
│   ├── Inbox          (unread count badge)
│   └── My Issues
├── WORKSPACE
│   ├── Issues         (board/list/gantt views)
│   ├── Projects
│   ├── Autopilots
│   ├── Agents
│   ├── Squads
│   └── Usage (dashboard)
├── CONFIG
│   ├── Runtimes       (update dot indicator)
│   ├── Skills
│   └── Settings
└── PINNED ITEMS
    └── (drag-drop issues + projects, skeleton loading)
```

URL structure: `/{workspaceSlug}/{section}` — e.g. `/acme/issues`, `/acme/agents`. Dynamic route group `[workspaceSlug]/(dashboard)/` wraps all workspace content.

Navigation hierarchy rule: top-level items stay active on child routes (`/acme/projects/123` keeps Projects highlighted). Pinned items use strict equality.

Global shortcut: pressing `C` anywhere opens create-issue modal (with project context auto-filled on project detail pages).

---

## 4. Visual Identity

### Color system

Uses oklch color space with semantic tokens:

```css
--color-background      /* page background */
--color-foreground      /* primary text */
--color-card            /* card surface, distinct from bg */
--color-muted           /* secondary surfaces */
--color-muted-foreground /* secondary text */
--color-primary         /* CTAs, active states */
--color-brand           /* brand accent (purple implied by "purple background, robot icon" for agents) */
--color-accent          /* hover states */
--color-destructive     /* errors, failures */
--color-success         /* completed states */
--color-warning         /* unstable/offline amber */
--color-info            /* running/active tasks (blue) */
--color-border          /* 0.5px borders on cards */
```

Full light + dark mode via `.dark` class. Sidebar has its own token set (`--color-sidebar-*`) — the sidebar can differ from main content area.

Scrollbar tokens: thin custom scrollbars (6px, 3px radius) matching the surface color.

### Typography

Four font stacks: `--font-heading`, `--font-sans`, `--font-serif`, `--font-mono`. The onboarding welcome screen uses serif at 5xl–6xl for impact. Body UI uses sans. Code/transcripts use mono.

Text scale as used in components:
- Issue titles: `text-sm font-medium` (14px)
- Secondary meta: `text-xs text-muted-foreground` (12px)
- Section headers (skills file tree): `text-xs font-medium uppercase tracking-wider`
- Onboarding headline: `text-5xl`/`text-6xl` with one italic brand-colored word

### Border radius

Base token `--radius: 0.625rem` (10px). Range: sm through 4xl. Cards use `rounded-lg`. Buttons use `rounded-lg`. Badge chips use tighter radius.

### Shadow

Cards: `shadow-[0_3px_6px_-2px_rgba(0,0,0,0.02),0_1px_1px_0_rgba(0,0,0,0.04)]` — extremely subtle, almost imperceptible. Floating elements: `shadow-lg`. Drag overlay: `shadow-lg` with `rotate-2 scale-105`.

### Density

Compact. Board cards are `py-3 px-2.5`. List row headers are `h-10`. Button default is `h-8`. This is Linear-level density, not Notion-level spaciousness.

### Mood

**Linear-tight meets Vercel-stark**. Not Slack-friendly, not Notion-clean. The product aesthetic is serious developer tooling: monochromatic, semantic color only (never hardcoded `text-red-500`), generous use of `text-muted-foreground` for secondary information, very thin borders (`border-[0.5px]`), and micro-shadows. The only "brand moment" is the purple/violet for agent identity and thinking blocks — everything else is neutral.

The one playful exception: the onboarding welcome screen, which uses serif typography, stacked card animations, and a `welcome-emoji-pop` keyframe (1.12x overshoot spring-pop).

---

## 5. TUI/CLI Experience

### What it is

Not a TUI. Multica's CLI is a pure text CLI — Cobra-based, gh-CLI-styled help format, no bubbletea/lipgloss/charm. Output is `fmt.Printf` to stdout/stderr.

### Output patterns

**Tables**: `text/tabwriter` with headers. Daemon disk usage: `PATH | KIND | STATUS | AGE | SIZE | ARTIFACTS`. Agent list: `ID | NAME | STATUS | RUNTIME | ARCHIVED`. Empty values become `-` via `emptyDash()`.

**Status messages**: plain text with Unicode symbols:
- `"✓ Setup complete!"`
- `"⚠ Server at [url] is not reachable"`
- `"Daemon started (pid %d, version %s)"`
- `"Authenticated as %s (%s)\nToken saved to config.\n"`

**Formats**: `--output json` flag available on most commands for machine-readable output. Default is table.

**Progress polling**: 500ms sleep loops during startup/shutdown with health check retries (up to 10 attempts). No progress bars.

**Interactive**: `multica login` opens browser, prints URL for manual copy if auto-open fails, waits with `"Waiting for authentication..."`. Token prompt: `"Enter your personal access token: "` (reads from stdin).

**Log streaming**: `multica daemon logs --follow` tails log file (similar to `tail -f`).

### Agent context commands

Agents themselves use the CLI from within their task workdir: `multica issue get`, `multica issue create`, `multica comment add`. These are called by the agent CLI (Claude, Codex, etc.) during execution, not by the human user.

### Help format

gh-inspired: grouped command sections (CORE, RUNTIME, ADDITIONAL), `NAME: description` pairs with auto-padded alignment, UPPERCASE section headers, no ASCII art.

---

## 6. Cross-Surface Coherence

```
Web (Next.js)  ──shares packages/views + packages/ui──  Desktop (Electron)
      │                                                         │
      └──── same components, same tokens, same routes ─────────┘
                              │
                    packages/core (headless stores)
                              │
              Mobile (Expo/RN) ── types + pure fns only ──
                              │
                    CLI (Cobra Go) ── REST API calls only ──
```

**Web ↔ Desktop**: Extremely coherent. They share all business components via `packages/views/`, all UI primitives via `packages/ui/`, all state via `packages/core/`. The only divergence: desktop uses WindowOverlay for pre-workspace flows (not routes), has a DragStrip for macOS window dragging, uses IPC bridge for daemon communication, and has per-workspace tab groups with cross-workspace navigation interception.

**Web/Desktop ↔ Mobile**: Intentionally independent. Same API, same semantic token naming, same Tailwind utility patterns — but mobile uses NativeWind 4 + Tailwind 3.4, react-native-reusables (RNR) not shadcn, and React 19 pinned independently of web. Design language is similar (same color token names, same density philosophy) but not pixel-perfect. Mobile uses iOS native APIs first (`Alert.prompt`, `ActionSheetIOS`).

**Web/Desktop ↔ CLI**: Near-zero visual coherence. The CLI is pure text, the web is a polished component system. They share the same data model but nothing else. The CLI is a power-user control plane, not a companion to the web UI.

**Coherence verdict**: The web/desktop pair is exceptional — probably the tightest sharing architecture in any open-source project at this complexity. Mobile is close in spirit. CLI is a separate experience by design.

---

## 7. Notifications / Progress / Async UX

### The "agent working for 20 minutes" pattern

Three concurrent mechanisms:

```
┌──────────────────────┐
│  AgentLiveCard       │ ← sticky frosted glass banner on issue detail
│  (WebSocket driven)  │   "alice-agent is working  7m 17s  10 tool calls"
└──────────────────────┘
          │
          ▼
┌──────────────────────┐
│  WorkspaceAgent      │ ← filter chip in issues header
│  WorkingChip         │   "3 working" avatar stack, click to filter board
└──────────────────────┘
          │
          ▼
┌──────────────────────┐
│  AgentPresence       │ ← dot + workload chip on every agent row/card
│  Indicator           │   "working 2 tasks / queued 1"
└──────────────────────┘
```

**Issue-level live card** (`AgentLiveCard`): sticky at `top-4`, `z-10`, `backdrop-blur-md` frosted glass, `bg-background/55`. Shows elapsed time (updates every second via `setInterval`), tool call count, spinner (`Loader2 animate-spin` for running, `Clock` for queued). Cancellation button inline.

**Workspace-level chip**: shows in issues header. Hovering active chip shows full task list panel.

**Activity feed**: agent comments stream into the issue thread in real time via WebSocket `task:message` events. Older activity groups collapse by default; the trailing block stays expanded.

**Transcript dialog**: full execution log — 5-color event taxonomy (emerald/agent, violet/thinking, blue/tool, slate/result, red/error), timeline bar with proportional segments, expandable JSON tool inputs/outputs with secret redaction, toggleable sort direction.

**Execution log section**: past runs visible in issue detail. Running tasks on top, failed first in historical list. Hover reveals transcript/cancel/retry buttons via gradient-faded overlay.

**No email notifications visible in the codebase**. Async is handled entirely through the web UI's WebSocket connection. The Inbox (sidebar nav item with unread badge) is the notification surface for items that need human attention.

---

## 8. Reading the Marketing Site

Hero headline: "Your next 10 hires won't be human."

This is deliberately provocative — it positions the product not as a productivity tool but as a workforce multiplier. The subtext repositions AI agents from "tools you use" to "colleagues you manage."

Navigation: Home / Changelog / GitHub / Login / Download Desktop / Talk to Sales / Documentation / About.

Feature story arc the homepage tells:

```
1. "Agents as teammates" — assignee picker shows agents alongside humans
2. "Autonomous work" — real-time progress (7m 17s, 10 tool calls)
3. "Skills Library" — reusable deployments, migrations, PR reviews
4. "Runtimes Dashboard" — MacBook + Cloud + Linux Server monitoring
```

Screenshots described: issue "MUL-18 Refactor API error handling middleware" with agent in activity feed. This is deliberate — a real-looking issue, not a toy demo. The story is about normalizing agents in existing workflows, not showcasing a novel interface.

Supported tools prominently listed: Claude Code, Codex, Cursor, Copilot, Gemini, and 6 others. The vendor-neutral message is as important as the feature set.

Tone: serious, technical, slightly provocative. No "magic", no "AI-powered", no "10x". The name etymology (Multics, time-sharing) signals the audience is engineers who appreciate systems history.

---

## 9. Design System Inventory

### Component library

Base: shadcn/ui with **Base UI primitives** (`@base-ui/react`) — not Radix. `base-nova` style variant. All components installed to `packages/ui/components/ui/` via `pnpm ui:add`.

Full inventory (~70 components):
- Form: accordion, alert-dialog, alert, avatar, badge, breadcrumb, button-group, button, calendar, card, carousel, checkbox, collapsible, combobox, command, input-group, input-otp, input, label, native-select, radio-group, select, textarea, time-input
- Display: chart, empty, kbd, pagination, progress, separator, skeleton, spinner, table, tabs
- Interactive: context-menu, data-table, data-table-column-header, dialog, drawer, dropdown-menu, hover-card, item, menubar, navigation-menu, popover, scroll-area, sheet, sidebar, toggle, toggle-group
- Utility: direction, field, resizable, slider, switch, tooltip
- Feedback: sonner (toasts)

### Custom components built on top

- `ProgressRing` — SVG concentric circles, `stroke-dashoffset` animation, `text-primary` / `text-info` color states
- `Sparkline` — stacked bar chart (success bottom, failure top), brand color 60% opacity / destructive 100%, per-component max scaling
- `AgentLiveCard` — frosted glass sticky banner
- `AgentPresenceIndicator` — 2D presence (availability dot + workload chip)
- `AgentLivePeekCard` — hover card with 3 live signals
- `WorkspaceAgentWorkingChip` — avatar stack filter toggle
- `IssueAgentActivityIndicator` — `animate-chat-text-shimmer` text animation
- `ActorAvatar` — polymorphic (member|agent), with status dot and hover card
- `ResizablePanelGroup` — issue detail main/sidebar split

### Naming conventions

Domain-first: `agent-live-card`, `issue-agent-activity-indicator`, `runtime-list`. Not feature-first (`live-agent-card`). Consistency enforced via `apps/docs/content/docs/developers/conventions.mdx`.

### State management tokens

- Zustand: client state (filters, selections, drafts, modal state)
- TanStack Query: server state (issues, agents, tasks)
- WebSocket events: invalidate queries, never write to stores directly

### i18n

`packages/views/locales/`. Chinese supported with dedicated voice guide. All UI strings externalized.

---

## 10. Accessibility & Responsiveness

### Accessibility signals

- Semantic tokens exclusively — no hardcoded color values (enforced in CLAUDE.md)
- Focus states: `focus-visible:border-ring focus-visible:ring-3` on all interactive elements
- `disabled:pointer-events-none disabled:opacity-50` on buttons
- Base UI / Radix primitives handle keyboard nav and ARIA for dialogs, dropdowns, popovers
- `select-none` on buttons prevents text selection on rapid clicks
- Sonner toasts use `align-items: flex-start` (not centered)
- Skip-to-content patterns not explicitly visible in codebase

### Responsiveness

- Mobile-first: `16px` font-size on inputs prevents iOS auto-zoom (enforced in base.css)
- Sidebar: Sheet overlay on mobile, 256px fixed on desktop
- Issue detail: Sheet overlay on mobile, ResizablePanel on desktop
- Dashboard KPI grid: 1-col mobile, 2-col tablet, 4-col desktop
- Onboarding: single-col below `lg`, two-col above
- Mobile app: separate Expo/RN app, not responsive web

### Gaps

- No explicit screen reader testing evidence in codebase
- WCAG compliance not mentioned in CLAUDE.md or contributing guide
- Transcript dialog (JSON tool inputs, 4000-char truncation) has no announced live region for streaming content

---

## 11. UX Gaps / Friction

1. **CLI and web are disconnected experiences.** `multica issue list` produces a bare text table. There is no way to get the rich web UI information (sparklines, presence, agent activity) in the terminal. The CLI is infrastructure, not a productivity surface.

2. **Onboarding runtime step can dead-end.** If no CLI is installed, the step times out after 5 seconds showing a "coming soon" cloud option that is disabled. Users without Claude/Codex/etc. installed hit a wall. The skip path exists but is not prominent.

3. **No offline indicator.** The runtimes page re-renders every 30 seconds, but there is no ambient signal in the main UI when the daemon goes offline mid-session.

4. **Frosted glass banner blocks content on small screens.** `AgentLiveCard` is `sticky top-4 z-10`. On a short viewport with multiple active agents, this can obscure the issue title.

5. **Gantt view is opt-in and may not be available on all surfaces** (`allowGantt` prop). The view switcher silently falls back to list, which could confuse users who switched to Gantt in one view and see it missing in another.

6. **Skill conflict UX is developer-grade.** The conflict banner requires understanding `updated_at` semantics. No visual diff of what changed remotely.

7. **Desktop tab bar not visible in the web UI.** The desktop's per-workspace tab groups are a meaningful UX improvement over the web's single-view navigation. New users on web may not discover the "pin items" feature in the sidebar.

8. **Transcript dialog is unindexable.** Tool call inputs are raw JSON, output is truncated at 4000 chars, no search. Fine for debugging, inadequate as an audit trail.

9. **Agent avatar is the only visual differentiator from humans.** The CLAUDE.md says "purple background, robot icon" but the comment-card analysis shows no explicit purple treatment in actual comment rendering — the differentiation may be primarily in the assignee picker, not the activity feed.

10. **Mobile is iOS-only.** The Expo app targets iOS. Android users must use the web app, which isn't designed for touch-first navigation.

---

## 12. What to Clone for ainb

### Clone verbatim (5-10 moves)

**1. Polymorphic actor model**
Agents and humans share the same assignee picker, same presence indicators, same activity feed. ainb should model every surface where a "person" appears as actor-type-agnostic. The visual differentiation (dot color, avatar ring color) should come from the actor type, not a separate code path.

**2. 3-state presence system (online / unstable / offline)**
Not just online/offline. The amber "unstable" state (runtime connected but degraded) is critical for communicating agent health without alarming users unnecessarily. Map to ainb's daemon health model: connected/degraded/disconnected.

**3. Sticky frosted-glass live card on active tasks**
```
bg-background/55 backdrop-blur-md sticky top-4 z-10 rounded-lg
```
This pattern nails the "background task in progress" UX without blocking the main content. For ainb TUI → web, this should be a persistent bottom bar or sidebar chip, not a modal.

**4. Execution log taxonomy (5 colors)**
Agent text (emerald), Thinking (violet), Tool call (blue), Tool result (slate), Error (red). This is the best visualization of LLM execution seen in any open-source project. Clone exactly for ainb's task transcript view. The timeline bar with proportional segments is a bonus.

**5. Sparkline with dual-dimension encoding**
Success height + failure proportion in a single bar. No legend needed. Scales per-component to prevent busy agents flattening quiet ones. This is a compact, scan-speed-friendly way to show agent reliability history on list rows.

**6. `WorkspaceAgentWorkingChip` pattern**
An ambient status chip that filters the board to active work. ainb should have an equivalent in TUI — a hotkey or pill that narrows the view to sessions with active agents. The avatar stack (up to 3) + count badge is the right density.

**7. Semantic token system with sidebar sub-tokens**
The sidebar having its own `--color-sidebar-*` token family is underrated. It lets the sidebar have a subtly different surface color without breaking the main content tokens. For ainb's TUI, this maps to: panel background ≠ content background, use distinct color indices.

**8. Progressive disclosure in issue sidebar**
Core properties always visible, optional ones behind "+ Add property". Do not show 15 fields on every task detail. ainb's task detail should show: status, assignee, created, duration always — everything else on demand.

**9. Drag region + WindowOverlay pattern for desktop**
If ainb ships an Electron/Tauri desktop wrapper, the DragStrip + WindowOverlay pattern avoids the web-in-a-frame aesthetic. Pre-workspace flows as overlays (not routes with URL bars) feel native.

**10. Inbox as first-class nav item with unread badge**
Not a notification bell in the header. A dedicated nav section with unread count that agents and the system write to. Normalizes async communication from agents. For ainb, this is the "what needs my attention" surface.

### Consciously diverge from (3-5 moves)

**1. CLI UX: add richness**
Multica's CLI is functional but bare — no colors, no progress bars, no real-time streaming. ainb already has a Ratatui TUI. Lean into this. The CLI should be a first-class experience: use `indicatif` for progress bars on daemon operations, `console` crate for colors, live task streaming in the terminal. Multica deliberately kept the CLI minimal; ainb's competitive angle is that the TUI IS the product, not a fallback.

**2. Don't hide the terminal from terminal users**
Multica's strategy is "use the web UI for everything interesting." ainb's TUI means terminal users should be able to do everything from the terminal: live agent transcript streaming, sparklines via Unicode block chars, inline task management. The TUI and web should be equal surfaces, not tool + product.

**3. Make mobile a real surface, not an afterthought**
Multica's mobile app is iOS-only and shares no UI components with web. For ainb, if mobile matters, it should feel like the same product. Consider a responsive web app optimized for mobile before a native app — the architecture cost is lower and the surface coherence is higher.

**4. Richer conflict UX for skills/configurations**
Multica shows a conflict banner but no diff. For ainb where skills are a core primitive, show a side-by-side diff when remote changes conflict with local edits. This is an area where ainb can materially improve on Multica.

**5. Surface the "why did it fail" earlier**
Multica puts the execution transcript behind a button in a collapsed section. For ainb, when a task fails, the failure reason should be the first thing visible on the task detail — not the last. Failure is a first-class workflow in agent-managed work, not an exception.

---

## ASCII Mockups of Top 3 Screens

### Screen 1: Issues Board View

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│ [workspace-logo] acme  >  Issues                    [search]  [C] create        │
├──────────┬──────────────────────────────────────────────────────────────────────┤
│ Inbox  1 │  ● All  ○ Members  ○ Agents   [filter▼] [display▼] [⬚][≡][⬛]        │
│ My Issues│  [3 working ▪▪▪]                                                     │
│ ──────── │                                                                      │
│ Issues   │  ┌─ Todo (12) ──────────┐  ┌─ In Progress (3) ──┐  ┌─ Done (48) ──┐ │
│ Projects │  │ ┌──────────────────┐ │  │ ┌────────────────┐ │  │ ┌────────────┐│ │
│ Autopilot│  │ │ MUL-42           │ │  │ │ MUL-18  ↻⟳    │ │  │ │ MUL-17     ││ │
│ Agents   │  │ │ Add rate limiting│ │  │ │ Refactor API   │ │  │ │ Auth fixes ││ │
│ Squads   │  │ │ to billing API   │ │  │ │ error handling │ │  │ └────────────┘│ │
│ Usage    │  │ │                  │ │  │ │ middleware      │ │  │ ┌────────────┐│ │
│ ──────── │  │ │ [alice ▪]  7/5  ○│ │  │ │ [bot ⬡]  ○ 2m │ │  │ │ MUL-16     ││ │
│ Runtimes │  │ └──────────────────┘ │  │ └────────────────┘ │  │ └────────────┘│ │
│ Skills   │  │ ┌──────────────────┐ │  │ ┌────────────────┐ │  └──────────────┘ │
│ Settings │  │ │ MUL-43           │ │  │ │ MUL-19         │ │                   │
│          │  │ │ Update deps      │ │  │ │ Write e2e tests│ │                   │
│          │  │ └──────────────────┘ │  │ └────────────────┘ │                   │
│          │  │ [+]                  │  │ [+]                 │                   │
│          │  └──────────────────────┘  └─────────────────────┘                  │
└──────────┴──────────────────────────────────────────────────────────────────────┘
```

Key: `↻⟳` = agent working indicator (animate-chat-text-shimmer), `⬡` = agent avatar (purple ring), `○` = status dot, `▪` = presence chip.

---

### Screen 2: Issue Detail with Agent Working

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│ ← Issues  /  MUL-18  Refactor API error handling middleware                     │
├──────────────────────────────────────────────────────────────────────────────────┤
│ ┌── LIVE CARD (sticky, frosted glass) ────────────────────────────────────────┐ │
│ │ [⬡] claude-agent  is working  ⟳  9m 42s  ·  14 tool calls          [✕ stop]│ │
│ └─────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                  │
│  MAIN CONTENT (flex)                           │  SIDEBAR (resizable, ~280px)   │
│ ─────────────────────────────────────────────  │ ─────────────────────────────  │
│  Description                                   │  STATUS        In Progress     │
│  Refactor the error handling middleware to     │  ASSIGNEE      [⬡] claude-agent│
│  use structured error types...                 │  PROJECT       Backend API     │
│                                                │                                │
│  ACTIVITY FEED (virtualized, timeline)         │  + Add property                │
│                                                │  ─────────────────────────     │
│  ┌────────────────────────────────────────┐   │  PULL REQUESTS                 │
│  │ [⬡] claude-agent  just now             │   │  #142 open ↗                   │
│  │ Analyzing middleware structure...      │   │  ─────────────────────────     │
│  └────────────────────────────────────────┘   │  METADATA                      │
│                                                │  Created  2d ago               │
│  ┌────────────────────────────────────────┐   │  Updated  just now             │
│  │ [👤] alice  3h ago                     │   │                                │
│  │ Please also check the rate limit       │   │  TOKEN USAGE (collapsed)       │
│  │ error codes in auth/middleware.go      │   │  ▸ 14,203 tokens               │
│  └────────────────────────────────────────┘   │                                │
│  ─────────── earlier (collapsed) ──────────   │                                │
│                                                │                                │
│  EXECUTION LOG                                 │                                │
│  [running]  claude-agent  ·  9m 42s  ···      │                                │
│  [Show past runs ▾]                            │                                │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

### Screen 3: Agent Detail Page (Activity tab)

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│ ← Agents  /  claude-agent                          ● online                     │
├─────────────────────────┬───────────────────────────────────────────────────────┤
│  INSPECTOR (320px)      │  Activity  Tasks  Instructions  Skills  Env  Args     │
│ ─────────────────────── │ ─────────────────────────────────────────────────────  │
│  Avatar (64px)          │                                                        │
│  [⬡] claude-agent       │  TODAY                                                │
│  ─────────────────────  │  ┌──────────────────────────────────────────────────┐ │
│  Runtime                │  │ MUL-18  In Progress  ●  9m 42s                   │ │
│  wifi ●  MacBook Pro    │  │ Refactor API error handling                       │ │
│  ─────────────────────  │  └──────────────────────────────────────────────────┘ │
│  Skills (3)             │                                                        │
│  [deploy] [migrate] [+1]│  PAST 7 DAYS  ─────────────────────────────          │
│  ─────────────────────  │  ████████████████████████████████████  28 runs        │
│  Owner   alice          │  ▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉▉ (sparkline)          │
│  ─────────────────────  │                                                        │
│  Instructions           │  RECENT COMPLETED                                      │
│  You are a backend      │  ┌──────────────────────────────────────────────────┐ │
│  specialist...          │  │ ✓  MUL-17  Auth fixes       2h ago      4m 12s   │ │
│  [Edit]                 │  │ ✗  MUL-15  Perf regression  5h ago      failed   │ │
│                         │  │ ✓  MUL-14  DB migration     1d ago      8m 03s   │ │
│                         │  └──────────────────────────────────────────────────┘ │
└─────────────────────────┴───────────────────────────────────────────────────────┘
```

---

## Summary Table

| Dimension             | Multica                              | ainb opportunity               |
|-----------------------|--------------------------------------|--------------------------------|
| Visual density        | Linear-tight (compact, semantic)     | Match it                       |
| Color system          | oklch semantic tokens, full dark mode| Clone token naming             |
| Component base        | shadcn + Base UI                     | Same if web; RNR if mobile     |
| CLI UX                | Cobra, plain text, no colors         | Ratatui TUI is stronger — use it |
| Agent differentiation | Avatar ring, robot icon, purple bg   | Clone polymorphic actor model  |
| Async progress        | Frosted sticky banner + WS events    | Clone for web; adapt for TUI   |
| Onboarding            | 6-step wizard, polling runtime detect| Clone step structure           |
| Navigation            | Left sidebar, semantic sections      | Clone information architecture |
| Transcript view       | 5-color taxonomy, timeline bar       | Clone exactly                  |
| Mobile                | iOS-only Expo app                    | Consider responsive web first  |
