# Specification: federation, mobile, agent status on the shared core

**Generated from:** docs/plans/2026-09-11-multi-surface-decisions.md
**Interview date:** 2026-09-11, 3 rounds, 11 forks, all resolved by Stevie
**Version:** 1.0
**Extends:** docs/plans/2026-09-04-desktop-shared-core-spec.md (D1-D9 stay locked; this adds D10-D18 and phases W0, T0, R1, R2, M1)
**Research:** research/2026-09-11_multi-surface_*.md (gitignored dir, `git add -f`)
**Format:** diagram-first, tables second, no prose paragraphs

## Executive summary

| Question | Answer |
|---|---|
| What? | One UI drives N boxes with live streaming; a phone answers a needs-input banner in two taps; every surface reads one agent status |
| How? | Peer daemon per box over WebSocket + E2EE, tmux kept as PTY owner with a daemon-side emulator, daemon-owned status store, Expo mobile |
| When? | After P0+S land (running elsewhere); W0 and T0 parallel to P1-P5; R1 absorbs D4; R2 then M1 |

## Target shape

```
┌─ desktop (Tauri) ─┐  ┌─ TUI ─┐  ┌─ mobile (Expo) ─┐  ┌─ web (axum) ─┐
│ Vec<HostApp>      │  │ ainb  │  │ thin RPC client │  │ PWA + xterm  │
└────────┬──────────┘  └───┬───┘  └────────┬────────┘  └──────┬───────┘
         │ mirror+sub      │ mirror        │ sections+stream  │
         ▼                 ▼               ▼                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ TRANSPORT   unix sock (local) │ WebSocket + Noise IK (off-box)       │
│             carriers: LAN · tailnet · ssh -L · (relay slot reserved) │
│ AUTH        SO_PEERCRED (local) │ per-device token + scope allowlist │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼  one binary, one role per box
┌──────────────────────────────────────────────────────────────────────┐
│ ainb-hangar-daemon = the control plane, per box                      │
│  HostId · status store (single writer, provenance) · op-id ledger    │
│  event_outbox + after_revision replay · VT emulator · snapshot       │
│  driver floor: input generation + resize owner per viewer            │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼
                        tmux server (unchanged, PTY owner)
```

## Decisions D10-D18 (locked 2026-09-11)

| # | Decision | Locked answer | Rejected | Why |
|---|---|---|---|---|
| D10 | tmux | **Hybrid.** tmux stays PTY owner and multi-attach substrate; daemon adds a headless VT emulator fed by the attached stream for snapshot, status, per-viewer flow control. Spike 2 gates R2 scope | own daemon PTY; tmux only | keeps restart survival, hand-started discovery, `tmux attach` escape hatch; own PTY reopens only on native Windows or a failed spike 2 |
| D11 | host model | **Peer daemon on the box** with `HostId` on every wire type and schema row; `ssh -L` is one carrier among LAN and tailnet; socket path versioned by protocol, never by build; D4 rescoped into R1 | client-owned control plane; both models per box | agents keep running with the laptop closed; two boxes must not collide |
| D12 | relay | **None now.** Pairing offer carries an optional `relay` field from day one. Reopen path designed in: single-cell relay behind a managed identity provider and managed SQL when a client cannot dial the box | build now | tailnet covers laptop-to-box and phone-on-tailnet; relay is an operable service with an identity issuer dependency |
| D13 | off-box transport + auth | **WebSocket carrying today's JSON-RPC envelope, Noise IK via `snow`, host key pinned in the pairing offer, per-device revocable tokens with `scope` and expiry, scope-to-method allowlist checked before dispatch** | shared token over TLS; mTLS; NaCl box | revoke one device, narrow a phone's surface, no LAN cert problem, transcript binding comes with the pattern |
| D14 | agent status | **Six tiers:** hook push > ACP feed > OSC frame > process > transcript > pane text. Only tiers 0/1 open a turn or assert needs-input. Silence is `unverifiable` (we hold the pane) or `idle`, never `done`. **Daemon-owned single-writer store**, provenance per row, three clocks, `restored_unconfirmed` on hydrate. Provider order: Claude-compatible family, then OSC contract, then session-state family | pane regex primary; store per surface | one truth across TUI, web, desktop, phone; pane regex demoted to discovery, readiness gate, tagged last-resort row |
| D15 | renderer contract | **Plan B stays** with three invariants: frames name changed sections; one store transaction per channel drain, effects after commit; clients subscribe to a section subset. Lands in W0, not P0 | plan B as written; RPC catalog | P0 is running in another session; must land before the second consumer surface |
| D16 | mobile | **Expo / React Native, xterm in a webview, thin RPC client, interactive terminal in v1** | Tauri mobile; Swift + Kotlin | one codebase, mature keychain/camera/notification modules; interactive v1 pulls the driver floor into R2 |
| D17 | wire versioning | **One integer protocol version with a written bump rule, capability strings negotiated both ways, handshake-negotiated opcodes with permanent numbers, two-direction skew harness** | integer only; exact equality | an app-store phone cannot be force-upgraded; unknown opcodes hang silently |
| D18 | mutations | **Client-minted op id + expected fence on every mutation; daemon commits then replies; `created / adopted / replayed`; `accepted / rejected / unknown`; durable receipt at the pre-PTY-write boundary** | first-answer-wins only; client dedupe | a lost reply must not double-fire "approve" on an agent about to run a command |

