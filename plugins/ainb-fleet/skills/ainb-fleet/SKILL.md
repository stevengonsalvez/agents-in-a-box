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
discover (ainb + peers + jobs)  ──▶  state read  ──▶  send (tmux-first, broker fallback)
```

## Global flag

`--format json|text|csv|markdown` (default `text`). Prefer `--format json`
when an LLM is the consumer.

## Transport — tmux is primary

Writes (broadcast / sequence / daemon / answer-routing) go out over **tmux
send-keys by default**. The claude-peers broker is opt-in / fallback only,
because in practice it has proven flaky (silent delivery gaps). The primitives
the fleet uses are exactly: `tmux send-keys -l` (write), `tmux capture-pane -p
-S -<n>` (read, incl. scrollback), and the session JSONL transcript
(`~/.claude/projects/<cwd-slug>/<sid>.jsonl`) as ground-truth read.

Select the write channel with `AINB_FLEET_TRANSPORT`:

| value | behaviour |
|---|---|
| unset / `tmux` / `tmux-first` | tmux send-keys first, broker fallback (**default**) |
| `tmux-only` | tmux send-keys only — never touch the broker |
| `peers` / `broker` / `peers-first` | legacy: broker first, tmux fallback |

```
            ┌──────────────┐  tmux send-keys -l   ┌─────────────┐
  send ────▶│ tmux pane?   │─────────────────────▶│  delivered  │
            └──────┬───────┘                       └─────────────┘
                   │ no live pane (or peers-first)
                   ▼
            ┌──────────────┐  broker /send-message ┌─────────────┐
            │ peer + broker│──────────────────────▶│  delivered  │
            │   healthy?   │                        └─────────────┘
            └──────────────┘
```

## Discovery sources

Discovery is independent of the write transport — peers are still *read* for
visibility (PID, `summary`, `WAITING:` markers) even when writes go via tmux.

- **ainb** — every session ainb has spawned via `ainb run`
- **peers** — sessions registered with the claude-peers broker (`~/.claude-peers.db`),
  read directly from its sqlite (no HTTP), so discovery survives a down broker
- **jobs** — bg-session dirs under `~/.claude/jobs/`

Merged + deduped by `cwd` so the same session in two sources collapses
into one record with `sources: ["ainb", "peers"]`.

## Environment overrides

| var | default | use |
|---|---|---|
| `AINB_BIN` | `ainb` | override binary the discover layer shells to (tests) |
| `AINB_FLEET_TRANSPORT` | `tmux-first` | write channel: `tmux` / `tmux-only` / `peers` |
| `AINB_FLEET_PEER_ID` | `ainb-fleet-cp` | from-id used when a write does fall back to the broker |
| `AINB_FLEET_JOBS_DIR` | `~/.claude/jobs` | bg-job scan root |
| `CLAUDE_PEERS_DB` | `~/.claude-peers.db` | broker sqlite path (discovery) |
| `CLAUDE_PEERS_PORT` | `7899` | broker HTTP port (fallback writes only) |

## See also

- `ainb list` — lifecycle-only session list (no peer/jobs enrichment)
- `ainb attach <workspace>` — drop into a session's tmux
- `ainb kill <workspace>` — terminate a single session by exact name
