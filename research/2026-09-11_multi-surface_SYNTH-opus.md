# SYNTH: architecture decisions for the multi-surface rebuild

Synthesis of reports A-F. Citations are `A§7(c)` style. `[fact]` = a report cites code; `[inference]` = my reasoning.
Naming: the analysed system is "the reference"; everything below is how **we** want to build it.

## 0. Target shape

```
┌─ desktop (Tauri) ─┐  ┌─ TUI ─┐   ┌─ mobile (Expo) ─┐   ┌─ web ─┐
│ Vec<HostApp>      │  │ ainb  │   │ thin RPC client │   │ axum  │
└────────┬──────────┘  └───┬───┘   └────────┬────────┘   └───┬───┘
         │ mirror+sub      │ mirror         │ sections+stream │
         ▼                 ▼                ▼                 ▼
┌──────────────────────────────────────────────────────────────────┐
│ TRANSPORT: unix sock (local) │ WS+E2EE (off-box) │ ssh -L carrier │
│ auth: SO_PEERCRED (local only) │ per-device token + scope         │
└──────────────────────────┬───────────────────────────────────────┘
                           ▼  one binary, one role per box
┌──────────────────────────────────────────────────────────────────┐
│ ainb-hangar-daemon = THE control plane, per box                  │
│  HostId ▸ status store (single writer) ▸ op-id receipt ledger    │
│  event_outbox + after_revision replay ▸ PTY stream + snapshot    │
└──────────────────────────┬───────────────────────────────────────┘
                           ▼
                    tmux server (unchanged)
```

## 1. Decisions

### X1 tmux: keep, replace, or hybrid?
**Answer: hybrid. tmux stays the PTY owner and multi-attach substrate. Add a daemon-side headless emulator fed by an attached stream, for snapshot and status, behind the existing `Transport` seam.**
Alternatives: (a) a daemon-owned PTY. Loses `list-panes` discovery of hand-started agents, a capability not a refactor, and loses survival across our own restart [fact F§8, A§7(a)]. (b) pure tmux, no emulator. `capture-pane` cannot express cursor, alt-screen, mouse mode or OSC 8 ranges, so a phone has no authoritative first frame: the hard mobile blocker [fact A§7(d).1]. Downside of the pick: tmux is itself an emulator, so we parse its re-render, not the child's bytes, and alt-screen plus mouse tracking may not survive the hop [inference A§7(c)].
Cost: emulator, snapshot, per-viewer flow control, roughly 2.5-4k LOC over 3-4 weeks [inference, scaled from A§2 and B§8.4 item 5].
Blocked by: 126 non-test `Command::new("tmux")` sites, 642 in `ainb-core`, no trait at the tmux boundary [fact F§8]. Mitigated because the `Transport` enum in `send/route.rs` is a designed extension point [fact F§10].
Spec delta: nothing in P0-P6. New phase R2.

### X2 Host model: peer daemon on the box, or client-driven ssh?
**Answer: peer daemon on the box, which is already our shape. Demote `ssh -L` from "the architecture" to one carrier among several. Add `HostId` to wire types and schema.**
Alternatives: (a) a client-owned control plane. Nothing on the box works while the laptop is shut, defeating a remote agent box [fact B§8.6, B§3.3]. (b) register a box as both dumb exec host and peer. Splits its sessions across two identities [fact B§1.4]. Downside of the pick: every listing becomes a fan-out needing a coverage census, and `omitted_hosts` must be modelled or a host vanishes under a row cap [fact B§1.3].
Cost: HostId enum threaded through 159 proto methods, `fleet_session` and 3 clients, roughly 1.5k LOC plus a migration, 2-3 weeks [inference].
Blocked by: no host or machine field anywhere in the wire types or the 96 migrations, and `session_key` is derived from tmux target and cwd so two boxes collide [fact F§4, F "Redesign inputs"].
Spec delta: rewrites the D4 transport row. `HostId` must land before D4 and before a second host reaches the UI.