Scope answers from the interview:

| Fork | Answer |
|---|---|
| native Windows in v2 | no; WSL only; it is the sole trigger that reopens D10 |
| driver floor (input generation counter, subscription-scoped resize owner) | R2, alongside the emulator; M1 consumes it |
| T0 timing | alongside P1, after spike 1 |
| W0 | own phase after P0+S land; contains D15, D17, D18 |

## Phases and gates

```
now ──▶ P0+S (running elsewhere) ──▶ P1 ──▶ P2-P5 ──▶ P6 ──▶ D1-D3 ──▶ D4 (⊂ R1)
                                     │
      W0 (D15, D17, D18) ── parallel to P1-P5, gates P6 ──▶ R1 (D11, D13) ──▶ R2 (D10 + floor)
                                     │                          │                 │
      T0 (D14) ── alongside P1, after spike 1 ──────────────────┴──▶ M1 (D16) ◀────┘
```

| Phase | Contents | Entry gate | Exit gate |
|---|---|---|---|
| W0 | protocol integer + capability strings + skew harness; op-id/fence/receipt envelope on every mutation; section naming + per-drain apply + subscription filter; listener census + fan-out bench with CI ceiling | P0+S merged; spike 4 number in hand | skew harness green both directions; every mutation carries an op id; census in CI |
| T0 | daemon status store, single writer, provenance, three clocks; Claude-family normalizer; OSC in-band frame schema; `agent_status` mirror section with own drain budget | spike 1 passes | one status truth across TUI, web, CLI; no tier-5 row outranks a hook; silence never `done` |
| R1 | `HostId` through proto, schema, three clients; WebSocket listener behind a transport trait; Noise IK pairing offer (QR, deep link, paste) with reserved relay field; device registry with scope + expiry; allowlist at dispatch; coverage census + `omitted_hosts`; absorbs D4 host switcher | W0, P6, spike 3 | two boxes in one desktop UI; laptop closed, agents keep running; revoke one device kills its socket |
| R2 | headless VT emulator fed by `portable-pty` attach; snapshot-then-tail (start, chunks, end) with monotonic seq; per-viewer batching with drain flush and `data_gap`; driver floor: generation-counted input owner, subscription-scoped resize owner | R1, spike 2 | phone-shaped first frame with colour, cursor, alt-screen; two viewers type without interleaving; phone at 40x20 beside desktop without thrash |
| M1 | Expo app: pair, host list, sessions with live status, transcript, send prompt, answer permission and question, cancel turn, interactive terminal with snapshot replay, notifications off the live socket, connection log | R1, R2, W0 | answer a needs-input banner in two taps; type into a session from the phone |

Paint-into-corner order, each before the phase that consumes it:

| Item | Before |
|---|---|
| D17 versioning + capabilities | any off-box client |
| D18 op ids | mobile |
| D13 per-device tokens | first pairing |
| D15 section naming + subscription | second consumer surface |
| D11 `HostId` | second host in any UI |
| D4 rescope | D4 starts |

## Spikes

| # | What | Time | Gates | Flips |
|---|---|---|---|---|
| 1 | OSC status frame from a wrapper in a tmux pane with `allow-passthrough on`; confirm intact on a `portable-pty` attach stream | 1 day | T0 | on failure, hooks carry everything alone over tmux |
| 2 | alt-screen, mouse, wide chars, OSC 8 through `tmux attach` under `portable-pty` into a Rust VT emulator; snapshot diffed against a direct PTY | 2-3 days | R2 | poor fidelity reopens daemon-owned PTY as a third execution mode |
| 3 | peer WS + Noise IK over tailnet and over `ssh -L`; daemon keeps working after desktop closes; reconnect and resync measured | 2-3 days | R1, D4 rescope | go/no-go for R1 scope |
| 4 | specta on the real 19 sections; 2,000-update burst at 100 sessions through the Solid store with a listener census; per-section sends vs one transaction per drain | 2 days | W0 | if per-drain batching cannot hold a ceiling, subscription becomes mandatory locally |

Spikes 1 and 4 can start now. None blocks P0+S.

