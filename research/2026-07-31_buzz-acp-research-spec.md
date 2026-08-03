# Specification: buzz → ainb Port Research (ACP, Chat, Federation)

**Generated from:** /research interview (no plan file, scoped live)
**Interview date:** 2026-07-31
**Version:** 1.0

## Executive Summary

Comprehensive research of https://github.com/block/buzz (Block's ACP-based multi-agent chat app) to determine what ainb (agents-in-a-box) should port. Research only, no implementation this round. Deliverables: cited research doc + multi-tab HTML explainer published to here.now.

## Objectives

### Primary Goals
- Full feature inventory of buzz, each feature tagged port / adapt / skip with S/M/L effort
- ACP verdict: adopt / hybrid / skip vs hangar plugin protocol AND tmux session adapters, with migration sketch
- Chat surface comparison: ainb TUI chat tab vs Fleet chat + broadcast vs daemon-level chat bus, ranked build order, shared-infra analysis
- Federation design track: options survey (SSH tunnel, tailscale, TCP/websocket daemon, sync) + architecture sketch + pick

### Success Metrics
- Every buzz feature catalogued with a port call
- ACP verdict backed by spec + ecosystem evidence (claude-code-acp, codex, goose, Zed)
- Ranked chat build order with one recommended first surface
- One recommended federation approach with sketch

## Scope

### In Scope
- Clone + deep read of block/buzz (full inventory: tabs/sessions, agent mgmt, MCP, modes, slash commands, UI, config, persistence, keybindings)
- ACP spec + ecosystem survey (adapters, clients, agent support matrix)
- ainb current-state mapping: hangar plugin protocol, session adapters, fleet-core, Fleet macOS app chat-relevant surfaces
- Federation options research + design sketch

### Out of Scope
- Any implementation, prototyping, or code changes
- Non-chat buzz internals beyond inventory level (e.g. reimplementation detail)

### Future Considerations
- Implementation plan (/plan) for whichever chat surface ranks first
- Federation prototype

## Technical Requirements

### Components under study
| Component | Side | Purpose |
|-----------|------|---------|
| buzz | external | ACP chat client, feature source |
| ACP spec + ecosystem | external | protocol candidate |
| ainb-hangar-daemon/proto | ainb | current daemon + plugin protocol |
| ainb session adapters (tmux) | ainb | current agent transport |
| ainb-fleet-core + Fleet macOS | ainb | fleet surfaces for chat/broadcast |

## Decisions Made

- ACP framing: full adopt/hybrid/skip verdict with migration sketch
- Chat surfaces: research ALL three, compare + rank; no build this round
- Federation: design track (options + sketch + pick)
- Buzz depth: full feature inventory with per-feature port calls
- ACP breadth: full ecosystem survey beyond buzz
- Deliverable: research doc in research/ + /explain-to-me multi-tab HTML explainer, published to here.now per config

## Open Questions

- [ ] Whether ACP supports/anticipates remote transport (feeds federation verdict)
- [ ] Whether codex has first-party ACP support or needs an adapter

---

*This specification was generated through systematic interview of the plan author.*
