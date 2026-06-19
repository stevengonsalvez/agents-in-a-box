---
name: ainb-fleet:needs
description: |
  Center control panel — enumerate every claude session that is blocked
  waiting on something: a user answer (AskUserQuestion fired), an API
  error retry, an idle assistant turn-end with no follow-up, or an
  explicit WAITING: marker. Returns rich JSON with signal kind + context
  per session. Use this when you've stepped away from the fleet and want
  one place to see everything that wants your attention and answer it.
version: "0.2.0"
user-invocable: true
triggers:
  - ainb-fleet:needs
  - fleet needs
  - what does my fleet need
  - blocked sessions
  - sessions waiting on me
  - jarvis status
  - control panel
allowed-tools:
  - Bash
---

# ainb fleet:needs — center control panel

Single place to see every claude session that wants your attention. Four
signal kinds classified per session, priority ASK > ERR > IDLE > WAIT.

## `needs` vs `fleet-needs` — which skill?

> **This skill (`needs`) is the plain, always-available control panel.** It is
> prompt-driven: render the HUD, fire AskUserQuestion, route answers back. It
> needs no workflow gate and works everywhere.
>
> **[`/ainb-fleet:fleet-needs`](../fleet-needs/SKILL.md) is the workflow-backed
> "Jarvis" cockpit** — same purpose, but it runs the deterministic `hangar`
> workflow (verb=needs) for discover→enrich→prioritize, batches enrichment in a
> single agent, and adds **key-route ASK answering** (presses the target's
> picker keys via tmux for an open AskUserQuestion). It requires
> `CLAUDE_CODE_WORKFLOWS=1`; with the gate off it falls back to **this** skill.
>
> Rule of thumb: reach for `fleet-needs` when the workflow gate is on and you
> want the batched/cockpit path; otherwise use `needs`. Both read the same
> underlying `ainb fleet needs` data, so the panel content is identical.

## Source of truth — materialized `current_state` (hooks), tmux fallback

