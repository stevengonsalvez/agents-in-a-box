---
name: ainb-fleet
description: |
  Fleet orchestration overview — the `ainb fleet ...` Rust subcommand
  namespace for driving every claude session on the host. Routes to one of
  five sub-skills (standup / broadcast / sequence / needs / daemon).
  Invoke this for an at-a-glance map of what fleet can do; reach for the
  specific sub-skill for the verb you want.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet
  - fleet
  - jarvis
  - control plane
  - multi-session orchestration
allowed-tools:
  - Bash
---

# ainb fleet — overview

Single Rust binary (`ainb`) provides 5 orchestration verbs across every
claude session running on this host. Each verb has its own colon-namespaced
sub-skill with focused docs.

## Verbs

| sub-skill | what it does |
|---|---|
| [`/ainb-fleet:standup`](../standup/SKILL.md) | List every claude session — merged across ainb · peers · bg jobs |
| [`/ainb-fleet:broadcast`](../broadcast/SKILL.md) | Send one prompt to selected sessions |
| [`/ainb-fleet:sequence`](../sequence/SKILL.md) | Ordered multi-step prompts, ack-gated between steps |
| [`/ainb-fleet:needs`](../needs/SKILL.md) | Show sessions blocked on input / errors / waiting |
| [`/ainb-fleet:daemon`](../daemon/SKILL.md) | Background watcher that auto-continues API errors |

## Architecture (1-line)

```
discover (ainb + peers + jobs)  ──▶  state read  ──▶  send (peers-first, tmux fallback)
```

## Global flag

`--format json|text|csv|markdown` (default `text`). Prefer `--format json`
when an LLM is the consumer.

## Discovery sources

- **ainb** — every session ainb has spawned via `ainb run`
- **peers** — sessions registered with the claude-peers broker (`~/.claude-peers.db`)
- **jobs** — bg-session dirs under `~/.claude/jobs/`

Merged + deduped by `cwd` so the same session in two sources collapses
into one record with `sources: ["ainb", "peers"]`.

## Environment overrides

| var | default | use |
|---|---|---|
| `AINB_BIN` | `ainb` | override binary the discover layer shells to (tests) |
| `AINB_FLEET_PEER_ID` | `ainb-fleet-cp` | peer id the daemon registers as |
| `AINB_FLEET_JOBS_DIR` | `~/.claude/jobs` | bg-job scan root |
| `CLAUDE_PEERS_DB` | `~/.claude-peers.db` | broker sqlite path |
| `CLAUDE_PEERS_PORT` | `7899` | broker HTTP port |

## See also

- `ainb list` — lifecycle-only session list (no peer/jobs enrichment)
- `ainb attach <workspace>` — drop into a session's tmux
- `ainb kill <workspace>` — terminate a single session by exact name
