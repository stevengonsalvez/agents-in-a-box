---
name: ainb-fleet:standup
description: |
  Show fleet status — every claude session running on the host, merged
  across ainb + claude-peers broker + background jobs. Use when you need
  to enumerate sessions before composing an action, see which sessions
  have a peer registered (broker-routable) vs tmux-only, check the
  `summary` of each session, or pipe the list into jq for filtering.
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
```

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

**Only sessions with peer registration (broker-routable):**
```bash
ainb fleet --format json standup | jq '.[] | select(.peer_id != null)'
```

**Group by source mix:**
```bash
ainb fleet --format json standup \
  | jq -r 'group_by(.sources | sort | join(",")) | .[] | "\(.[0].sources | sort | join(",")): \(length)"'
```

## Performance

Sub-100ms cold start. Reads broker SQLite directly + shells out to `ainb list`
once. Safe to call frequently.
