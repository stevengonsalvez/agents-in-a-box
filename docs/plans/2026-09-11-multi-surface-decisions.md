# Decisions: federation, mobile, agent status on the shared core

**Date:** 2026-09-11
**Status:** options paper, nothing decided. Input to `/interview`; output amends `2026-09-04-desktop-shared-core-spec.md` and seeds plan slices 2 and 3.
**Builds on:** handover `~/.agents-in-a-box/handover/2026-09-11-desktop-shared-core-handover.md`, spec `docs/plans/2026-09-04-desktop-shared-core-spec.md` (decisions D1-D9 locked, P0+S in flight).
**Research:** six code-level reports plus two independent syntheses under `research/2026-09-11_multi-surface_*.md` (gitignored dir, `git add -f`). A prior-art system is called "the reference" throughout; every claim below is how we want to build it.
**Format:** diagram-first, tables second, no prose paragraphs. `[fact]` cites a report section, `[inference]` is judgment.

## Target shape

```
┌─ desktop (Tauri) ─┐  ┌─ TUI ─┐  ┌─ mobile (Expo) ─┐  ┌─ web (axum) ─┐
│ Vec<HostApp>      │  │ ainb  │  │ thin RPC client │  │ PWA + xterm  │
└────────┬──────────┘  └───┬───┘  └────────┬────────┘  └──────┬───────┘
         │ mirror+sub      │ mirror        │ sections+stream  │
         ▼                 ▼               ▼                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ TRANSPORT   unix sock (local) │ WebSocket + E2EE (off-box)           │
│             carriers: LAN · tailnet · ssh -L                         │
│ AUTH        SO_PEERCRED (local) │ per-device token + scope allowlist │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼  one binary, one role per box
┌──────────────────────────────────────────────────────────────────────┐
│ ainb-hangar-daemon = the control plane, per box                      │
│  HostId · status store (single writer, provenance) · op-id ledger    │
│  event_outbox + after_revision replay · PTY stream + snapshot        │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼
                        tmux server (unchanged, PTY owner)
```

## Features we want, and the gap

| Feature | Needs from core | Have today | Gap |
|---|---|---|---|
| One UI, N boxes, live streaming | host identity on the wire, off-box transport, per-host connection FSM, coverage census on listings | daemon control plane, cursor-replay `fleet/subscribe {after_revision}`, `SnapshotReset` [fact F redesign] | no host field in wire or schema; unix socket is the only door; `session_key` collides across boxes [fact F§4] |
| Mobile companion | multiplexed WS, E2EE, per-device tokens, snapshot-then-tail terminal, sequenced status feed with epoch, idempotent mutations | VAPID web-push to a PWA, `ainb-web` PtyBridge [fact F§5-6] | no mobile client, no APNs/FCM, single shared token, no op ids [fact F redesign] |
| Agent dashboard with reliable status | daemon-owned single-writer status store, hook push as tier 0, provenance per row, three clocks, silence never `done` | 30 lifecycle hooks into `events.jsonl`, pure `classify()`, transcript tail [fact F§7] | pane regex is the default tier, three session-existence records, no store with write-time precedence [fact F§11] |
| Remote execution that survives laptop closed | daemon on the box owns sessions; client is a mirror | daemon already owns sessions locally; ACP pool and headless runner are PTY-free [fact F§8] | spec frames remote as "ssh-forwarded socket owned by the desktop", a transport not a host model [fact B§8.6] |
| Answer from any surface | exactly-once answer with receipts | `answer.rs` first-answer-wins + misroute guards [fact F redesign] | picker answer reads the screen 60 times via `capture-pane`, local tmux only [fact F§8] |

## Blockers in the current architecture