`ainb fleet needs` reads the **event-sourced** read model first. Claude Code
hooks append lifecycle events to `events.jsonl`; notifyd ingests them into a
SQLite `current_state` table that materializes the latest stage per session
(ASK / ERR / WAIT / IDLE / RUNNING / DONE). For each session (keyed by `cwd` —
the fleet's cross-source dedupe key), the reader resolves one of three ways:

| resolution | when | what happens | row `source` |
|---|---|---|---|
| **hook-authoritative** | a materialized ASK/ERR/WAIT/IDLE row exists for the cwd (`source = hook`) | use it directly — **no pane scan** | `"hook"` |
| **hook-healthy** | the hook says RUNNING / DONE | not a "need" — emit nothing, skip the pane scan | — |
| **fallback** | no usable hook row — absent from `current_state` (a **non-Claude session like Codex/Gemini fires no Claude hooks**), `source = tmux`, or a stale row | run the live `classify()` pane/transcript scan, exactly as before the event store existed | `"tmux"` |

So a Claude session's ASK/WAIT/IDLE/ERR is learned **primarily from hooks**;
the tmux pane scan is the fallback for non-Claude agents (Codex/Gemini) and for
sessions the daemon hasn't materialized. The read-time merge IS the
tmux / non-Claude fold — each session yields at most one row, never double-listed.

The `source` field (`"hook"` | `"tmux"`) on each row tells a consumer which
path produced it. ASK/ERR/WAIT/IDLE are sticky states the materializer only
changes on a new event, so an aged row is normally still the truth and there is
**no staleness check by default**. Override only when you have an independent
reason to distrust an aged row: `AINB_FLEET_STATE_STALE_MS=<ms>` (default `0` =
no age check). When notifyd is down or the hooks were never installed,
`current_state` is empty and **every** session falls back to the live scan, so
the panel still works.

> **This skill does NOT install hooks.** It only *consumes* the materialized
> state. The global Claude Code hooks are installed by
> [`ainb fleet atc setup`](../atc/SKILL.md) (into `~/.claude/settings.json`).

## Run

```bash
ainb fleet needs                        # text Jarvis-HUD layout
ainb --format json fleet needs          # structured JSON (preferred by LLM)
ainb fleet needs --idle-min 10          # override idle threshold (default 5 min)
```

Env override: `AINB_FLEET_IDLE_MIN=10`.

## JSON schema

Each row in the output array:

```jsonc
{
  "session": {
    "id": "…",
    "cwd": "/Users/…",
    "tmux_session": "tmux_…",
    "workspace_name": "…",
    "peer_id": "…",         // null if not broker-registered
    "sources": ["ainb", "peers"],
    "summary": "…",         // JSONL-derived (see standup)
    "last_seen_ms": 1779…
  },
  "kind": "ASK" | "ERR" | "IDLE" | "WAIT",
  "context": {
    // shape depends on `kind`:
    //
    // ASK — structured AskUserQuestion pulled from JSONL tool_use:
    "question": "…",
    "header": "…",
    "options": [ { "label": "…", "description": "…" } ],
    "multi_select": false,
    //
    // ERR — API error matched in pane:
    "pattern": "rate_limited",
    "snippet": "…API Error · rate_limited · …",
    //
    // IDLE — assistant turn-end, no user follow-up, > N min ago:
    "idle_minutes": 17,
    "last_assistant_text": "…",
    //
    // WAIT — explicit WAITING: prefix in peer summary:
    "marker": "WAITING:",
    "text": "…the post-marker text…"
  },
  "route_hint": "tmux" | "broker" | "none",  // advisory; send is tmux-first
  "source": "hook" | "tmux"                   // provenance: event-sourced row vs live tmux/transcript scan
}
```

`source` distinguishes a hook-sourced need (materialized from `events.jsonl`)
from a tmux-folded one (the live fallback scan for non-Claude agents / un-
materialized sessions). It is additive — older consumers that ignore it still work.

## Enrich — token-efficient (cache-aware)

Each row also carries three enrich fields:

| field | meaning |
|---|---|
| `enrich_key` | content hash of the card; the producer's cache key |
| `enriched` | a **fresh cached** suggestion (free) — render it as-is |
| `need_enrich` | `true` only when there is no cache entry and enrichment is on |

Draft the missing ones by COUNT, never one subagent per session:

```
stale = rows where need_enrich == true
  stale == 0  ─▶ render cached/snippet only         (0 tokens)
  stale ≤ 6   ─▶ draft inline in THIS session        (0 subagents)
  stale  > 6  ─▶ /ainb-fleet:fleet-needs workflow     (1 batched agent)
```

Cutoff: `AINB_FLEET_ENRICH_INLINE_MAX` (default 6). When you draft inline,
persist each so the next read is free:

```bash
ainb fleet enrich-cache put --key "<enrich_key>" --suggestion "<text>"
```

`ainb fleet needs --no-enrich` (or `AINB_FLEET_ENRICH=0`) skips all of this —
cached suggestions still render, nothing new is drafted, 0 tokens.

## Render template — Jarvis HUD

The calling LLM session should render this exact layout in chat:

```
╔════════════════════════════════════╗
║  ⚡ FLEET STATUS · N NEED YOU ⚡    ║
║  🔴 X err  🟡 Y ask  ⚪ Z idle      ║
║  highest priority: <session> (<KIND>) ║
╚════════════════════════════════════╝

▸ 🟡 <session> ─ <question text>
    ① <option label>
    ② <option label>
    ③ <option label>

▸ 🔴 <session> ─ <pattern> (<snippet>)

▸ ⚪ <session> ─ idle <N>m
    '<last assistant snippet>'

▸ 🟢 <session> ─ WAITING: <text>
```

Rules:
- Banner always present (even for 0 sessions — "0 NEED YOU" + skip the priority line)
- Per-card `▸` prefix, signal emoji, em-dash separator
- ASK cards show options as ① ② ③ ④ ⑤ (numeric circled)
- Cap at 10 cards visible; if more, render top 10 then `+ N more`
- Priority order: ASK > ERR > IDLE > WAIT (binary already sorts this way)
- Status emoji: 🔴 ERR · 🟡 ASK · ⚪ IDLE · 🟢 WAIT
- Box-drawing chars: ╔ ╗ ╚ ╝ ║ ─ ▸ (monospace, no ANSI needed)

## Compose the AskUserQuestion batch

After rendering the HUD, fire AskUserQuestion **per session that wants an
answer**. Each kind maps to a different prompt shape:

| kind | AskUserQuestion shape |
|---|---|
| ASK | Relay options 1:1 from `context.options` |
| ERR | "<session> hit `<pattern>` — retry? skip? investigate?" |
| IDLE | "<session> idle <N>m after: '<snippet>' — resume? close? other?" |
| WAIT | "<session> says: `<marker> <text>` — answer:" |

For ASK kinds, the LLM session SHOULD use AskUserQuestion's structured
options so the user can click rather than type.

## Route the answers back

Writes are **tmux-first**. Prefer driving the target pane directly — it lands
reliably and is verifiable with `capture-pane`:

```bash
# default, most reliable: write keystrokes straight to the pane
tmux send-keys -t "<tmux_session>" -l "<answer>"
tmux send-keys -t "<tmux_session>" Enter
# verify it landed
tmux capture-pane -t "<tmux_session>" -p -S -40 | grep -F "<answer>" && echo "✓"
```

Or go through the verb, which honours `AINB_FLEET_TRANSPORT` (tmux-first):

```bash
ainb fleet broadcast "<answer>" --filter "<exact tmux_session>"
```

`route_hint` is advisory — it mirrors the default tmux-first order:
- `tmux` — a live tmux pane exists → `tmux send-keys -l` (the normal case)
- `broker` — no tmux pane, but a healthy broker peer → claude-peers HTTP fallback
- `none` — bg job or no targets; can't auto-route; tell user manually

## Composition example

```bash
out=$(ainb --format json fleet needs)

# 1. Render the HUD in chat (LLM does this from the JSON)
# 2. For each entry, fire AskUserQuestion (claude code session does this)
# 3. After each answer:
echo "$out" | jq -r ".[] | select(.session.tmux_session == \"$picked\") | .session.tmux_session" \
  | xargs -I% ainb fleet broadcast "$answer" --filter "%"
```

## Caveats

- **Race window** — between fleet sees the AskUserQuestion and Stevie's
  answer reaches claude, the session still appears blocked on next
  invocation. Acceptable for v0.2; full dedupe lands in v0.3.
- **IDLE false positives** — a session you walked away from briefly shows
  IDLE if past the threshold. Tune via `--idle-min` or env var.
- **WAIT requires opt-in** — only fires when a session explicitly sets
  `summary` to start with `WAITING:`. Most sessions never do.
- **Bg jobs are excluded** — they have no tmux pane and no JSONL
  transcript follow the normal shape; surfacing them would dilute the
  panel. Use `ainb status <job>` for those.

## Changelog

### v0.3 — hooks-primary (event-sourced)

- Reads the materialized `current_state` table FIRST (hook-sourced ASK/ERR/
  WAIT/IDLE per session); the live tmux/transcript `classify()` scan is now the
  **fallback** for non-Claude agents (Codex/Gemini) and un-materialized sessions
- Added the `source` field (`"hook"` | `"tmux"`) to each row
- `AINB_FLEET_STATE_STALE_MS` (default `0` = no age check) to optionally distrust aged rows
- Hooks are installed by `ainb fleet atc setup`, NOT by this skill

### v0.2

- Added ASK, ERR, IDLE signal kinds (was: WAIT-only in v0.1)
- Added `--idle-min` flag + `AINB_FLEET_IDLE_MIN` env override
- Rich JSON output with `context` polymorphic per kind
- `route_hint` field to guide answer-routing
- Text-mode renders the Jarvis HUD directly
- Priority sort: ASK > ERR > IDLE > WAIT
