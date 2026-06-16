---
name: reflect:slots
description: |
  Read and edit the pinned memory slots — a fixed set of named, size-capped,
  agent-editable scratchpads (persona, user_preferences, tool_guidelines,
  project_context, guidance, pending_items, session_patterns, self_notes).
  Slots are the agent's fast working memory: they auto-inject at SessionStart
  ahead of any recall results. Use to record durable preferences, project
  context, pending work, or notes-to-self mid-session.
version: "1.0.0"
user-invocable: true
triggers:
  - reflect:slots
  - memory slots
  - memory slot
  - pinned slots
  - update my scratchpad
  - remember this for next session
allowed-tools:
  - Bash
  - Read
  - AskUserQuestion
---

# Reflect: Slots — pinned editable memory

A small fixed set of named, size-limited scratchpad slots stored in
`~/.reflect/reflect.db` (table `slots`). They sit between skills
(workflow-shaped, slow to refresh) and learnings (aggregated from
corrections): slots are the **agent's working memory**, editable
mid-session, injected at the top of every SessionStart context
(Tier-0, before skills and recall results) when `REFLECT_SLOTS=1`.

## The 8 default slots

| slot | scope | cap (chars) | what belongs there |
|------|-------|-------------|--------------------|
| `persona` | global | 1000 | Role, voice, operating principles |
| `user_preferences` | global | 2000 | Durable user habits: style, naming, tooling |
| `tool_guidelines` | global | 1500 | Tool selection / sequencing rules |
| `project_context` | project | 3000 | Architecture notes, conventions, build/test commands |
| `guidance` | project | 1500 | Steering for the next session: focus, hazards, risks |
| `pending_items` | project | 2000 | Unfinished work and TODOs (Stop hook auto-appends) |
| `session_patterns` | project | 1500 | Recurring behaviours (Stop hook auto-counts) |
| `self_notes` | project | 1500 | Hypotheses, dead ends, follow-ups |

Global slots apply in every project; project slots are keyed by the git
repo name (fallback: cwd basename) and shadow a same-named global slot.
All eight are seeded automatically on first use — empty until edited.

## Operations (memory_slot_*)

All operations go through the reflect_db CLI. `--project` defaults to
the current repo, so it can normally be omitted.

```bash
# memory_slot_list — every slot visible here, with fill levels
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/reflect_db.py slot-list

# memory_slot_get — full JSON for one slot
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/reflect_db.py slot-get \
  --name pending_items

# memory_slot_append — add a line (errors if it would blow the size cap)
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/reflect_db.py slot-append \
  --name pending_items --text "- wire the retry budget into the drain loop"

# memory_slot_replace — rewrite the whole slot (compact when near the cap)
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/reflect_db.py slot-replace \
  --name guidance --content "Focus: finish A1 port. Avoid: touching uv.lock."

# memory_slot_delete — empty a slot (the named slot itself survives)
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/reflect_db.py slot-delete \
  --name self_notes
```

## Rules

- **Size caps are hard** for agent edits: an append that would exceed the
  slot's `size_limit` fails with an error — `slot-replace` with a
  compacted body instead of retrying the append.
- **Read-only slots** (`read_only=1`) reject every edit; only `slot-get`
  and `slot-list` work on them.
- **Fixed vocabulary**: `slot-delete` empties a slot but never removes
  the named row. Stick to the 8 defaults — that is the contract the
  SessionStart inject and the Stop-hook auto-append rely on.
- **Keep entries terse.** Slots are injected into every session start;
  every char spent here is context spent everywhere.

## Automation around the slots

- **SessionStart** (`skills/recall/hooks/session_start_recall.py`): when
  `REFLECT_SLOTS` is truthy, every non-empty slot injects as a markdown
  block *before* any skill hit or recall result (Tier-0 of R10).
- **Stop hook** (`hooks/stop_reflect.py`): deterministic, no-LLM pass
  over the transcript — open TODOs append to `pending_items`, tool-usage
  counts summarize into `session_patterns`, touched files accumulate in
  `project_context`. Auto-writers dedupe and tail-truncate within the cap.

## When the user invokes /reflect:slots

1. Run `slot-list` and show the table (name, scope, fill level, description).
2. Ask (via `AskUserQuestion`) whether they want to view, append, replace,
   or clear a slot — or just leave it.
3. Apply the edit with the matching command above and confirm the new
   fill level.
