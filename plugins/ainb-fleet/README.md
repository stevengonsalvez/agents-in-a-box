# ainb-fleet

**A control plane for every coding-agent session running on your machine.**

`ainb-fleet` lets one agent (or you) see and drive a whole fleet of interactive
CLI coding agents — Claude Code, Codex, Gemini, Copilot, or anything else that
runs in a terminal — from a single place. It enumerates every live session,
tells you which ones are blocked waiting on you, fans a prompt out to many at
once, runs ordered multi-step sequences, and auto-recovers sessions that hit
transient API errors.

It is **agent-agnostic by design**: it talks to sessions over the terminal
itself (tmux), so it does not care which agent is running in the pane.

```
                         ┌─────────────────────────────┐
                         │        ainb fleet           │
                         │  standup · needs · broadcast │
                         │  sequence · daemon           │
                         └──────────────┬──────────────┘
              read (capture-pane + JSONL)│ write (send-keys)
        ┌───────────────┬───────────────┼───────────────┬──────────────┐
        ▼               ▼               ▼               ▼              ▼
   ┌─────────┐    ┌─────────┐     ┌─────────┐     ┌─────────┐    ┌─────────┐
   │ Claude  │    │ Codex   │     │ Gemini  │     │ Copilot │    │ any CLI │
   │ (tmux)  │    │ (tmux)  │     │ (tmux)  │     │ (tmux)  │    │ (tmux)  │
   └─────────┘    └─────────┘     └─────────┘     └─────────┘    └─────────┘
```

---

## Why "over PTY"?

Most agent-orchestration tools require a special protocol on both ends (the
agent must speak it, you must adopt it). `ainb-fleet` takes the opposite bet:
**the terminal is the universal interface.** Every interactive coding agent
already reads keystrokes and prints to a pane. So the fleet:

- **Writes** with `tmux send-keys -l` — literal keystrokes into the target pane,
  exactly what a human would type. Works for any agent, any prompt, no
  injection risk.
- **Reads** with `tmux capture-pane` (including scrollback) — to see what the
  agent printed, detect API errors, and read its current state.
- **Reads, precisely,** the session's JSONL transcript when available — to pull
  structured signals (a fired `AskUserQuestion`, an assistant turn that ended
  with no follow-up) that a screen-scrape can't reliably give you.

Because the transport is the terminal, **the same commands orchestrate Claude,
Codex, Gemini, Copilot, and any future agent** without per-agent integration.

### Cross-agent capability matrix

| Capability | Claude | Codex | Gemini | Copilot | Any PTY agent |
|---|:---:|:---:|:---:|:---:|:---:|
| Send a prompt (`send-keys`) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Read pane / detect API errors (`capture-pane`) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Broadcast / sequence / auto-continue daemon | ✓ | ✓ | ✓ | ✓ | ✓ |
| Precise ASK / IDLE signal from JSONL | ✓ | pane¹ | pane¹ | pane¹ | pane¹ |
| Resume a stopped session | ✓² | fresh | fresh | fresh | fresh |

¹ Agents without a Claude-style JSONL transcript fall back to pane-text
heuristics for ASK/IDLE; ERR (pane) and WAIT (summary) work for all.
² Claude resumes via `--resume <jsonl path>`; other agents start a fresh session.

---

## The verbs

| Sub-skill | What it does |
|---|---|
| [`/ainb-fleet:standup`](skills/standup/SKILL.md) | List every session — merged across ainb · peers · bg jobs |
| [`/ainb-fleet:needs`](skills/needs/SKILL.md) | Show sessions blocked on you (ASK / ERR / IDLE / WAIT) |
| [`/ainb-fleet:broadcast`](skills/broadcast/SKILL.md) | Send one prompt to many selected sessions |
| [`/ainb-fleet:sequence`](skills/sequence/SKILL.md) | Ordered multi-step prompts, ack-gated between steps |
| [`/ainb-fleet:daemon`](skills/daemon/SKILL.md) | Background watcher that auto-continues API errors |
| [`/ainb-fleet:fleet-needs`](skills/fleet-needs/SKILL.md) | Workflow-backed Jarvis HUD over `needs` |

All verbs are also raw CLI subcommands of the `ainb` binary:

```bash
ainb fleet standup                     # text table (--format json for piping)
ainb fleet needs                       # Jarvis HUD of blocked sessions
ainb fleet broadcast "/clear" --all    # fan-out (needs an explicit target flag)
ainb fleet sequence "step1" "step2" --all
ainb fleet daemon --verbose            # unattended API-error recovery
```

---

## Transport — tmux-first, broker optional

Writes go out over **tmux send-keys by default.** A session-to-session message
broker (claude-peers) is supported as an opt-in fallback. Pick the channel with
`AINB_FLEET_TRANSPORT`:

```
            ┌──────────────┐  tmux send-keys -l   ┌───────────┐
  send ────▶│ live tmux     │────────────────────▶│ delivered │
            │ pane?         │                      └───────────┘
            └──────┬───────┘
                   │ no pane (or transport=peers)
                   ▼
            ┌──────────────┐  broker /send-message ┌───────────┐
            │ peer + broker │─────────────────────▶│ delivered │
            │  healthy?     │                       └───────────┘
            └──────────────┘
```

| `AINB_FLEET_TRANSPORT` | behaviour |
|---|---|
| unset / `tmux` / `tmux-first` | tmux send-keys first, broker fallback (**default**) |
| `tmux-only` | tmux send-keys only — never touch the broker |
| `peers` / `broker` | legacy broker-first, tmux fallback |

