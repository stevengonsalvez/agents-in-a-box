# Specification: Fleet Chat + Fleet Copilot (buzz-port part 2)

**Generated from:** /interview (3 rounds, 11 questions) + research/2026-07-31_14-56-19_buzz-acp-port.md + research/2026-07-31_acp-resume-steering-spike.md
**Interview date:** 2026-07-31
**Version:** 1.0

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
Contract-first against part 1's draft `fleet/message_*` family; reconciliation gate before implementation. Copilot session lives in the daemon (AgentPool from part 1), UIs are thin subscribers. Copilot tools target ALL fleet sessions uniformly (tmux sessions via existing fleet action plumbing, ACP sessions via adapter).

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
- Pin ACP session permission mode explicitly at session/new; never inherit ambient (spike: claude-agent-acp defaulted to bypassPermissions from environment)
- Copilot destructive actions (spawn, interrupt, kill, archive) always behind confirm cards
- Every auto action logged with receipt; per-tool override toggles surfaced in settings
- Steering always sent with idleBehavior=promptRequired (spike: prevents ghost detached turns)

## User Experience

### User Flows
1. Copilot triage: open #copilot, ask "what's blocked?", copilot reads needs, answers s1's ASK automatically (logged), proposes killing wedged s3 (confirm card)
2. Per-session chat: select session, chat thread shows prompt/reply history with receipts
3. Broadcast: create channel with N sessions, send "run tests", replies thread per recipient

### Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| Daemon restart mid-copilot-turn | session/load resume; if adapter lacks it or load fails, re-prime from persisted transcript; user sees a resume marker row |
| codex-acp after load | model/mode/reasoning re-applied by daemon (spike: config not persisted in rollout) |
| Copilot proposes action on dead session | tool returns typed error; copilot sees it; no receipt row emitted as delivered |
| Confirm card ignored | expires after timeout, action dropped, logged as expired |
| Two UIs open simultaneously | both subscribe same bus; revision-contiguous replay keeps them consistent |

## Constraints & Dependencies

### Technical Constraints
- Fleet protocol version bump shared with part 1 (one bump, not two)
- TUI tab registration is compile-time (screen id + PLUGIN_SCREENS + keybinding in core)
- Adapter version floors asserted from agentInfo at spawn

### External Dependencies
- Part 1 plan (plans/2026-07-31-buzz-port-01-chat-bus-acp.md, generating at interview time): message model, AgentPool, ainb-acp crate

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Part 1 contract drifts from draft | Med | Med | Phase 0 reconciliation gate; contract fixtures shared both directions |
| Autonomous copilot misfires (wrong session answered) | High | Low-Med | destructive-only confirm + activity feed + undo-where-possible + per-tool overrides |
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

- [ ] Confirm-card timeout duration and expiry semantics (proposal: 10 min, expire-drop, logged)
- [ ] Copilot persona: ship a default .persona.md now or hardcode system prompt until part 5 (persona port)?
- [ ] Does session/load with well-formed-but-nonexistent UUID behave like malformed on claude-agent-acp (spike gap, one extra probe)?

---

*This specification was generated through systematic interview of the plan author.*
