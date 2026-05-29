---
name: ainb-fleet:fleet-needs
description: |
  Workflow-backed Jarvis control panel. Runs the deterministic `hangar`
  workflow with verb=needs (discover → enrich → prioritize), renders the
  Jarvis HUD from its render-ready cards, fires AskUserQuestion per blocked
  session, and routes each answer back via tmux send-keys (broker fallback
  only). Requires the workflow gate (CLAUDE_CODE_WORKFLOWS=1). If the gate is
  off, fall back to the prompt-driven `/ainb-fleet:needs` skill.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:fleet-needs
  - fleet needs workflow
  - jarvis panel
  - control panel workflow
allowed-tools:
  - Bash
  - Workflow
  - AskUserQuestion
---

# ainb fleet:fleet-needs — workflow-backed cockpit

The session "face" of the sensor-fusion hybrid. The deterministic brain is the
`hangar` workflow (verb=needs); this skill renders its output and handles the
irreducibly-interactive last mile (HUD + AskUserQuestion + routing).

```
SESSION (this skill) ──Workflow({name:'ainb-fleet:hangar', args:{verb:'needs'}})──▶ brain
   render HUD ◀──{banner,cards,asks}──────────────────────────────────────────┘
   AskUserQuestion per ask  ──answer──▶  tmux send-keys (write leg)
```

## Read/write split — architectural principle

| Direction | Channel | Why |
|-----------|---------|-----|
| **Reads** | JSONL (source of truth) — `ainb fleet needs/standup` tails JSONL + pane | content already committed; replayable; ground truth |
| **Writes** | **tmux send-keys** — direct keystrokes to the target pane (verify with capture-pane) | deterministic, no broker latency or delivery gap |
| Fallback writes | peers/broker via `ainb fleet broadcast` | only when no tmux_session known; broker has a known delivery gap |

All write routing below uses tmux first. Broker is a last resort, not the default.

## Step 0 — gate check (fallback if workflows off)

```bash
[ -n "$CLAUDE_CODE_WORKFLOWS" ] && echo "gate on" || echo "gate off"
```

If gate off → **stop and invoke `/ainb-fleet:needs`** (the prompt-skill).
The workflow path only works when `CLAUDE_CODE_WORKFLOWS=1` is set.

## Step 1 — run the hangar workflow with verb=needs

`hangar` is the single multi-verb workflow under `ainb-fleet`. The `needs`
verb runs the Discover ▸ Enrich ▸ Prioritize chain and returns the render-ready
panel data. Other verbs (`standup`, `sequence`) are wired into the same workflow.

```
Workflow({ name: 'ainb-fleet:hangar', args: { verb: 'needs' } })
```

It runs in background; wait for completion. The result is render-ready:

```jsonc
{ "banner": { "need", "err", "ask", "idle", "wait", "top": {session,kind} },
  "cards":  [ { "emoji", "kind", "session", "line", "enriched", "options?" } ],
  "asks":   [ { "question", "header", "options[{label,description}]",
               "multiSelect", "suggestion", "route": {target,hint} } ] }
```

## Step 2 — render the Jarvis HUD

From `banner` + `cards`, render this exact layout in chat:

```
╔════════════════════════════════════╗
║  ⚡ FLEET STATUS · N NEED YOU ⚡    ║
║  🔴 X err  🟡 Y ask  ⚪ Z idle      ║
║  top: <banner.top.session> (<KIND>) ║
╚════════════════════════════════════╝

▸ <emoji> <session> ─ <line>
    [suggest: <enriched>]
    ① <option>  ② <option>  ③ <option>      (ASK only)
```

Rules:
- Banner always present (0 → "0 NEED YOU", skip top line).
- One `▸` card per `cards[]` entry, in order (already priority-sorted).
- `[suggest: …]` line only when `enriched` is non-null.
- ASK cards show `options` as ① ② ③ ④ ⑤.
- Cap at 10 cards; if more, render 10 then `+ N more`.

## Step 3 — fire AskUserQuestion (batched ≤ 4)

`asks[]` are AskUserQuestion-ready. **The tool caps at 4 questions per call** —
batch in groups of 4 (highest-priority first, asks[] is already sorted):

```
for batch of up to 4 asks:
  AskUserQuestion({ questions: batch.map(a => ({
    question: a.question, header: a.header,
    options: a.options, multiSelect: a.multiSelect })) })
```

When `suggestion` is set, mention it ("recommended: <suggestion>") so Stevie
can one-tap the drafted choice.

## Step 4 — route answers back

Two routes, picked by ask `kind`:

### 4a — ASK kind → **key-route** (click the target's picker)

The target session already has its own AskUserQuestion picker open and is
waiting for `Down`/`Enter`. Sending the answer as TEXT would land as a fresh
user message the target then has to re-interpret. Worse, the broker-delivery
path has a known gap. So for ASK answers, **press the picker keys directly via
tmux send-keys** — the same UI keystrokes the human would use:

```bash
# idx = 0-based index of the option Stevie picked in asks[].options
# target = the ask's route.target (the target's tmux_session)
for _ in $(seq 1 "$idx"); do tmux send-keys -t "$target" Down; done
tmux send-keys -t "$target" Enter
```

The picker explicitly advertises `Enter to select · Tab/Arrow keys to navigate`
in its footer — that's the contract.

**Caveats:**
- The picker must still be the active surface in the target pane. If the
  target moved on (Esc'd / scrolled away), arrow-Enter lands somewhere
  unintended. `route.hint == 'broker'` doesn't help — the underlying race
  is the same.
- Option ordering matters and is preserved by the workflow (the target's
  `context.options[]` is returned verbatim).

### 4b — ERR / IDLE / WAIT kinds → **text-route via tmux send-keys**

No picker on the target; the answer is a normal prompt. Per the read/write
principle, prefer tmux directly — it lands reliably and is verifiable via
capture-pane. Broker is fallback only.

```bash
# default: write via tmux, verify via capture-pane
tmux send-keys -t "$target" -l "<answer text>"
tmux send-keys -t "$target" Enter

# verify it landed in the pane (the matching read)
tmux capture-pane -t "$target" -p -S -40 | grep -F "<answer text>" && echo "✓"
```

Only when `tmux_session` is unknown (rare — bg jobs, dead tmux), fall back to:

```bash
ainb fleet broadcast "<answer>" --filter "<route.target>"   # broker — last resort
```

### Verify the write landed (capture-pane is the matching read)

```bash
# the read leg that matches the write
tmux capture-pane -t "$target" -p -S -50 | grep -F "<answer>"
# slower confirmation: target's JSONL transcript records the user message
ls -t ~/.claude/projects/<cwd-slug>/*.jsonl | head -1 | xargs grep -F "<answer>"
```

## Caveats

- **AskUserQuestion is session-only** — the workflow cannot fire it; that's why
  this skill exists. The workflow only produces the `asks[]` shape.
- **Enrich cost** — one Haiku agent per blocked session, unbounded. A 17-session
  fleet measured ~920k tokens / ~175s on a cold run; resume is ~0 tokens.
- **Race window** — a just-answered session may reappear once before its tail
  moves; always-fresh discover re-reads live each invocation.
- **`unknown` session targets** — sessions ainb can't name (bg jobs, dead tmux)
  surface with `route.target: "unknown"`; those can't auto-route — tell Stevie.
