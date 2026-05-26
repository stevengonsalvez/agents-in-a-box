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
  "route_hint": "broker" | "tmux" | "none"
}
```

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

Once Stevie answers each, route via the existing send-route:

```bash
ainb fleet broadcast "<answer>" --filter "<exact tmux_session>"
```

`route_hint` tells you what'll happen:
- `broker` — clean send through claude-peers HTTP
- `tmux` — `tmux send-keys -l` literal mode (works for any tmux pane)
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

## v0.2 changelog

- Added ASK, ERR, IDLE signal kinds (was: WAIT-only in v0.1)
- Added `--idle-min` flag + `AINB_FLEET_IDLE_MIN` env override
- Rich JSON output with `context` polymorphic per kind
- `route_hint` field to guide answer-routing
- Text-mode renders the Jarvis HUD directly
- Priority sort: ASK > ERR > IDLE > WAIT