Verify any write landed the way you read everything else — straight from the pane:

```bash
tmux capture-pane -t "<session>" -p -S -40 | grep -F "<text>" && echo "✓"
```

---

## How signals are read

The `needs` verb classifies each session into one of four signal kinds. Reads
are layered — JSONL where it's authoritative, the pane where it isn't:

```
signal   primary source        fallback
─────    ──────────────        ────────────────────────
ASK   ─▶ JSONL (tool_use)      —
IDLE  ─▶ JSONL (turn-end)      —
ERR   ─▶ tmux pane (80 ln)     JSONL newest 40 rows (on pane miss)
WAIT  ─▶ peers summary         —
```

Priority when more than one fires: **ASK > ERR > IDLE > WAIT.**

---

## Token efficiency

The fleet is built to be cheap to run, even across a large host:

- **The whole Rust read/classify path is LLM-free.** Discovery, pane capture,
  JSONL parsing and signal classification run as plain code and emit compact
  JSON with short snippets — zero model tokens.
- **Enrichment is batched, not fanned out.** When sessions get an AI-drafted
  "suggested answer", the work is done in **one** pass over all blocked cards,
  not one subagent per session.

```
                blocked cards
                     │
        ≤ 6 ─────────┼───────── > 6
         │                       │
   draft inline             one batched
   (0 subagents)            subagent call
         └───────────┬───────────┘
                     ▼
            enrich-cache (sha(ctx))
        unchanged card  ─▶  reused, 0 tokens
```

- **Hybrid locus.** Small fleets draft suggestions inline (no subagent spawned);
  large fleets isolate the work into a single batched subagent. Cutoff is
  `AINB_FLEET_ENRICH_INLINE_MAX` (default `6`).
- **Content-hash cache.** Each enrichment is cached by a hash of its exact input
  context (`~/.agents-in-a-box/fleet/enrich-cache.json`). A session that hasn't
  advanced is never re-enriched — its card renders for free. The cache
  self-invalidates the instant the context changes; eviction is LRU.
- **Instant 0-token mode.** `--no-enrich` (or `AINB_FLEET_ENRICH=0`) renders the
  HUD straight from the Rust JSON with no model calls at all.

---

## Install

`ainb-fleet` ships as a plugin in the agents-in-a-box marketplace and is backed
by the `ainb` Rust binary.

```bash
# the fleet verbs live in the ainb binary
ainb fleet --help

# the LLM-facing skills are deployed with the plugin; invoke from a session:
/ainb-fleet:standup
/ainb-fleet:needs
```

---

## Discovery sources

Sessions are merged + deduped by `cwd` across three sources:

- **ainb** — every session spawned via `ainb run`
- **peers** — sessions registered with the claude-peers broker (read directly
  from its sqlite, so discovery survives a down broker)
- **jobs** — background-session dirs under `~/.claude/jobs/`

A session present in two sources collapses into one record with
`sources: ["ainb", "peers"]`.

---

## Environment variables

| var | default | use |
|---|---|---|
| `AINB_FLEET_TRANSPORT` | `tmux-first` | write channel: `tmux` / `tmux-only` / `peers` |
| `AINB_FLEET_ENRICH` | `1` | set `0` to disable enrichment globally (= `--no-enrich`) |
| `AINB_FLEET_ENRICH_INLINE_MAX` | `6` | blocked-card count: inline below, one batched agent above |
| `AINB_FLEET_IDLE_MIN` | `5` | minutes before an idle session is surfaced |
| `AINB_FLEET_PEER_ID` | `ainb-fleet-cp` | from-id used when a write falls back to the broker |
| `AINB_FLEET_JOBS_DIR` | `~/.claude/jobs` | background-job scan root |
| `AINB_BIN` | `ainb` | override the binary the discover layer shells to (tests) |
| `CLAUDE_PEERS_DB` | `~/.claude-peers.db` | broker sqlite path (discovery) |
| `CLAUDE_PEERS_PORT` | `7899` | broker HTTP port (fallback writes only) |

---

## Roadmap

**Now (landing)**

- [x] tmux-first transport with `AINB_FLEET_TRANSPORT` toggle
- [x] Batched enrichment — collapse per-session subagents into ≤1 call
- [x] Hybrid inline / batched enrich locus by fleet size (`AINB_FLEET_ENRICH_INLINE_MAX`)
- [x] Content-hash enrich cache (`sha(ctx)`, LRU, no TTL)
- [x] JSONL fallback for ERR detection on pane miss
- [x] Unified enrich path across `needs` and `standup`
- [x] `--no-enrich` instant 0-token HUD

**Next**

- [ ] Per-session retry cap + backoff in the daemon
- [ ] Filter / cwd targeting for `sequence` (currently `--all` only)
- [ ] Answer-routing dedupe (a just-answered session shouldn't reappear once)
- [ ] Richer non-Claude JSONL adapters (Codex / Gemini transcript shapes)

---

## See also

- `ainb list` — lifecycle-only session list (no peer/jobs enrichment)
- `ainb attach <workspace>` — drop into a session's tmux
- `ainb kill <workspace>` — terminate a single session by exact name
- [`ainb fleet bridge`](skills/bridge/SKILL.md) — native phone bridge (Telegram /
  Slack / Discord): relay a named session two-way to your phone, installable as a
  launchd/systemd service