## Components

| Component | Purpose | Reuse |
|---|---|---|
| `ainb-hangar-proto` | envelope, `HostId`, capability strings, op-id envelope | as-is, additive |
| `ainb-hangar-store` | `event_outbox`, `after_revision` replay, status store tables, device registry, op-id ledger | as-is plus migrations |
| `ainb-hangar-daemon` | transport trait over `UnixListener` and WebSocket; Noise IK; allowlist; status store single writer; emulator; snapshot; driver floor | change: `bind()` and `handle_conn` gain a trait |
| `ainb-hangar-client` | one client for desktop, TUI, web; transport enum `Unix \| Ws` | change: hardcoded `UnixStream` |
| `ainb-fleet-core` | `Transport` enum in `send/route.rs` gains the emulator-fed path; `classify()` stays pure as tier 5 | as-is |
| hook pipeline | 30 events, `events.jsonl`, byte-cursor ingest | as-is, becomes tier 0 |
| `ainb-web` | PtyBridge points at the daemon snapshot stream; daemon client replaced by `ainb-hangar-client` in P6 | change |
| Swift fleet app | generate `FleetWire.swift` from proto instead of 1,572 mirrored lines | change |
| `ainb-mobile` (new) | Expo app; generated TS types from Rust; build-enforced rule that wire validators never enter the bundle | new |

## Security requirements

- Local leg unchanged: `SO_PEERCRED` same-uid plus `auth/hello` token.
- Off-box leg: Noise IK handshake, host static key pinned in the pairing offer, transcript binds transport kind and `HostId`; per-device token with `scope` and expiry; revocation terminates open sockets immediately; allowlist checked before dispatch, not per handler.
- Pairing offer TTL bounded with clock-skew tolerance; three ways in (QR, deep link, paste), one parser.
- Relay reopen path: relay sees ciphertext only; host proof over a bound transcript; external identity provider issues the host-control token.
- Never fall back to local execution when a remote host is missing.
- Contact loss is `unverifiable`, never `exited`; only positive proof of exit authorises stop or retry.

## Edge cases

| Scenario | Expected behaviour |
|---|---|
| second host under a row cap | host-balanced round robin; `omitted_hosts` returned; UI shows the census |
| phone reply lost on cellular | retry carries the same op id; daemon returns `replayed` with the committed result |
| two clients answer one ASK | `created` for one, `adopted` for the other; `answered_by` names the winner |
| daemon restart with a phone watermark | status feed carries `seq` + `epoch`; epoch mismatch returns the whole retained buffer |
| unknown opcode from a newer client | negotiated at handshake; never sent to a peer lacking the capability |
| phone attaches to a running session | snapshot start, chunks, end, then tail; `data_gap{dropped}` triggers re-snapshot |
| two typists | generation-counted owner; loser's keystrokes rejected, not interleaved |
| device revoked | socket closed; token rejected at next hello |
| OSC frame eaten by tmux | spike 1 decides; hooks remain authoritative regardless |

## Do not build

| Not building | Reopen when |
|---|---|
| cloud relay | a client cannot dial the box; then single cell behind managed identity and managed SQL |
| daemon-owned PTY replacing tmux | spike 2 fails, or native Windows enters scope |
| checkpoint-and-log scrollback pipeline | tmux stops being the scrollback of record |
| native APNs / FCM push gateway | users need banners with the app suspended |
| per-connection capability authz beyond scope allowlist | a third-party client pairs |
| subagent roster rows | dashboard shows child rows |
| cross-box orchestration mailboxes | one box must run dispatches while the home client is offline |
| rewriting the `ainb-web` renderer onto the shared core | after M1 |

## Plan slices to write next

| Slice | Covers | `/plan` input | When |
|---|---|---|---|
| 2 | P1-P6 extraction | 2026-09-04 spec, unchanged | after P0+S land |
| 3 | D1-D3 desktop crate; D4 rescoped to R1 | 2026-09-04 spec + this spec D11 | after P2 |
| 4 | W0 + T0 | this spec | after P0+S land, spikes 1 and 4 done |
| 5 | R1 + R2 | this spec | after W0, P6, spikes 2 and 3 |
| 6 | M1 | this spec | after R2 |

## Open questions

- [ ] Rust VT emulator crate choice (`vt100`, `alacritty_terminal`, `wezterm-term`): decide inside spike 2.
- [ ] `HostId` encoding (`local` vs `remote:<stable id>`), and migration for existing `fleet_session` rows.
- [ ] Token expiry window and refresh flow for long-lived phone pairings.
- [ ] Which section subset a phone subscribes to by default.
- [ ] OSC number and payload schema for the in-band status frame.

---

*Generated through systematic interview of the plan author.*