| Blocker | Why it blocks | Fix lives in |
|---|---|---|
| unix socket is the only RPC door; `bind()` returns a concrete `UnixListener` with no seam | cannot cross a machine | X4 |
| `SO_PEERCRED` same-uid is the trust boundary | no off-box equivalent, must be replaced not extended | X4 |
| no `HostId` anywhere; `session_key` derived from tmux target and cwd | two boxes collide | X2 |
| capabilities are advisory, not per-connection authz (self-documented "v3 surface") | N remote clients need scopes | X4 |
| `capture-pane` status + `send-keys` digit answer | remote answering and mobile first frame both need bytes, not a rendered pane | X1, X5 |
| three hand-written daemon clients (Rust x2, Swift x1, 1,572 mirrored Swift lines) | every wire change is a three-way edit | X8 |
| `ainb-core` monolith, 252k LOC, no library boundary | nothing off-box can reuse session logic | P1-P6 (already planned) |
| `hangar/*` and `attention/*` carry no protocol version; `auth/hello` has no version field | mixed versions become the normal state once a phone ships | X8 |

## Decisions

Scores in the explainer are judgment [inference]. Recommended option first in every table.

### X1 tmux: keep, replace, or hybrid

```
tmux ──attach (portable-pty)──▶ daemon VT emulator ──▶ snapshot ──▶ phone first frame
  └── N viewers free, survives our restart, operator can attach by hand
```

| Option | Downside |
|---|---|
| **Hybrid (recommended):** tmux stays PTY owner and multi-attach substrate; daemon adds a headless emulator fed by the attached stream for snapshot and status | we parse tmux's re-render, not the child's bytes; alt-screen, mouse tracking, OSC 8 may not survive the hop [inference A§7c] |
| Replace with daemon-owned PTY | loses `list-panes` discovery of hand-started agents and survival across our own restart; the reference's daemon dies on `systemctl restart` [fact A§7a, F§8] |
| tmux only, no emulator | `capture-pane` cannot express cursor, alt-screen, mouse mode, OSC 8; no authoritative first frame for a phone [fact A§7d.1] |

- Cost: emulator, snapshot, per-viewer flow control, 2.5-4k LOC, 3-4 weeks [inference]
- Blocked by: 126 non-test tmux call sites, no trait at the boundary; mitigated by the `Transport` enum seam in `send/route.rs` [fact F§8, F§10]
- Spec delta: none to P0-P6; new phase R2
- Not building: input and resize arbitration between two typists until a phone can type [fact A§7d.3-4]

### X2 Host model: peer daemon on the box

```
laptop ──WS+E2EE──▶ daemon on box (owns sessions) ◀── laptop closed, agents keep running
   carriers: tailnet · LAN · ssh -L (one of several, not the model)
```

