---
name: ainb-fleet:standup
description: |
  Show fleet status — every claude session running on the host, merged
  across ainb + claude-peers broker + background jobs. Use when you need
  to enumerate sessions before composing an action, check the `summary`
  of each session, or pipe the list into jq for filtering. (Writes go
  via tmux by default; a `peer_id` is just an extra discovery signal /
  fallback channel, not a routing requirement.)
  Default output: text table. Pass --format json for LLM consumption.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:standup
  - fleet standup
  - fleet status
  - list claude sessions
  - what claude sessions are running
allowed-tools:
  - Bash
---

# ainb fleet:standup

List every claude session across the host. Three sources merged + deduped
by cwd.

## Run

```bash
ainb fleet standup                       # text table (default)
ainb fleet --format json standup         # JSON for piping
ainb fleet standup --no-enrich           # 0-token roster (env AINB_FLEET_ENRICH=0)
```

The workflow-backed briefing (`/ainb-fleet:hangar` verb=standup) drafts the
per-session "what it's doing" lines in a **single batched agent**, not one per
session. `--no-enrich` skips that entirely.

## Output fields (JSON)

| field | meaning |
|---|---|
| `id` | session id (peer id preferred, else ainb session_id, else bg job id) |
| `cwd` | working dir — primary dedupe key |
| `pid` | OS pid (when known — peers + bg jobs publish; ainb does not) |
| `tmux_session` | tmux session name if running in tmux |
| `workspace_name` | ainb workspace name |
| `worktree_path` | ainb worktree path |
| `peer_id` | broker peer id (if registered) |
| `sources` | array — subset of `["ainb","peers","jobs"]` |
| `summary` | peer-published summary (may start with `WAITING:` to flag block) |
| `last_seen_ms` | unix ms of last activity any source observed |

## Composition patterns

**Just names:**
```bash
ainb fleet --format json standup | jq -r '.[].tmux_session // .workspace_name'
```

**Sessions in a specific repo:**
```bash
ainb fleet --format json standup | jq '.[] | select(.cwd | contains("shotclubhouse"))'
```

**Sessions reachable by the default tmux transport:**
```bash
ainb fleet --format json standup | jq '.[] | select(.tmux_session != null)'
```

**Sessions with a broker peer (fallback-routable):**
```bash
ainb fleet --format json standup | jq '.[] | select(.peer_id != null)'
```

**Group by source mix:**
```bash
ainb fleet --format json standup \
  | jq -r 'group_by(.sources | sort | join(",")) | .[] | "\(.[0].sources | sort | join(",")): \(length)"'
```

## Auto-chain to `/ainb-fleet:needs`

After rendering standup, **scan the result for sessions whose `summary`
contains `AskUserQuestion`** (literal string — the JSONL-synthesised
summary surfaces this when the last assistant turn fired the tool).

If ANY such session exists, the standup MUST NOT stop at the standup
table. Immediately:

1. Tell Stevie explicitly:
   ```
   N session(s) blocked on AskUserQuestion — firing `/ainb-fleet:needs`
   to surface the questions + answer them in one batch.
   ```

2. Invoke the `ainb-fleet:needs` skill (via the Skill tool) OR run
   `ainb --format json fleet needs` directly and render the Jarvis HUD
   per the `ainb-fleet:needs` SKILL.md. No second Stevie-typed slash
   command required — the handover is automatic.

Detection one-liner:

```bash
ainb --format json fleet standup \
  | jq -r '[.[] | select(.summary // "" | contains("AskUserQuestion"))] | length'
```

If 0 → skip the chain, standup is the final word.
If >0 → chain immediately.

**Rationale:** standup is a snapshot; the moment it reveals an
answerable question, the fleet's center control panel (`needs`)
already knows how to extract the structured options + route the
answer back. Don't make Stevie type another slash command for a
mechanically inevitable next step.

## Performance

Sub-100ms cold start. Reads broker SQLite directly + shells out to `ainb list`
once. Safe to call frequently.