### X3 Relay: build one?
**Answer: no. Tailnet or ssh covers laptop-to-box.**
Alternatives: (a) a single-cell relay now, roughly 2k LOC plus SQLite plus an OIDC issuer, 2-3 weeks; buys nothing a tailnet does not and adds an operable service [inference B§8.5]. (b) the full topology, 18.5k LOC of which the data plane is about 110 lines, the rest admission, assignment durability and migration [fact B§5.2-5.3]. Recommended option's downside: a phone on cellular with no VPN cannot reach the box.
Cost: zero now.
Spec delta: none. Reserve an optional `relay` slot in the pairing offer so devices paired before a relay never have to re-pair [fact C§A.4].

### X4 Off-box transport and auth
**Answer: WebSocket carrying the existing JSON-RPC envelope, application-level E2EE (X25519 plus AEAD, host key pinned in the pairing offer, handshake transcript binding transport and host id), per-device revocable tokens carrying a `scope`, and a scope-to-method allowlist checked at the transport boundary before dispatch.**
Alternatives: (a) the single shared daemon token over TLS. Cannot revoke one device, cannot give a phone a smaller surface [fact C§A.2]. (b) mTLS. Solves identity but not the "TLS cert for a laptop on a LAN" problem that key pinning removes [inference C§A.3]. Downside of the pick: such tokens carry no expiry in the reference, so revocation is the only control [fact B§6.2].
Cost: transport, pairing, E2EE and device registry, roughly 2.9k LOC over 3-4 weeks [inference B§8.4 items 2-4].
Blocked by: `SO_PEERCRED` same-uid is the load-bearing gate and has no off-box equivalent; `bind()` returns a concrete `UnixListener` with no seam; capabilities are documented as advisory, not authz [fact F§3, F§10, F§12].
Spec delta: replaces "same token, peer_cred sees ssh user" in the Hosts table.

### X5 Status signal hierarchy and where the store lives
**Answer: hooks > structured (ACP) > in-band OSC > process evidence > transcript > pane text. Only the top two tiers may open a turn or assert needs-input. Silence becomes `unverifiable` when we hold the pane and `idle` when we do not, never `done`. The store lives in the daemon, single writer, provenance stamped at write time, three clocks, `restored_unconfirmed` on hydrate.**
Alternatives: (a) keep `classify()` over `capture-pane` as primary. One hookless CLI cost the reference about 300 lines with 75 hardcoded status words, pinned to one CLI version [fact D§3.9, D§6]. (b) a store per client. Three copies keyed differently means one session reads differently on desktop, phone and CLI [fact D§7.3]. Downside of the pick: per-provider normalizers are ongoing work, and subagent rosters are the largest complexity source there [fact D§3.12].
Cost: roughly 2-3k LOC over 3-4 weeks, with large reuse: our hook pipeline is already the authoritative tier and `classify()` is already pure [fact F "Redesign inputs"].
Blocked by: nothing. Tiers 0, 1, 3 and 4 are tmux-independent [fact D§7.2]. Highest correctness win per unit of work and independent of X1 [inference A§7(c)].
Spec delta: a new `agent_status` mirror section with its own drain budget. Tier 2 keeps *discovery* of hand-started agents and a tagged heuristic row, never authority.

### X6 Renderer contract under the fan-out measurements
**Answer: keep plan B, the whole `AppState` mirror in 19 `Versioned<Section>`, with three day-one invariants: frames name the changed sections, the renderer applies one store transaction per channel drain with effects strictly after commit, and a client may subscribe to a subset of sections.**
Alternatives: (a) mirror with no subscription. A phone showing one agent receives all 19 sections, and outbound volume to a phone is a budgeted resource [fact E§Q1]. (b) an RPC catalog with narrow subscriptions. 610 methods needed a generator, an identity-matching bundle and a byte-diff gate, and still leaked 3 unshareable schemas [fact E§1.6]. Downside of the pick: the measured cost of naive mirroring is 9,279 listeners, about 160 publications per second and roughly 11.6 points of renderer CPU, with **zero long tasks** to warn you [fact E§3.4].
Cost: roughly 400 LOC, 3-5 days.
Spec delta: additive to P0. Section naming and subscription must be in the contract before the second consumer surface, otherwise every send site gets audited later.

