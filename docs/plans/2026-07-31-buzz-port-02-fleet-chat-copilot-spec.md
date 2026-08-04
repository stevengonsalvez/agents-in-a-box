---
title: "Buzz port part 2: spec"
---

# Specification: Fleet Chat + Fleet Copilot (buzz-port part 2)

**Generated from:** /interview (3 rounds, 11 questions) + [research, discussion #570](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570) + [spike report, discussion #570 comment](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570#discussioncomment-17880848)
**Interview date:** 2026-07-31
**Amended:** 2026-08-04 (distinguished-engineer review of parts 1 and 2; amendments marked `(DE review 2026-08-04)`)
**Version:** 1.1

## Executive Summary

Chat surfaces for the ainb fleet, built on the part-1 daemon chat bus, shipped on BOTH the macOS Fleet app and the TUI in parallel against one shared contract. Centerpiece is a fleet copilot: a standing conversation with an ACP-backed agent (claude or codex) holding fleet-control tools, operating autonomously with destructive-only confirmation.

## Objectives

### Primary Goals
- Fleet copilot channel: chat with an agent that orchestrates the whole fleet (status, needs, prompts, broadcast) through daemon-backed tools
- Per-session chat threads replacing one-shot composer with transcripts + receipts
- Broadcast channels: N recipients, replies threaded per recipient
- Same feature set on macOS Fleet app and TUI, one bus contract, no drift

### Success Metrics
- Copilot answers "what's blocked?" from live fleet state and resolves an ASK need end to end without manual session hopping
- Kill daemon mid-conversation, restart, copilot conversation resumes with context (session/load proven in spike)
- Every copilot action visible as activity row + receipt; zero unlogged writes
- Contract tests green on both clients (Swift FleetRPCTests + Rust proto tests) from one shared fixture set
- No write tool fires from reading agent-authored content alone (DE review 2026-08-04, see Security Requirements)

## Scope

### In Scope
- Daemon: copilot service (ACP session mgmt, fleet-tool MCP server, guardrail engine), chat RPC surface consumption
- macOS: sidebar channels section, chat pane, inspector transcript, confirm cards, activity feed
- TUI: chat screen (plugin + core screen registration), same RPC family
- Providers: claude-agent-acp + codex-acp day 1

### Out of Scope
- gemini-cli provider (added later behind capability flags: no ACP steering, auth footguns per spike)
- Web (ainb-web) chat surface (later part, same bus)
- Federation/remote (part of federation track)
- Voice/TTS

### Future Considerations
- Copilot proactive wake on attention events (standing + scheduled check-ins was the runner-up lifecycle option)
- session/fork (advertised by claude-agent-acp) for broadcast branching

## Technical Requirements

### Architecture
Contract-first against part 1's `fleet/message_*` family; reconciliation gate before implementation. Copilot session lives in the daemon (AgentPool from part 1), UIs are thin subscribers. Copilot tools target ALL fleet sessions uniformly (tmux sessions via existing fleet action plumbing, ACP sessions via adapter).

Dependency made explicit (DE review 2026-08-04): "ACP sessions via the existing fleet action plumbing" is only true once part 1's Phase 5 adds the ACP arms to `fleet/action`. Before that, Approve/Deny/StructuredAnswer and Interrupt/Stop/Kill on a non-claude, non-codex provider return Unknown ("authoritative provider request transport is not active", `rpc/mod.rs:1614-1623`). Part 2's `interrupt`/`kill` tools and its permission-answer UI rows are blocked on that work.

### Components
| Component | Purpose | Technology |
|-----------|---------|------------|
| copilot service | standing ACP session + tool loop + guardrails | Rust, hangar daemon, ainb-acp (part 1) |
| fleet-tool MCP server | copilot's tools: status/needs/transcript/send/answer/broadcast/spawn/interrupt/kill | Rust MCP (stdio), passed via ACP session/new mcpServers |
| guardrail engine | auto vs confirm classification, activity log, per-tool overrides | daemon, receipts + new confirm RPC |
| macOS chat UI | sidebar + pane + inspector + confirm cards | SwiftUI, FleetRPC |
| TUI chat screen | chat tab, slash commands, actionable permission rows | ratatui plugin (hangar-tui pattern) + core screen registration |

### Integrations
- Part 1 chat bus: `fleet/message_*`, transcript persistence, AgentPool, resume
- Existing fleet spine: negotiate/subscribe/replay, capability ids, receipts, broadcast params
- ACP adapters: claude-agent-acp >= 0.64.0, codex-acp >= 1.1.7 (version floors from spike)

### Security Requirements
- Pin ACP session permission mode explicitly at session/new; never inherit ambient (spike: claude-agent-acp defaulted to bypassPermissions from environment). Part 1 makes this a tested invariant (I13): the mode is asserted from the `session/new` reply, re-asserted after every `session/load`, and a mismatch fails the spawn (DE review 2026-08-04)
- The permission mode is NOT settable through `fleet/copilot_configure` or any other per-session config. A remotely settable mode is a switch for turning the guardrails off (DE review 2026-08-04)
- Adapter and MCP child processes get an ALLOWLISTED environment, not the daemon's inherited one. The spike's bypassPermissions leak was ambient-state inheritance (DE review 2026-08-04)
- Copilot destructive actions (spawn, interrupt, kill, archive) always behind confirm cards
- Every auto action logged with receipt; per-tool override toggles surfaced in settings; `kill` never overridable to auto
- Steering always sent with idleBehavior=promptRequired (spike: prevents ghost detached turns)
- **Indirect prompt injection is in scope** (DE review 2026-08-04). The copilot reads agent-authored text through `session_transcript`, `session_needs` and `fleet_status`, then acts with auto-class write tools. Fleet content reaches the model inside a fenced, escaped envelope framed as observed data; the guardrail classifier decides on tool identity and arguments only, never on model-supplied justification; an adversarial test asserts that reading a hostile transcript fires no write tool
- Persona is a privileged field: it is a system prompt for an agent holding destructive tools. Capability-gated, length-bounded, and every change logged to the activity feed (DE review 2026-08-04)

## User Experience

### User Flows
1. Copilot triage: open #copilot, ask "what's blocked?", copilot reads needs, answers s1's ASK automatically (logged), proposes killing wedged s3 (confirm card)
2. Per-session chat: select session, chat thread shows prompt/reply history with receipts
3. Broadcast: create channel with N sessions, send "run tests", replies thread per recipient

### Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| Daemon restart mid-copilot-turn | session/load resume; if adapter lacks it or load fails, re-prime from persisted transcript; user sees a resume marker row |
| codex-acp after load | model/mode/reasoning re-applied by daemon (spike: config not persisted in rollout); the permission-mode re-assertion is not overridable and its failure fails the spawn |
| Copilot proposes action on dead session | tool returns typed error; copilot sees it; no receipt row emitted as delivered |
| Confirm card ignored | expires after timeout, action dropped, logged as expired, tool result resolves as denied. Expiry is strictly shorter than part 1's per-turn deadline so the deadline cannot converge the turn out from under a pending card (DE review 2026-08-04) |
| Confirm card answered twice, or answered after expiry | typed error, never a second execution (DE review 2026-08-04) |
| Two UIs open simultaneously | both subscribe same bus; revision-contiguous replay keeps them consistent |
| ADAPTER (not daemon) dies mid-copilot-turn | part 1's runtime convergence resolves the turn and frees the scope with no daemon restart; the copilot channel accepts the next message (DE review 2026-08-04, part 1 I16) |
| A session's transcript contains instructions aimed at the copilot | no write tool fires from the read alone; the content is rendered as fenced observed data (DE review 2026-08-04) |

## Constraints & Dependencies

### Technical Constraints
- Fleet protocol version bump shared with part 1 (one bump, not two)
- TUI tab registration is compile-time (screen id + PLUGIN_SCREENS + keybinding in core)
- Adapter version floors asserted from agentInfo at spawn, and the observed version persisted per session so a later resume can detect drift
- Stream cursors are SQLite-assigned commit-ordered integers, never client-minted ids or timestamps (DE review 2026-08-04, part 1 graft 9)
- Migrations are forward-only; migration 0076 has no in-place downgrade and its back-out is a database file restore (DE review 2026-08-04)

### External Dependencies
- Part 1 plan (docs/plans/2026-07-31-buzz-port-01-chat-bus-acp.md): message model, AgentPool, ainb-acp crate, and specifically its Phase 5 ACP arms on `fleet/action`

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Part 1 contract drifts from draft | Med | Med | Phase 0 reconciliation gate; contract fixtures shared both directions; re-run at every part-1 amendment |
| Autonomous copilot misfires (wrong session answered) | High | Low-Med | destructive-only confirm + activity feed + undo-where-possible + per-tool overrides |
| Indirect prompt injection via agent-authored content | High | Med | fenced envelope on read tools, classifier blind to justification text, adversarial test, cross-session write counter (DE review 2026-08-04) |
| Adapter behavior drift (npm packages) | Med | Med | version floors + capability probing on -32601 (safe per spike) + pinned versions in config |
| Two-surface parallel build drifts | Med | Med | single contract fixture set; shared golden transcripts; CI contract jobs both sides |

## Decisions Made

### Key Trade-offs
- **Surfaces:** macOS + TUI in parallel (revised from macOS-first mid-interview) for contract consistency; accepted higher coordination cost
- **Power:** autonomous with guardrails, destructive-only confirm; rejected read-only advisor (too weak) and everything-auto (spawn storms)
- **Provider:** claude + codex day 1; gemini deferred (spike evidence); rejected claude-only (Stevie wants selectable)
- **Lifecycle:** standing conversation with resume; rejected fresh-per-open (loses orchestration continuity)
- **Phasing:** one plan, 3 phases (copilot, session chats, channels); rejected split plans (contract negotiated three times)
- **Targets:** copilot orchestrates ALL fleet sessions from day 1 (tmux + ACP); rejected ACP-only (fleet is tmux today)
- **Timing:** contract-first parallel with part 1; rejected wait (loses a day) and UI-direct ACP (forks transport)
- **`answer_need` scope (2026-08-04):** auto only for sessions named in the triggering operator message, confirm card otherwise; rejected fully-auto (injection blast radius) and always-confirm (breaks zero-click triage)
- **Pool shape (2026-08-04, part-1 decision the copilot rides):** multiplexed, one adapter process per provider hosting many sessions; rejected process-per-scope (DE's recommendation) accepting wider crash blast radius because spike-proven session/load recovery plus I16 fan-out convergence make it recoverable

### Deferred Decisions
- Proactive copilot wake on attention events: v1.1, needs event-triggered prompting design
- gemini provider onboarding: after creds flow + capability-degradation UX

## Implementation Notes

### Priority Order
1. Phase 0: contract draft + part-1 reconciliation
2. Phase A: copilot channel (daemon service + both UIs)
3. Phase B: per-session chat threads
4. Phase C: broadcast channels

## Open Questions

- [ ] Confirm-card timeout duration and expiry semantics (proposal: 10 min, expire-drop, logged; must stay strictly shorter than part 1's per-turn deadline, default 30 min)
- [ ] Copilot persona: ship a default .persona.md now or hardcode system prompt until part 5 (persona port)?
- [ ] Does session/load with well-formed-but-nonexistent UUID behave like malformed on claude-agent-acp (spike gap, one extra probe)?
- [ ] Does `answer_need` stay in the auto class? It is the only auto tool that resolves an approval prompt inside another agent's session, so it is the tool an injected instruction most wants, yet the headline success metric depends on it being auto. Recommendation: keep it auto but scope it to sessions the operator's current prompt explicitly named, confirm card otherwise (DE review 2026-08-04)

---

*This specification was generated through systematic interview of the plan author, and amended 2026-08-04 by distinguished-engineer review.*
