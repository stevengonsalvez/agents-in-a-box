---
name: ainb-fleet
description: >
  Fleet orchestration via the `ainb fleet ...` Rust subcommand namespace.
  Use when an LLM session needs to: (1) list every claude session running
  on the host (standup), (2) fan a prompt out to many sessions at once
  (broadcast), (3) drive an ordered ack-gated multi-step interaction
  (sequence), (4) surface every session waiting on input (needs), or
  (5) run a background watcher that auto-continues sessions hitting API
  errors (daemon). All subcommands default to `--format json` output for
  programmatic consumption.
user-invocable: false
allowed-tools:
  - Bash
---

# ainb fleet — orchestration cheatsheet

`ainb fleet` is the orchestration namespace inside the ainb Rust binary. It
fans prompts across many running claude sessions and reports their state
across three discovery sources (ainb + claude-peers broker SQLite + bg jobs).

This skill is *docs-only*: no code, no MCP server. It teaches you when to
reach for which subcommand and how to compose them.

## Subcommand reference

```
ainb fleet --help

  standup     list every claude session (ainb + peers + jobs, merged + deduped)
  broadcast   send one prompt to selected sessions
  sequence    ordered multi-step prompts, ack-gated by JSONL turn-end
  needs       enumerate sessions blocked on input / errors / waiting
  daemon      watcher that auto-continues sessions hitting API errors
```

Global flag: `--format json|text|csv|markdown` (default `text`). Prefer
`--format json` when an LLM is the consumer.

## When to use which

### Discovery — `ainb fleet standup`

```bash
ainb fleet --format json standup
```

Returns the unified session list. Each row: `id`, `cwd`, `tmux_session`,
`workspace_name`, `peer_id`, `sources` (subset of `["ainb","peers","jobs"]`),
`summary`, `last_seen_ms`. `sources` reveals which discovery layers saw
the session — a session in both `ainb` and `peers` is the strongest signal.

Use this when:
- You need to enumerate every claude on the host before composing an action
- You want to know which sessions have a registered peer (can be sent
  prompts via the broker) vs tmux-only (fallback path)
- You're building a `needs` view manually before the `needs` subcommand
  matures

### Fan-out — `ainb fleet broadcast`

```bash
ainb fleet broadcast "remote-control disconnect" --all
ainb fleet broadcast "/status" --filter "shotclubhouse.*"
ainb fleet broadcast "/clear" --cwd "agents-in-a-box"
```

Mandatory target selector: `--all`, `--filter <regex>` (matches tmux
session or workspace name), or `--cwd <substring>`. The command refuses
to run without one — no implicit fan-out.

Routing per target: peers-first (broker HTTP) when a peer_id is known,
tmux send-keys fallback otherwise. Output reports per-target outcome.

### Ordered multi-step — `ainb fleet sequence`

```bash
ainb fleet sequence "remote-control disconnect" "remote-control" "/status" --all
```

Sends each step, waits for every target's next assistant-turn-end (via
JSONL transcript watch) before sending the next step. Default per-step
timeout 300s.

Use this when a series of prompts must be applied in order *and* each
needs the prior to complete first — e.g. disconnect/reconnect cycles,
multi-step config changes, sequential `/clear` then resume flows.

### Blocked sessions — `ainb fleet needs`

```bash
ainb fleet --format json needs
```

Surfaces sessions whose `summary` starts with `WAITING:` — sessions
explicitly opted in to flag blockage. (Future: detect AskUserQuestion
UIs in tmux panes + idle-assistant heuristics.)

Pipe this output into a follow-up LLM call to compose answers for each
blocked session, then route the answers back via `broadcast --filter`.

### Auto-continue watcher — `ainb fleet daemon`

```bash
ainb fleet daemon --verbose
```

Long-running background process. Scans every 5s. For each tmux session
whose last 80 pane lines match a known API-error pattern (rate-limited,
overloaded, internal-server-error, timeout, ECONNRESET, etc.), sends
`continue` to that session. Per-pattern dedupe within a session so it
doesn't spam.

**Sharp edge — no retry cap in v0.1.** If a session is permanently broken
(e.g. wrong credentials), the daemon will keep firing `continue` forever.
Kill the daemon (`Ctrl-C` or `kill <pid>`) when this happens. A retry
cap with exponential backoff is on the roadmap.

## Composition patterns

### "What does my fleet need from me right now?"

```bash
ainb fleet --format json needs \
  | jq '.[] | {name: (.tmux_session // .workspace_name), summary: .summary}'
```

Then compose follow-ups based on each summary.

### "Apply a one-shot fix to every related session"

```bash
ainb fleet --format json standup \
  | jq -r '.[] | select(.cwd | contains("shotclubhouse")) | .tmux_session' \
  | head -3
# verify the targets…
ainb fleet broadcast "git pull" --filter "shotclubhouse"
```

### "Cycle remote-control across the fleet"

```bash
ainb fleet sequence \
  "remote-control disconnect" \
  "remote-control" \
  --all
```

## Discovery sources

| source | what it sees | when missing |
|---|---|---|
| `ainb` | every session ainb has spawned (`ainb run` family) | claude started outside ainb |
| `peers` | every session that registered with the claude-peers MCP broker (sqlite `~/.claude-peers.db`) | broker daemon not running |
| `jobs` | bg-session job dirs (`~/.claude/jobs/<id>/`) | no bg jobs active |

The merge step dedupes by `cwd`. A session present in two sources merges
into one record with `sources: ["ainb", "peers"]` etc.

## Environment overrides

| var | default | use |
|---|---|---|
| `AINB_BIN` | `ainb` | override the binary `discover/ainb.rs` shells to (tests) |
| `AINB_FLEET_PEER_ID` | `ainb-fleet-cp` | peer id the daemon registers as |
| `AINB_FLEET_JOBS_DIR` | `~/.claude/jobs` | bg-job scan root |
| `CLAUDE_PEERS_DB` | `~/.claude-peers.db` | broker sqlite path |
| `CLAUDE_PEERS_PORT` | `7899` | broker HTTP port |

## Migration note

This skill replaces the deprecated `popa` plugin. The Node/TS reference
impl was used to design the surface — it has been deleted. All
orchestration lives in `ainb` itself now.

## See also

- `ainb list` — lifecycle-only session list (no peer/jobs enrichment, no
  state derivation). Use when you only care about ainb-spawned sessions.
- `ainb attach <workspace>` — drop into a session's tmux directly.
- `ainb status <workspace>` — single-session inspect.
