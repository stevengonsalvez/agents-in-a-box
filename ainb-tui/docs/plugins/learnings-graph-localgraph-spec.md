# Spec: Radial ego local-graph view (learnings Graph tab)

**Generated from:** .agents/specs/2026-06-06-learnings-graph-localgraph-tui-stub.md
**Date:** 2026-06-07
**Format:** diagram-first, table-second, no prose paragraphs

## Problem

| Question | Answer |
|----------|--------|
| What? | A spatial **radial ego-graph** of an entity + its neighbours in the Graph tab, vs the current text edge list |
| Why? | "See the graph" like Obsidian's local-graph panel — spatial sense of how memories connect |
| Who? | Anyone browsing the learnings KB in `ainb` (Stevie first) |

## Approach

| Option | Summary | Tradeoff | Picked? |
|--------|---------|----------|---------|
| A live force-directed | per-frame physics sim on Canvas | organic but non-deterministic + costly | |
| B static one-shot | single FR pass, frozen | deterministic-ish, still layout math | |
| C radial ego | centre + deterministic hop-rings | clean on char grid, cheap, **testable** | ✓ |

**Why C:** deterministic radial positions → exact-token tripwire tests; reads cleanly in a terminal; cheapest; matches the chosen 1-hop / ~15-node ego scope. (Force-directed considered but its non-determinism fights the tripwire harness.)

## Decisions (locked in interview)

| Decision | Choice |
|----------|--------|
| Layout | radial ego (centre + hop rings), deterministic |
| Coexistence | 3rd Graph sub-mode; **`v`** cycles `neighbourhood → map → back` (`c` still independently toggles community) |
| Default scope | **1 hop**, ~15-node cap, overflow → a `+N more` node |
| Edge type | **colour-coded + midpoint type label** (solves/caused_by/requires/relates_to) |
| Edge direction | **arrowheads** on directed edges (caused_by/solves/requires); `relates_to` plain line |
| Entry centre | the entity **selected in the Graph entity list** when you toggle to map |
| Enter (`⏎`) | **recentre** the graph on the selected node |
| `o` | open the **learnings behind the entity** (picker if >1 → P6 Detail) |
| Node render | **boxed labels** `[entity]` on rings, ASCII line edges |
| v1 keys | `↑↓←→` move · `⏎` recentre · `h` hop 1↔2 · `e` expand `+N` · `o` open · `Backspace` exit |
| v1 ALSO includes | **mouse** (click node → select/recentre) · **animation** (transition on recentre) |
| Deferred | full-graph (non-ego) view · force-directed physics |

## Architecture

```
 record relationships (.entities.yaml, typed)
        │  aggregate  (already built in src/data/graph.rs)
        ▼
 ┌─ typed adjacency ─┐    centre = list-selected entity
 │ entity → [(rel,   │           │  hop=1 (toggle 2)
 │  target, dir)]    │           ▼
 └─────────┬─────────┘   ┌─ EgoSubgraph ─┐  extract centre + N-hop,
           └──────────▶  │ nodes ≤ cap   │  cap → "+N more" overflow node
                         └──────┬────────┘
                                ▼
                       ┌─ RadialLayout ─┐  deterministic: centre @ mid,
                       │ (x,y) per node │  hop-k on ring radius k, angular
                       └──────┬─────────┘  slots by stable sort
                              ▼
        ┌─ MapRender (ratatui Buffer) ─────────────────┐
        │ boxed nodes · ASCII edges · arrowheads ·     │ ─▶ WireBuffer ─▶ host
        │ coloured midpoint type labels · ▶ selection  │
        └──────────────────────────────────────────────┘
              ▲ MapState: centre, hop, selection, expanded, anim-frame
              ▲ MouseHit: click (col,row) → nearest node box
              ▲ Anim: interpolate old→new positions over K frames on recentre
```

| Component | Purpose | Owns |
|-----------|---------|------|
| `EgoSubgraph` | extract centre + N-hop neighbourhood, apply cap → `+N` | node/edge set for the view |
| `RadialLayout` | deterministic (x,y) per node by ring + angular slot | positions (no physics) |
| `MapRender` | draw boxes + ASCII edges + arrowheads + coloured labels into the Buffer | the WireBuffer cells |
| `MapState` | centre id, hop depth, selected node, expanded set, anim frame | interaction state |
| `MouseHit` | map a click (col,row) → the node whose box contains it | click→node resolution |
| `Anim` | interpolate node positions old→new across K frames on recentre | transition frames |

## Interface (TUI)

```
┌─ 🧠 Learnings ── Browse │ Search │ Graph · MAP ──────────────┐
│ centre: /plugin update command         hop:1  nodes:9 (+3)   │
│                                                              │
│        [stale plugin cache]                                  │
│             ╲ solves                                         │
│  [autowire] ─caused_by→ ◉ /plugin update ─requires→ [cache]  │
│             ╱                                                │
│        [30-commit threshold]              [+3 more]          │
│ sel: stale plugin cache                                      │
│ ↑↓←→ move · ⏎ recentre · h hops · e expand · o open · v view │
└──────────────────────────────────────────────────────────────┘
```

