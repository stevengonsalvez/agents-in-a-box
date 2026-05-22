---
name: ainb-fleet:needs
description: |
  Enumerate every claude session that is blocked waiting on something —
  user input (AskUserQuestion), an API error retry, or a peer-published
  `WAITING:` summary. Use when you've stepped away and want to know
  "what does my fleet need from me right now?" Output is JSON suitable
  for piping into a follow-up LLM call that composes answers per
  session.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:needs
  - fleet needs
  - what does my fleet need
  - blocked sessions
  - sessions waiting on me
allowed-tools:
  - Bash
---

# ainb fleet:needs

List sessions whose state is "blocked / waiting on you" right now.

## Run

```bash
ainb fleet needs                          # JSON (default)
ainb fleet --format text needs            # text table
```

## What counts as "blocked" (v0.1)

Currently surfaces sessions whose peer-published `summary` field starts
with `WAITING:`. Sessions explicitly opt in by calling broker
`/set-summary` with the prefix.

## Roadmap signals (not yet wired)

The state-derivation layer recognises these signal types — once wired
into `needs`, they'll widen the surface:

| signal | source |
|---|---|
| `ApiError` | API-error regex match in tmux pane buffer |
| `AskUserQuestion` | AskUserQuestion UI box detected in pane |
| `NeedsInputMarker` | `needs input:` literal in bg-job output |
| `WaitingSummary` | `WAITING:` prefix in peer summary |
| `Idle` | assistant turn ended, no new user message for N min |

When a session has multiple signals, priority order picks the strongest:

```
ApiError  >  NeedsInput  >  WaitingSummary  >  none
```

## Compose answers per-session

```bash
ainb fleet --format json needs \
  | jq -r '.[] | "\(.tmux_session): \(.summary)"' \
  | while IFS= read -r line; do
      echo "Session $line — answer:"
      # … compose answer via LLM …
    done
```

Then route each answer back via `ainb fleet broadcast` with a per-session
filter.

## Caveats

- v0.1 needs sessions to opt-in via `WAITING:` summary. If your sessions
  don't set this, the list will be empty even when sessions are actually
  blocked.
- The richer detection (regex on tmux pane, JSONL idle heuristic) lives
  in the read layer but isn't wired into `needs` yet.