### X7 Mobile stack
**Answer: Expo and React Native, xterm inside a webview, thin RPC client. v1 is pair, host list, session list with live status, transcript, send prompt, answer permission and question prompts, cancel a turn, read-only terminal with snapshot replay, notifications, connection log.**
Alternatives: (a) Tauri mobile. A phone wants a thin client talking to the core on the box, which Tauri mobile does not help with, and its plugin ecosystem has no keychain, camera or notification equivalent [fact C§C]. (b) Swift plus Kotlin native. Two UIs, and the existing Swift macOS app shares models, not screens [fact C§C]. Recommended option's downside: no type sharing with a Rust core, so bindings must be generated; Hermes lacks a CSPRNG and `Buffer` [fact C§2.4].
Cost: roughly 16-19k LOC, 2-3 months [inference C§8].
Spec delta: new phase M1. v1 banners ride the live socket plus our existing VAPID web-push [fact F§6]; native push is deferred.

### X8 Wire versioning and capabilities
**Answer: one integer protocol version with a written bump rule, plus named capability strings for everything additive, negotiated in both directions. New binary opcodes must be handshake-negotiated and opcode numbers are permanent. Add a two-direction cross-version skew harness.**
Alternatives: (a) the integer alone. A phone shipped to an app store cannot be force-upgraded in lockstep, and capability strings are what allow weekly additive change [fact C§6.2]. (b) exact-version equality. Forces a socket path per version and permanent legacy adapters [fact A§4]. Downside of the pick: 65 to 80 capability strings to maintain.
Cost: roughly 600 LOC, 1-1.5 weeks. Cheapest item on this list today and the most expensive after mobile ships [fact A§7(c) ADD 4].
Blocked by: `FLEET_PROTOCOL_VERSION = 2` covers the fleet family only; `hangar/*` and `attention/*` have no version at all and `auth/hello` carries no version field [fact F§3].
Spec delta: new phase W0, gates P6 and D4. An unknown opcode is dropped silently, so the feature behind it appears to hang [fact B§1.7 rule 2].

### X9 Idempotent mutation envelope
**Answer: every mutation carries a client-minted operation id, timestamp-prefixed and freshness-bounded, plus an expected fence. The daemon commits first and then replies; a retry receives the committed result with a disposition of `created | adopted | replayed`. Outcomes are a closed three-way `accepted | rejected | unknown`, with an explicit `ownership_unknown`. A durable receipt marks the pre-write boundary on the send path.**
Alternatives: (a) today's first-answer-wins at SQLite. Correct for a double answer, but a lost reply leaves the client unable to tell delivered from not, so a retry double-fires [inference F§7, B§3.1]. (b) client-side dedupe only. Retained ids must be bounded by expiry, not count, or a dropped id turns a retry into a second message on the host [fact C§3.3].
Cost: roughly 900 LOC plus one migration, 1.5-2 weeks.
Blocked by: `answer.rs` is 1,940 lines and `send-keys` has no natural idempotency; effects become ambiguous exactly at the PTY write [fact F§7, E§Q3.4].
Spec delta: extends Phase S as S-E. Must precede mobile: a doubled "approve permission" on an agent about to run a command is a correctness bug, not a UX one.

## 2. Sequencing

```
now ──▶ P0+S (in flight)          ──▶ P1 ──▶ P2-P5 ──▶ P6
         │  + X6 invariants            │       │         │
         │  + S-E (X9)                 │       │         │
         ▼                             ▼       ▼         ▼
      W0 (X8) ──────────────────────────────────▶ R1 (X2+X4) ──▶ R2 (X1 stream)
         │                                              │             │
      T0 (X5) parallel, independent of all ─────────────┴──▶ M1 (X7) ──┘
                                              D1-D3 unchanged; D4 ⊂ R1
```

| Phase | Contents | Entry gate | Exit gate |
|---|---|---|---|
| P0+S | as planned, **plus** X6 invariants and S-E envelope | now | tripwires green; listener census and fan-out bench in CI with a ceiling |
| W0 | X8 version, capabilities, skew harness | any time, before P6 | skew harness green in both directions |
| T0 | X5 status store, normalizers, OSC contract | spike 1 passes | one status truth across TUI, web and CLI; no tier-2 row outranks a hook |
| R1 | X2 HostId, X4 WS plus device tokens and scope allowlist; absorbs D4 | W0 and P6 done | two boxes in one UI with a coverage census and `omitted_hosts` |
| R2 | X1 emulator, snapshot, per-viewer flow control | R1, spike 2 | phone-shaped first frame with colour, cursor and alt-screen |
| M1 | X7 mobile v1 | R1, R2, S-E | answer a needs-input banner in two taps |