| Surface | Trigger | Shape |
|---------|---------|-------|
| Map sub-mode | `v` (from neighbourhood) | radial canvas over the Graph tab body |
| Learnings picker | `o` on a node | small popup list of learnings citing the entity → `⏎` → P6 Detail |
| Community view | `c` | unchanged (orthogonal toggle) |

| Key | Action |
|-----|--------|
| `v` | cycle Graph view: neighbourhood → map → neighbourhood |
| `↑ ↓` | move selection across rings |
| `← →` | orbit selection within a ring |
| `⏎` | recentre map on selected node (animated) |
| `h` | toggle hop depth 1 ↔ 2 |
| `e` | expand the `+N more` overflow node |
| `o` | open learnings behind the selected entity (picker → Detail) |
| mouse click | select / recentre the clicked node |
| `Backspace` | exit map (host-reserved `Esc` pops the whole screen) |

## Behavior

Happy path:

```
[Graph·neighbourhood] ──v──▶ [MAP centred on list-selected entity]
   ──↑↓←→──▶ [select a ring node] ──⏎──▶ [recentre, animate] ──▶ [new ego]
   ──o──▶ [pick a learning] ──⏎──▶ [P6 Detail] ──⌫──▶ back to MAP
```

| Scenario | Trigger | Expected behavior |
|----------|---------|-------------------|
| node cap exceeded | >15 neighbours at hop range | show 15 + a `[+N more]` node; `e` expands it |
| isolated entity | 0 edges | lone centre box + "no connections" hint |
| huge hub | hot entity, 50+ edges | cap to 15 + `+N`; `h`/`e` to dig |
| multibyte label | unicode entity name | truncate via `chars()`, never byte-slice (UTF-8 trap) |
| recentre | `⏎`/click on node | animate old→new positions over K frames, then settle |
| empty KB / no entities | open map | empty-state hint, no panic |

## Testing strategy

| Layer | Scope | Gate |
|-------|-------|------|
| Unit (testkit) | radial layout is deterministic (centre at mid; hop-k on ring-k; stable angular order); ego extraction + cap + `+N`; `chars()` truncation; directed-edge arrow mapping; mouse hit-test (col,row→node) | must-pass |
| Tripwire (tmux) | `v` opens map over fixture KB → asserts the centre box + an expected neighbour box + a coloured typed-edge label + an arrowhead render; `⏎` recentre changes the centre token; `h` flips node count; `Backspace` returns | must-pass |

Determinism note: radial layout is fully deterministic, so tripwires assert **exact tokens** (entity boxes, edge labels) — not coordinates. (No RNG seeding needed, unlike force-directed.)

## Out of scope (v1)

- Full-graph (whole-KB, non-ego) view — ego/local only
- Force-directed physics simulation — radial chosen instead
- Saved/persisted layouts across sessions

## Resolved (during /plan)

- **Mouse forwarding**: NEW host protocol extension (P0-mouse). Host already captures crossterm mouse (`EnableMouseCapture` in `ainb-core/src/main.rs`); we add `PLUGIN_HANDLE_MOUSE` + `HandleMouseParams` mirroring `handle_key`, a runtime `MouseInbox`/`send_mouse` priority channel, an SDK `handle_mouse` default-no-op trait method + dispatch, and a `forward_mouse_to_focused_plugin` host forwarder. CTS axis added.
- **Animation tick**: render is dirty-flag-gated (host ticks 33ms but skips unless dirty/viewport-changed). NEW minimal mechanism (P0-anim): `RenderResult.redraw: bool` (`#[serde(default)]`, back-compat) + SDK `Plugin::wants_redraw(&self)->bool` default-false trait method queried post-render; runtime re-marks the plugin dirty when `redraw` is set → next host tick re-renders. requestAnimationFrame pattern; no churn to existing `render()` signatures.
- **Arrowhead glyphs**: per-direction glyph on directed edges (`→ ← ↑ ↓ ↗ ↖ ↘ ↙`) chosen by the centre→neighbour vector octant; `relates_to` renders a plain line with no arrowhead.
- **`o` entity→learnings mapping**: scan loaded `LearningRecord`s for any whose `entities[]` or `relationships[]` cite the selected entity name; 1 hit → open Detail directly, >1 → picker popup → `⏎` → Detail.
- **Ego cap selection**: deterministic — neighbours sorted by `(descending edge-strength, ascending entity name)`; first 15 survive, remainder fold into `[+N more]`.
- **Layout vs Graph body**: map replaces the Graph tab body region (same area as the neighbourhood split); help bar updated to map keys; the `Browse│Search│Graph · MAP` breadcrumb signals the sub-mode.
