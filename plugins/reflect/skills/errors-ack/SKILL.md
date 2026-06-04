---
name: reflect:errors-ack
description: |
  Triage and acknowledge entries in the reflect errors sink
  (~/.reflect/errors.json). Invoked from the statusline ⚠N badge when
  pipeline errors accumulate (drain poison, parser crashes, ingest
  failures, hook timeouts).
version: "1.0.0"
user-invocable: true
triggers:
  - reflect:errors-ack
  - errors-ack
  - ack reflect errors
  - clear reflect errors
  - what are the reflect errors
allowed-tools:
  - Read
  - Bash
  - AskUserQuestion
---

# Reflect: Errors Ack

Triages unacknowledged entries in `~/.reflect/errors.json` and
acknowledges them (individually or in bulk). The statusline `⚠N
/reflect:errors-ack` badge is the entry point — clicking the badge or
typing `/reflect:errors-ack` lands here.

## When to Use

- Statusline shows `⚠N /reflect:errors-ack` badge
- User asks "what are the reflect errors", "what's broken in reflect"
- After a known fix has landed and user wants to clear stale errors
- Periodic triage of accumulated pipeline noise

## What This Skill Does

1. Load `~/.reflect/errors.json` and filter to unacked entries
2. Render a triage table (id · ts · source · kind · short message)
3. Group entries by `kind` so repeats are obvious
4. Ask the user (via `AskUserQuestion`):
   - **Ack all** — wipe the badge, suitable when the user has already
     fixed the root cause
   - **Ack by kind** — clear a specific failure class while keeping
     others visible (e.g. ack all `drain_poison` after fixing the parser)
   - **Show details** — print the full message + traceback for an entry
   - **Leave alone** — exit without changes
5. Run `reflect errors ack [ids...]` and report the count
   of records flipped to `acked: true`

## Triage table format

```
id           when     source   kind             message (first 80 chars)
─────────────────────────────────────────────────────────────────────────
err-b177eb   05-17    drain    drain_poison     poison after 3 retries: …
err-614742   05-17    drain    drain_poison     poison after 3 retries: …
err-579ed4   05-17    drain    drain_no_output  claude -p produced no out…
err-c942a3   05-17    drain    drain_poison     poison after 3 retries: …
```

Group repeats by kind in the prompt: "4 drain_poison + 1 drain_no_output
— ack all? ack just drain_poison? show one in detail?"

## Backend commands

Use the installed `reflect` CLI (from `uv tool install reflect-kb`). If `reflect`
isn't on `$PATH`, self-bootstrap with `uv run --with reflect-kb python -m
reflect_kb.errors <args>` — the bare `python3 -m reflect_kb.errors` form only
works when reflect_kb is importable by *system* python3, which it usually isn't.

```bash
# Count of unacked entries (what the statusline badge reads)
reflect errors count

# Ack all unacked entries
reflect errors ack

# Ack specific entries by ID
reflect errors ack err-b177eb err-614742

# Append a new error (used by hooks/scripts, not user)
reflect errors append \
  --source drain --kind drain_poison --message "…" --context '{}'
```

The store at `~/.reflect/errors.json` is locked via `fcntl` so it's
safe to ack and append concurrently. Entries are deduplicated within a
short window (same kind + same hash) to prevent loops from flooding it.

## Implementation

When invoked:

```bash
# 1. show the count + table
reflect errors count
python3 <<'PY'
import json, datetime
with open('/Users/stevengonsalvez/.reflect/errors.json') as f:
    d = json.load(f)
unacked = [e for e in d.get('errors', []) if not e.get('acked')]
for e in unacked[:20]:
    eid = e.get('id', '?')
    ts = e.get('timestamp', e.get('ts', '?'))[:10]
    src = (e.get('source') or '?')[:10]
    kind = (e.get('kind') or '?')[:18]
    msg = (e.get('message') or '')[:80]
    print(f"{eid:12} {ts:10} {src:10} {kind:18} {msg}")
PY

# 2. ask user via AskUserQuestion (ack all / ack by kind / show detail / leave)

# 3. run ack
reflect errors ack [optional-ids]
```

After ack the badge disappears on next statusline refresh (10s cache).

## Output template

```
Found N unacked errors in ~/.reflect/errors.json.

By kind:
  drain_poison      ×4
  drain_no_output   ×1

Acked M entries. Badge will clear within 10s.
```

## When NOT to use this skill

- Don't ack errors blindly — at least skim the kinds. Repeated
  `parser_typeerror` or `ingest_*` failures usually point at a real bug
  that needs fixing before acking (otherwise the same error returns
  next session).
- Don't delete `~/.reflect/errors.json` to clear the badge — that loses
  the history. Always ack.

## Related

- `/reflect-status` — broader system health view
- `/reflect:recall` — search learnings (separate concern, not errors)
- The errors sink itself: `~/.reflect/errors.json`
- Statusline badge: `⚠N /reflect:errors-ack` (rendered red, only when
  `count > 0`)
