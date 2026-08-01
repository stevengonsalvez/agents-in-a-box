---
title: "ainb-fleet"
description: "Claude Code skill bundle that teaches agents to drive the `ainb fleet` multi-session orchestration subcommands."
---

`ainb-fleet` (v0.1.0) is a **Claude Code plugin** — loaded by Claude Code itself, not by the ainb TUI — that ships a bundle of LLM-facing skills teaching an agent how to use the `ainb fleet ...` orchestration subcommands: broadcasting a prompt to many sessions, ack-gated step sequences, surfacing blocked sessions ("needs"), an auto-continue daemon, and a fleet standup. The orchestration logic itself lives in the `ainb` Rust binary; this plugin is purely the teaching layer that points the agent at the right command for the job. It replaces the deprecated `popa` plugin, whose Node code is gone.

## How it works

![ainb-fleet — how it works](../../assets/diagrams/ainb-fleet.svg)

This plugin registers **no hooks, no commands, and no agents** — it is a pure skill bundle. When you invoke one of its colon-namespaced skills (`/ainb-fleet:standup`, `/ainb-fleet:broadcast`, etc.), the skill teaches the agent the exact `ainb fleet ...` invocation to shell out to. All the real work — discovering sessions, reading transcripts, routing writes — happens inside the `ainb` Rust binary.

The fleet engine discovers sessions from three sources — every session `ainb` has spawned, sessions registered with the claude-peers broker (`~/.claude-peers.db`), and background-job dirs under `~/.claude/jobs/` — then merges and dedupes them by `cwd`. Reads go through the source of truth (JSONL transcripts plus tmux pane buffers); writes go out peers-first via the broker when a peer is registered, falling back to `tmux send-keys` literal mode otherwise.

The bundle has a top-level router skill (`ainb-fleet`) that maps the five verbs to their focused sub-skills, plus a workflow-backed variant of `needs`. The `fleet-needs` skill runs the deterministic `hangar` workflow (verb=`needs`: discover → enrich → prioritize → render-ready cards), then renders a "Jarvis HUD" of every blocked session and fires an `AskUserQuestion` per session so the user can answer the whole fleet in one batch; answers are routed back to each target's tmux pane.

The daemon skill describes a long-running watcher that scans each session's recent pane buffer every 5 seconds and auto-sends `continue` to any session whose buffer matches a known API-error regex (`rate_limited`, `overloaded_error`, `internal_server_error`, `request_timeout`, `socket_hang_up`, `fetch_failed`, `connection_reset`), deduping on `(session_id, pattern, match-context)` so it fires once per error.

**ATC (Air Traffic Control)** is the persistent orchestrating brain that ties the verbs together on a schedule with a policy. `ainb fleet atc setup <name>` provisions `~/.agents-in-a-box/atc/<name>/` — a generated `CLAUDE.md` policy, a `meta.json` config, seeded `state.json` / `task-log.md` durable memory, and an installed OS-timer heartbeat (launchd `StartInterval` on macOS, a systemd `--user` timer on Linux) — then spawns a real `ainb run` Claude session reading that policy. Every N minutes (default 15) the timer runs `ainb fleet atc heartbeat <name>`, which builds a compact `[HEARTBEAT]` nudge from the **LLM-free** `ainb fleet needs --format json` read and tmux-sends it into the session, so ATC spends tokens only deciding. The policy is conservative and **escalate-on-uncertainty**: it auto-clears the safe cases (confident ASK → `broadcast`; ERR → `continue`, capped at 3 per session) and escalates the rest to the phone bridge, never auto-running destructive actions. ATC is **poll-mode** — it needs no hook/inbox plumbing and works against the existing fleet verbs today; the plumbing track is a drop-in event-driven enhancement. For managed fleets ATC **supersedes** the `daemon` skill, absorbing its auto-`continue` job with the retry cap the daemon lacks. `status` / `list` / `teardown` round out the lifecycle; teardown idempotently removes the timer and session.

## What it provides

### Skills