Paint-into-corner items, each before the phase consuming it: X8 before any off-box client; X9 before mobile; X4 per-device tokens, since a retrofit means re-pairing every device and auditing every handler; X6 section naming and subscription before the second consumer; X2 HostId before a second host reaches the UI. The rest is additive.

## 3. Do not build

| Not building | Reopen when |
|---|---|
| Cloud relay: director, cells, splice, assignment, fencing | a client genuinely cannot dial the box, meaning a phone on cellular with no VPN |
| Daemon-owned PTY replacing tmux | spike 2 shows the tmux-hop snapshot is unusable, or native Windows enters scope |
| Checkpoint-and-log durable scrollback pipeline | tmux is no longer the scrollback of record |
| Native APNs or FCM push gateway | mobile ships and users need banners with the app suspended |
| Per-connection capability authz (the "v3 surface") | a non-operator or third-party client pairs |
| Input and resize driver-floor arbitration | two viewers can type, meaning phone input, or a phone at 40x20 attaches beside a desktop |
| Subagent roster rows | the dashboard shows child rows |
| Cross-box orchestration mailboxes with ack checkpoints | one box must run dispatches while the home client is offline |
| Regional placement, cell migration, evacuation | never for us; only with more than one relay cell |
| Rewriting the `ainb-web` renderer onto the shared core | after M1; P6 migrates only its daemon client |

## 4. Contradictions resolved

| Reports | Tension | Resolution |
|---|---|---|
| A§7(a) vs A§7(d) | tmux is our biggest asset / tmux is the hard mobile blocker | Both hold, at different layers. The blocker is the missing snapshot primitive, not tmux ownership, and A's own ADD 1 fixes it without replacing tmux. Residual risk is fidelity: spike 2 |
| A§7(a) vs A§7(d).3-4 | tmux gives N viewers free / tmux has no input or resize arbitration | Concurrent *viewing* is free; concurrent *typing* and per-viewer geometry are not, under any model. Our locked "no input lease, `window-size latest`" is right for TUI plus desktop, and breaks only when a phone of different geometry types. Defer the floor to phone input |
| E§Q1 vs locked plan B | the mirror is the riskier design | Not a reversal. B stays for the local renderer where it wins decisively on drift; section subscription is how remote and mobile use the same mechanism selectively |
| B§8.6 vs our spec | "the client drives the remote box" is wrong | B argues against a model we do not have: our `ssh -L` forwards to the *remote daemon's* socket, so the control plane is already on the box. The valid part is that forwarding is only a transport, and terminal bytes riding a second channel behind the same tunnel should consolidate |
| D§6 Avoid vs F§8 | pane text must not be primary / `list-panes` discovery is a real capability | Split the roles. Tier 2 keeps discovery of hand-started agents plus a tagged last-resort row, and never outranks a hook. Keep the readiness gate, fixture-pinned, since nothing else answers "can I type yet" [fact D§7.2] |

## 5. Three spikes worth running first

| # | What | Time | Flips |
|---|---|---|---|
| 1 | Write an OSC status frame from a wrapper inside a tmux pane with `allow-passthrough on`; check it reaches a `portable-pty` attach stream intact and that `capture-pane` does not eat it | 1 day | X5 tier ordering. On failure the in-band tier is worth nothing over tmux, hooks carry everything alone, and ADD 2 leaves the hybrid |
| 2 | Drive a real session (alt-screen, mouse tracking, wide chars, OSC 8) through `tmux attach` under `portable-pty` into a Rust VT emulator, serialize a snapshot, diff against the same agent on a direct PTY | 2-3 days | X1. Poor fidelity means mobile's first frame needs a daemon-owned PTY, a third sibling of the headless and ACP paths, reopening a "do not build" row |
| 3 | Extend the planned specta spike: generate TS for 19 sections on the real struct, then run a 2,000-update burst at 100 sessions through the Solid store with a listener census, comparing per-section sends against one transaction per drain | 2 days | X6. If per-drain batching cannot hold a CPU and scheduling-drift ceiling, section subscription becomes mandatory for the local renderer too, and P0's contract changes shape |

Spike 1 gates T0, spike 2 gates R2, spike 3 gates P0's contract. None blocks the keymap or Phase S work starting today.