| Option | Downside |
|---|---|
| **Peer daemon (recommended):** same binary on the box owns its control plane; `HostId` threaded through wire and schema; `ssh -L` demoted to a carrier | every listing becomes a fan-out with a coverage census and `omitted_hosts`, or a host silently vanishes under a row cap [fact B§1.3] |
| Client-owned control plane over `ssh -L` (spec's current wording) | nothing on the box works while the laptop is shut; socket path per build orphans live work on every update [fact B§8.6, B§1.5] |
| Register a box both ways | splits its sessions across two identities [fact B§1.4] |

- Not a reversal of the locked spec: the daemon already lives on the box; this sharpens ownership, adds `HostId`, and versions the socket path by protocol version not build [inference, both syntheses]
- Cost: `HostId` through 159 proto methods, `fleet_session`, three clients, 1.5k LOC plus a migration, 2-3 weeks [inference]
- Spec delta: rewrites the D4 transport row; `HostId` lands before a second host reaches any UI

### X3 Relay: no

| Option | Downside |
|---|---|
| **No relay (recommended):** tailnet or ssh covers laptop-to-box; reserve an optional `relay` slot in the pairing offer | a phone on cellular with no VPN cannot reach the box |
| Single-cell self-hosted relay | 2k Rust LOC plus SQLite plus an OIDC issuer, an operable service, 2-3 weeks [inference B§8.5] |
| Full topology | 18.5k LOC of which the data plane is ~110 lines; the rest is admission, assignment, migration [fact B§5.3] |

- Reopen when: a client genuinely cannot dial the box [fact B§8.2]

### X4 Off-box transport and auth

```
pair (QR / deep link / paste) ──▶ {endpoints[], device_token, host_pubkey, scope, relay?}
connect ──▶ X25519 handshake, transcript binds transport + host id ──▶ AEAD frames
dispatch ──▶ scope → method allowlist check BEFORE handler
```

| Option | Downside |
|---|---|
| **WebSocket + app-level E2EE + per-device revocable tokens with `scope` + allowlist at the transport boundary (recommended)** | tokens carry no expiry in the reference; revocation is the only control [fact B§6.2] |
| Shared daemon token over TLS | cannot revoke one device, cannot give a phone a smaller surface than the desktop [fact C§A.2] |
| mTLS | cert distribution to a laptop on a LAN; key pinning in the pairing offer removes that problem [inference C§A.3] |

- Cost: transport, pairing, E2EE, device registry, ~2.9k LOC, 3-4 weeks [inference B§8.4]
- Blocked by: `SO_PEERCRED` gate, concrete `UnixListener`, advisory capabilities [fact F§3, F§10]
- Spec delta: replaces "same token, peer_cred sees ssh user" in the Hosts table
- Reuse as-is: JSON-RPC envelope, `ainb-hangar-proto`, `event_outbox` replay [fact F redesign]

### X5 Status signal hierarchy and store

```
T0 hook push ─┐  only T0/T1 may OPEN a turn or assert needs-input
T1 ACP feed  ─┤
T2 OSC frame ─┼──▶ daemon store, single writer, provenance stamped at write ──▶ every surface mirrors
T3 process   ─┤
T4 transcript─┤  usage and prompt recovery only
T5 pane text ─┘  discovery of hand-started agents + readiness gate + tagged last-resort row
silence ──▶ unverifiable (we hold the pane) | idle (we do not). never done.
```

| Option | Downside |
|---|---|
| **Six tiers, daemon-owned store, three clocks, `restored_unconfirmed` on hydrate (recommended)** | per-provider normalizers are ongoing work; one hookless CLI cost the reference ~300 lines and 75 hardcoded status words [fact D§3.9] |
| Keep `capture-pane` regex as primary | shared literal lists strand providers that name events differently; version-pinned to CLI rendering [fact D§7.5] |
| Store per surface | three copies keyed differently, one session reads differently on desktop, phone, CLI [fact D§7.3] |

- Cost: 2-3k LOC, 3-4 weeks; large reuse of the hook pipeline and pure `classify()` [fact F redesign]
- Blocked by: nothing; tiers 0, 1, 3, 4 are tmux-independent [fact D§7.2]
- Spec delta: new `agent_status` mirror section with its own drain budget
- Order of provider work: Claude-compatible family (one normalizer, six CLIs), then the OSC in-band contract, then session-state family [inference D§7.4]

### X6 Renderer contract under the fan-out measurements

| Option | Downside |
|---|---|
| **Keep plan B with three day-one invariants (recommended):** frames name changed sections; one store transaction per channel drain, effects after commit; clients may subscribe to a subset of sections | the measured cost of naive mirroring is 9,279 listeners, ~160 publications/s, ~11.6 points of renderer CPU with zero long tasks to warn you [fact E§3.4] |
| Plan B as written | a phone showing one agent receives all 19 sections; outbound volume to a phone is a budgeted resource [fact E§Q1] |
| RPC catalog with narrow subscriptions | 610 methods needed a generator, identity-matching bundle, byte-diff gate, and still drifted [fact E§1.6] |

- Cost: ~400 LOC plus a listener census and fan-out bench with a CI ceiling, 3-5 days [inference]
- Spec delta: additive to P0; must be in the contract before the second consumer surface

### X7 Mobile stack

| Option | Downside |
|---|---|
| **Expo / React Native, xterm in a webview, thin RPC client (recommended)** | no type sharing with a Rust core, bindings must be generated; Hermes lacks a CSPRNG and `Buffer` [fact C§2.4]; iOS release needs a hosted Mac runner pinned to current Xcode [fact C§C] |
| Tauri mobile | a phone wants a thin client talking to the core on the box, exactly what Tauri mobile does not help with; no mature keychain, camera, notification plugins [fact C§C] |
| Swift + Kotlin native | two UIs; the macOS fleet app shares models not screens; inverts the moment Android ships [fact C§C] |

- v1 set: pair, host list, session list with live status, transcript, send prompt, answer permission and question, cancel turn, read-only terminal with snapshot replay, notifications, connection log [inference C§B]
- Cost: 16-19k LOC, 2-3 months [inference C§8]
- Spec delta: new phase M1; v1 banners ride the live socket plus existing VAPID web-push; native push deferred

### X8 Wire versioning and capabilities

| Option | Downside |
|---|---|
| **One integer protocol version with a written bump rule, named capability strings negotiated both ways, handshake-negotiated opcodes with permanent numbers, two-direction skew harness (recommended)** | a real catalogue of 65-80 strings to maintain |
| Integer only | an app-store phone cannot be force-upgraded in lockstep; unknown opcode is dropped silently and the feature appears to hang [fact B§1.7, C§6.2] |
| Exact-version equality | socket path per version, permanent legacy adapters [fact A§4] |

- Cost: ~600 LOC, 1-1.5 weeks; cheapest today, most expensive after a phone ships [fact A§7c]
- Blocked by: `FLEET_PROTOCOL_VERSION = 2` covers the fleet family only [fact F§3]
- Spec delta: new phase W0, gates P6 and D4

### X9 Idempotent mutation envelope

```
client mints op_id (ts-prefixed, freshness-bounded) + expected fence
daemon commits FIRST, then replies ──▶ retry gets committed result: created | adopted | replayed
receipt marks the pre-PTY-write boundary; after the write, effects are ambiguous
```

| Option | Downside |
|---|---|
| **Op id + fence + receipt on every mutation (recommended)** | ~900 LOC plus a migration, 1.5-2 weeks; `answer.rs` is 1,940 lines to thread through [inference] |
| Today's first-answer-wins only | a lost reply leaves the client unable to distinguish delivered from not; retry double-fires "approve permission" on an agent about to run a command [fact C§A.5] |
| Client-side dedupe | retained ids must be expiry-bounded or a dropped id becomes a second message [fact C§3.3] |

- Spec delta: generalises the locked "reject-second on answers" into the envelope; lives in W0

## Sequencing

```
now ──▶ P0+S (in flight) ──▶ P1 ──▶ P2-P5 ──▶ P6 ──▶ D1-D3 ──▶ D4 (rescoped ⊂ R1)
         + X6 invariants                │
         ▼                              │
      W0 (X8, X9) ─── any time, before P6 and any off-box client ──▶ R1 (X2, X4) ──▶ R2 (X1 emulator)
         │                                                              │               │
      T0 (X5) ─── parallel, independent of everything ─────────────────┴──▶ M1 (X7) ◀───┘
```

| Phase | Contents | Entry gate | Exit gate |
|---|---|---|---|
| P0+S | as planned, plus X6 invariants | now | tripwires green; listener census and fan-out bench in CI with a ceiling |
| W0 | X8 version + capabilities + skew harness; X9 envelope | any time, before P6 | skew harness green both directions; every mutation carries an op id |
| T0 | X5 daemon status store, Claude-family normalizer, OSC contract | spike 1 passes | one status truth across TUI, web, CLI; no tier-5 row outranks a hook |
| R1 | X2 `HostId`, X4 WS + E2EE + device tokens + scope allowlist; absorbs D4 | W0 and P6 | two boxes in one UI with coverage census and `omitted_hosts` |
| R2 | X1 emulator, snapshot, per-viewer flow control | R1, spike 2 | phone-shaped first frame with colour, cursor, alt-screen |
| M1 | X7 mobile v1 | R1, R2, W0 | answer a needs-input banner in two taps |

Paint-into-corner items, each before the phase that consumes it:

| Item | Before |
|---|---|
| X8 versioning + capabilities | any off-box client |
| X9 op ids | mobile |
| X4 per-device tokens | first pairing; retrofit means re-pairing every device |
| X6 section naming + subscription | second consumer surface |
| X2 `HostId` | second host in any UI |
| D4 rescope to peer WS with `ssh -L` as carrier | D4 starts |

## Do not build

| Not building | Reopen when |
|---|---|
| Cloud relay: director, cells, splice, assignment, fencing | a client cannot dial the box at all |
| Daemon-owned PTY replacing tmux | spike 2 shows the tmux-hop snapshot is unusable, or native Windows enters scope |
| Checkpoint-and-log durable scrollback pipeline | tmux stops being the scrollback of record |
| Native APNs / FCM push gateway | mobile ships and users need banners with the app suspended |
| Per-connection capability authz ("v3 surface") beyond scope allowlist | a non-operator or third-party client pairs |
| Input and resize driver-floor arbitration | two viewers can type, or a phone at 40x20 attaches beside a desktop |
| Subagent roster rows in the status store | dashboard shows child rows |
| Cross-box orchestration mailboxes | one box must run dispatches while the home client is offline |
| Rewriting the `ainb-web` renderer onto the shared core | after M1; P6 migrates only its daemon client |

## Contradictions resolved

| Tension | Resolution |
|---|---|
| tmux is our biggest asset vs tmux is the hard mobile blocker | both true at different layers; the blocker is the missing snapshot primitive, fixed by the emulator without replacing tmux; residual fidelity risk is spike 2 |
| N viewers free vs no input/resize arbitration | viewing is free; typing from two geometries is not, under any model; defer the floor to phone input |
| whole mirror is riskier vs plan B locked | not a reversal; B stays for the local renderer, section subscription is the remote/mobile escape hatch |
| "client drives the box" is wrong vs spec | the spec forwards to the remote daemon's socket, so the control plane is on the box already; what changes is `HostId`, one WS transport, socket path versioned by protocol not build |
| pane text must not be primary vs `list-panes` discovery is real | split the roles: tier 5 keeps discovery and the readiness gate, never authority |
| only 2 of 6 providers have a tmux-free answer path | moot under the peer model: the daemon on the box answers into its local tmux; the client sends an RPC |

## Spikes before committing

| # | What | Time | Flips |
|---|---|---|---|
| 1 | Emit an OSC status frame from a wrapper inside a tmux pane with `allow-passthrough on`; confirm it reaches a `portable-pty` attach stream intact | 1 day | X5 tier 2. On failure, hooks carry everything alone over tmux |
| 2 | Drive a real session (alt-screen, mouse, wide chars, OSC 8) through `tmux attach` under `portable-pty` into a Rust VT emulator; serialise a snapshot; diff against the same agent on a direct PTY | 2-3 days | X1. Poor fidelity reopens "daemon-owned PTY" as a third sibling of headless and ACP |
| 3 | Peer WS + E2EE prototype over tailnet and over `ssh -L`; confirm the daemon keeps working after the desktop closes; measure reconnect and resync | 2-3 days | X2/X4 go for R1 before D4 is rescoped |
| 4 | Extend the planned specta spike: 2,000-update burst at 100 sessions through the Solid store with a listener census, per-section sends vs one transaction per drain | 2 days | X6. If per-drain batching cannot hold a CPU ceiling, section subscription becomes mandatory locally too |

Spikes 1 and 4 can run during P0+S. None blocks the keymap or Phase S work.

## Forks for /interview

| # | Fork | Recommended |
|---|---|---|
| F1 | X1 hybrid tmux vs replace | hybrid, spike 2 gates |
| F2 | X2 accept peer model, rescope D4 now | yes |
| F3 | X3 no relay, reserve slot | yes |
| F4 | X4 WS + E2EE + per-device tokens | yes; choose Noise IK vs NaCl box |
| F5 | X5 six tiers; which providers first | Claude-compatible family, then OSC |
| F6 | X6 section subscription in P0 contract now vs before D4 | now, it is ~400 LOC |
| F7 | X7 Expo; v1 read-only terminal acceptable | yes |
| F8 | X8 W0 as its own phase vs folded into S | own phase, gates P6 |
| F9 | X9 envelope on every mutation vs answers only | every mutation |
| F10 | Ordering: T0 before or alongside P1 | alongside, independent |
| F11 | Native Windows in scope for v2 | no; it is the one trigger that reopens X1 |