| Skill | Purpose |
|---|---|
| `ainb-fleet` | Overview / router — at-a-glance map of the five fleet verbs, discovery sources, and global flags |
| `ainb-fleet:ainb-spawn` | Spawn contract for `ainb run`: the two legal shapes (repo root plus `--worktree --create-branch`, or an existing worktree passed bare), the flag-by-flag rationale, the trap table (bare checkout, empty `--parent`, unset `--parent` eating the next token), and the post-spawn verify loop |
| `ainb-fleet:standup` | List every claude session on the host, merged + deduped across ainb · peers broker · bg jobs |
| `ainb-fleet:broadcast` | Fan one prompt out to selected sessions (requires `--all`, `--filter <regex>`, or `--cwd <substring>`) |
| `ainb-fleet:sequence` | Send ordered multi-step prompts, ack-gated between steps via JSONL turn-end detection |
| `ainb-fleet:needs` | Center control panel — enumerate sessions blocked on ASK / ERR / IDLE / WAIT signals, render the Jarvis HUD |
| `ainb-fleet:fleet-needs` | Workflow-backed `needs` — runs the `hangar` workflow, renders the HUD, fires `AskUserQuestion`, routes answers back |
| `ainb-fleet:cost` | Per-session / model / day / group USD spend rollups sourced from burndown, plus `config.toml` budget caps that fire notifyd alerts |
| `ainb-fleet:atc` | **Air Traffic Control** — provision / inspect / tear down the persistent fleet brain that watches on a heartbeat, auto-clears safe sessions, and escalates the rest to your phone |
| `ainb-fleet:daemon` | Background watcher that auto-`continue`s sessions matching an API-error regex (**superseded by ATC** for managed fleets) |

### Workflow

| Workflow | Purpose |
|---|---|
| `hangar` | Multi-verb deterministic orchestrator (verbs: `needs` / `standup` / `sequence`). Discover → enrich (per-session, Haiku by default) → prioritize / group / step. Backs the `fleet-needs` skill. |

### Hooks / Commands / Agents

None — this plugin registers no lifecycle hooks, slash commands, or sub-agents.

## Install

```sh
claude plugin install ainb-fleet@agents-in-a-box
```

The plugin is published via this repo's root `.claude-plugin/marketplace.json` (marketplace name `agents-in-a-box`). It requires the `ainb` binary on `PATH` to do any real work — the skills shell out to `ainb fleet ...`.

## Using it

- **Get a fleet roster:** `/ainb-fleet:standup` (add `--format json` to pipe into `jq`).
- **Apply one instruction everywhere:** `/ainb-fleet:broadcast` — e.g. `ainb fleet broadcast "/clear" --filter "shotclubhouse"`. A targeting flag is mandatory to prevent accidental fan-out.
- **Run an ordered cycle:** `/ainb-fleet:sequence` — e.g. disconnect → reconnect → verify, waiting for each step's assistant turn-end before the next.
- **See what wants your attention:** `/ainb-fleet:needs` (or `/ainb-fleet:fleet-needs` when `CLAUDE_CODE_WORKFLOWS=1`) renders a HUD of every blocked session and lets you answer them in one `AskUserQuestion` batch.
- **Unattended fleet supervision:** `/ainb-fleet:atc` (`ainb fleet atc setup <name>`) stands up the persistent brain — it watches the whole fleet on a heartbeat, clears the safe/blocked sessions itself, and pings your phone only for the calls that genuinely need you. Supersedes the daemon for managed fleets.
- **Unattended error recovery (one-off):** `/ainb-fleet:daemon` runs a watcher that auto-`continue`s sessions hitting transient API errors (run via `nohup ... &` for real background use). For ongoing supervision prefer ATC, which adds a retry cap.

## Source

`plugins/ainb-fleet/` — a Claude Code skill bundle (8 skills + the `hangar` workflow) teaching agents to drive the `ainb fleet` Rust subcommands; the ATC skill pairs with the `ainb fleet atc` provisioning verbs in the Rust binary. Diagram generated via /fireworks-tech-graph.
