# Specification: federation, mobile, agent status on the shared core

**Generated from:** docs/plans/2026-09-11-multi-surface-decisions.md
**Interview date:** 2026-09-11, 3 rounds, 11 forks, all resolved by Stevie
**Version:** 1.1 (amended 2026-09-11 after a distinguished-engineer critique, verdict CAUTION 8/10; 30 amendments folded, see `research/2026-09-11_multi-surface_CRITIQUE-amendments.md`)
**Extends:** docs/plans/2026-09-04-desktop-shared-core-spec.md (D1-D9 stay locked; this adds D10-D18 and phases W0, T0, R1, R2, M1)
**Research:** research/2026-09-11_multi-surface_*.md (gitignored dir, `git add -f`)
**Format:** diagram-first, tables second, no prose paragraphs

## Executive summary

| Question | Answer |
|---|---|
| What? | One UI drives N boxes with live streaming; a phone answers a needs-input banner in two taps; every surface reads one agent status |
| How? | Peer daemon per box over WebSocket + Noise IK, tmux kept as PTY owner with a daemon-side emulator fed by tmux control mode, status store as the `fleet_session` family, Expo mobile over a uniffi Rust wire crate |
| When? | After P0+S land (running elsewhere); W0-wire after S-B, W0-mirror after P1; T0-daemon alongside P1; R1 after P6; R2 then M1 |

## Target shape

```
┌─ desktop (Tauri) ─┐  ┌─ TUI ─┐  ┌─ mobile (Expo) ─┐  ┌─ web (axum) ─┐
│ Vec<HostApp>      │  │ ainb  │  │ ainb-wire-mobile│  │ PWA + xterm  │
└────────┬──────────┘  └───┬───┘  └────────┬────────┘  └──────┬───────┘
         │ sections        │ sections      │ RPC subs+stream  │ RPC
         ▼ (ainb-app)      ▼ (ainb-app)    ▼                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ TRANSPORT   unix sock (local) │ WebSocket + Noise IK (off-box)       │
│             carriers: LAN · tailnet · ssh -L · (relay slot reserved) │
│ AUTH        SO_PEERCRED (local) │ per-device token + scope allowlist │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼  one binary, one role per box
┌──────────────────────────────────────────────────────────────────────┐
│ ainb-hangar-daemon = the control plane, per box                      │
│  HostId (ULID) · status store = fleet_session family · op-id ledger  │
│  event_outbox + after_revision replay · VT emulator (cache) · snapshot│
│  driver floor: input generation + resize owner per daemon viewer     │
└──────────────────────────────┬───────────────────────────────────────┘
                               ▼  tmux -C control mode (raw pane bytes)
                        tmux server (unchanged, PTY owner, truth)
```

| Contract | Between | Selectivity |
|---|---|---|
| hangar RPC | daemon and every client, phone included | per-method subscriptions (7 arms today), `HostId`-scoped |
| section mirror | `ainb-app` and its renderers (ratatui, Tauri Channel) | D15 subset subscription |

## Decisions D10-D18 (locked 2026-09-11)

| # | Decision | Locked answer | Rejected | Why |
|---|---|---|---|---|
| D10 | tmux | **Hybrid.** tmux stays PTY owner, multi-attach substrate, and truth; daemon adds a headless VT emulator as a cache for snapshot, status, per-viewer flow control. Feed is tmux control mode (`tmux -C attach`, raw `%output` bytes, excluded from window sizing) or a rendered attach with `-f ignore-size,read-only`; spike 2 picks. The daemon's tmux client never influences pane size | own daemon PTY; tmux only | keeps restart survival, hand-started discovery, `tmux attach` escape hatch; own PTY reopens only on native Windows or a failed spike 2 on both feeds |
| D11 | host model | **Peer daemon on the box** with `HostId` on every wire type and schema row; `ssh -L` is one carrier among LAN and tailnet; socket path versioned by protocol, never by build; D4's host switcher and transport move into R1. Every cross-host surface (palette, OS notification payload, deep link, mobile route, `answered_by`) carries `SessionRef { host_id, session_key }`; a bare `session_key` is a type error outside a single-host `HostApp` | client-owned control plane; both models per box | agents keep running with the laptop closed; two boxes must not collide |
| D12 | relay | **None now.** Pairing offer carries an optional `relay` field from day one. Reopen path designed in: single-cell relay behind a managed identity provider and managed SQL when a client cannot dial the box | build now | tailnet covers laptop-to-box and phone-on-tailnet; a relay is an operable service with an identity issuer dependency |
| D13 | off-box transport + auth | **WebSocket carrying today's JSON-RPC envelope, Noise IK via `snow`, host static key pinned in the pairing offer, single-use invite redeemed inside the Noise session, per-device revocable tokens bound to the device static key with `scope` and expiry, scope allowlist on methods at dispatch and on event families at subscribe.** Phone crypto and framing live in a uniffi-compiled Rust crate, never in JavaScript | shared token over TLS; mTLS; NaCl box | revoke one device, narrow a phone's surface, no LAN cert problem, transcript binding comes with the pattern |
| D14 | agent status | **Six tiers:** hook push > ACP feed > OSC frame > process > transcript > pane text. Only tiers 0/1 open a turn or assert needs-input. Silence is `unverifiable` (we hold the pane) or `idle`, never `done`. **The store IS the `fleet_session` + `fleet_event` family**, extended with provenance, tier, three clocks, incarnation; `attention` rows are a projection written by the same single-writer apply path in the same transaction; `fleet_session.attention_state` has no other producer; `sweep_once` becomes a drift assertion that logs and never mutates. Provider order: Claude-compatible family, then OSC contract, then session-state family | pane regex primary; store per surface; a third status table | one truth across TUI, web, desktop, phone; the 732-vs-7 attention drift measured today must not get a third writer |
| D15 | renderer contract | **Plan B stays** with three invariants: frames name changed sections; one store transaction per channel drain, effects after commit; renderers subscribe to a section subset. Lands in W0-mirror after P1. Justified by the measured renderer fan-out; the phone never consumes sections, it consumes RPC subscriptions | plan B as written; RPC catalog | P0 is running in another session; must land before the second renderer |
| D16 | mobile | **Expo / React Native, xterm in a webview, thin RPC client over `ainb-wire-mobile`, interactive terminal in v1 behind a separate `mobile+type` pairing scope** | Tauri mobile; Swift + Kotlin | one codebase, mature keychain/camera/notification modules; interactive v1 pulls the driver floor into R2 |
| D17 | wire versioning | **One integer `PROTOCOL_VERSION` carried in `auth/hello` as `{min, max}`, capability strings negotiated both ways, handshake-negotiated opcodes with permanent numbers, two-direction skew harness including the local leg.** `FLEET_PROTOCOL_VERSION` frozen at 2 forever; `fleet/negotiate` stays as a compatibility echo; its 25 ids append to the one catalogue | integer only; exact equality; two integers | an app-store phone cannot be force-upgraded; unknown opcodes hang silently |
| D18 | mutations | **Client-minted opaque 128-bit op id + a mutation-specific fence on every mutation; daemon commits then replies; `created / adopted / replayed`; `accepted / rejected / unknown`; receipt lifecycle written in the same SQLite transaction as the state flip.** `FleetActionParams.request_id` is already the op id and `fleet_action_receipt` already the ledger for that family; W0 renames nothing on the wire | first-answer-wins only; client dedupe; a second idempotency vocabulary | a lost reply must not double-fire "approve" on an agent about to run a command |

Scope answers from the interview:

| Fork | Answer |
|---|---|
| native Windows in v2 | no; WSL only; it is the sole trigger that reopens D10 |
| driver floor (input generation counter, subscription-scoped resize owner) | R2, alongside the emulator; governs daemon-mediated viewers only |
| T0 timing | T0-daemon alongside P1, after spike 1; T0-section after P1 |
| W0 | W0-wire after S-B merges; W0-mirror after P1 merges |

## Contracts in detail

### D11 host identity

- `HostId` is a ULID minted once at first daemon boot, persisted in `hangar.db` table `daemon_identity { host_id, host_static_pubkey, display_name, created_at }`. Never derived from hostname. Backup restore keeps the id; fresh install mints a new one.
- A client holding a pairing whose `host_id` matches but whose pinned static key differs refuses with `peer_changed` and requires re-pair.
- Display name is client-side and renameable; the wire never carries it.
- Existing `fleet_session` rows migrate with `host_id = <this daemon's id>`.
- Proto gains `SessionRef`; a tripwire asserts no `String` session key in `ainb-desktop` shell types.

### D13 pairing, keys, scopes

Pairing offer and redemption:

```
offer = { v, host_id, host_static_pubkey, endpoints[], invite_id, invite_secret, expires_at, relay? }
  QR · deep link · paste ──▶ one parser (in ainb-wire-mobile / ainb-hangar-client)
phone ──Noise IK (initiator static = device key)──▶ daemon
      ──{ invite_secret }──▶ consumed once ──▶ { device_id, device_token, scope, expires_at }
device_token bound to the initiator static key that redeemed it; any other key ──▶ UNAUTHORIZED, logged
```

- TTL 5 min, 30 s skew leeway, 5 attempts then the invite is burned.
- Desktop shows "paired: <device name>, scope <s>" with one-click revoke for 60 s after each pairing.
- Host static key: generated at first boot, stored via `ainb-hangar-secrets` (keychain) with a `0600` file fallback under `{home}/hangar/`, backed by the `daemon_identity` row. `ainb hangar host-key rotate` mints a new key and a `key_rollover` record signed by the old key; a device accepts the rollover once on next connect and re-pins. `--revoke-all` burns every pairing.

| Scope | May call | May receive | Notes |
|---|---|---|---|
| operator | everything | everything | local unix leg only (`SO_PEERCRED`) |
| desktop | everything except device admin, daemon stop, transcript prune | everything | laptop pairing default |
| mobile | `fleet/snapshot`, `fleet/subscribe`, `attention/list`, `attention/answer`, `fleet/message_send`, `fleet/action{cancel}`, `fleet/transcript_*`, `terminal/attach` | fleet, attention, transcript events only | phone default |
| mobile+type | mobile plus `terminal/input`, `terminal/resize` | as mobile | separate toggle on the pairing screen: "this device may type into terminals" |
| admin | `device_list`, `device_revoke`, `device_rescope` | `ConnectionsChanged` | granted to the first paired desktop; revocable |

- The allowlist filters event families on subscribe as well as methods on dispatch; a scope never receives an event whose read method it cannot call.
- Off-box, one persistent Noise session per host multiplexes requests and subscriptions; the fresh-dial-per-call path in `hangar-client` stays local-only.

Listener policy:

- WS bind mirrors `ainb-web::check_bind_security`: default bind is the tailnet interface when one exists, else loopback (reach over `ssh -L`); `--bind <lan-ip>` requires an explicit flag and logs a warning at every boot.
- Pre-auth limits: first frame within 2 s; 30 handshake attempts per minute per source; 8 open unauthenticated sockets per source; over-cap accept closes with WS 1013 and a `Retry-After` hint, never a silent drop.
- Close codes in proto: 4401 unauthenticated, 4403 revoked, 4409 protocol incompatible, 4429 rate limited, 4503 draining. Revoked latches "re-pair" on the client, never retries.

### D14 status store

| Clock | Set by | Used for |
|---|---|---|
| `received_at` | daemon, monotonic per-connection watermark | ordering, replay cut |
| `evidence_observed_at` | the source (hook payload ts, OSC frame ts, process exit ts) | staleness; never moved by a replay |
| `state_started_at` | daemon, when `state` last changed | "working since", attention ordering |
| `mirrored_received_at` | the mirroring client, its own clock | decay on a mirror; a client never subtracts a remote stamp from local now |

- Per-host `reachability { reachable | unreachable{since} | stale{last_seq} }` is a `HostApp` field, never folded into a row's `state`; rows on an unreachable host render frozen with `stale_since`, not `unverifiable`.
- Spool: tier 0 = `events.jsonl` (exists; cursor replay; `att:<session>:<event_id>` idempotency). Tier 1 = none needed (ACP children die with the daemon, reclaimed by runtime instance id). Tier 2 = none (OSC bytes during downtime are lost; rows hydrate `restored_unconfirmed` until the next frame).
- Boot order: hydrate rows and stamp `restored_unconfirmed` on every non-`exited` row, drain the tier-0 cursor, then start the feed. A hydrated row is confirmed only by a tier 0/1 event in this daemon incarnation.
- Fence: every row carries `session_incarnation` (`process_start_fingerprint` for tmux panes, pool session id for ACP). An event whose incarnation differs from the live row is `restart` (new row; old row `exited` only with process proof) or `suppress` (older incarnation). Store idempotency key: `(host_id, session_key, tier, event_id)`.
- Unknown event names per provider are counted (`status_unknown_event{provider,name}`) and surfaced in `ainb doctor`; a normalizer answers `null` for an unknown name, never an error.
- Rollback: `[fleet.status] legacy_classify_primary = true` restores today's `classify()`-first ordering for one release after T0 ships; removed in T0+2.

### D17 versioning

- `auth/hello` becomes `{ token, surface?, device?, protocol: {min, max}, capabilities: [..] }`; the reply carries the daemon's `{ protocol, capabilities }`. `HelloParams` reaches this final shape once, in W0-wire, after S-B; R1 adds nothing to hello.
- Bump rule: the integer bumps only on removing a method or field, changing a field's meaning, or changing framing, auth, or crypto. New methods, optional fields, and new event kinds are capability strings. The catalogue is a committed file; a test fails if a string is removed.
- The daemon binds `hangar.sock` (current) and symlinks `hangar-v<N>.sock` for every protocol version it serves; a client dials the versioned path when it knows one, else `hangar.sock`.
- Skew harness matrix includes the local leg: {sidecar daemon, installed daemon} x {TUI, desktop, web, CLI, Swift app} at N and N-1.

### D18 mutation envelope

- Ledger key `(host_id, principal, op_id)`; principal is `device:<id>` off-box and `local` on the unix leg. A matching `op_id` from a different principal is `rejected{reason: op_id_foreign}`.
- Retention: 7 days or 100k rows per host, whichever first. A retry after eviction returns `unknown{reason: op_expired}`; the client shows "could not confirm, check the session". Op ids are opaque; no timestamp freshness rule, so a phone with a wrong clock is never rejected for skew.
- `adopted` requires the mutation body fingerprint to match the committed one; a different body against a committed row is `rejected{already_answered_by}`.

| Mutation | Fence | Stale means |
|---|---|---|
| `attention/answer` | attention row `version` and `state = open` | already answered: `rejected{already_answered_by}` |
| `fleet/message_send`, prompt send | `fleet_session.lifecycle_updated_at` observed by the client | the agent moved on: `rejected{turn_advanced}` unless `force` |
| `fleet/action{cancel, kill}` | `session_incarnation` | a different process owns the name: `rejected{incarnation_mismatch}` |
| `device_revoke` | registry `version` | concurrent admin edit: `rejected{conflict}` |

Two tiers of guarantee:

1. Generic dedupe at dispatch for every mutation, keyed by ledger row storing the serialized reply (`replayed`).
2. Transactional receipts with a write boundary only for PTY-effecting mutations: `attention/answer`, prompt send, `fleet/action{cancel,kill}`, `terminal/input`. Adding a handler to this tier is a checklist item, not the default.

Receipt lifecycle `claimed -> writing -> delivered | failed | unknown`, written in the same SQLite transaction as the state flip, never via the event outbox. `writing` is set immediately before the first byte reaches the PTY. On boot every receipt still in `writing` becomes `unknown{effects_ambiguous}`; the row is not reopened (a retry could double-type) and not closed silently: it surfaces as an attention row of kind `delivery_unconfirmed` naming the answer text, and only an operator closes it. A retry against a `claimed` or `writing` receipt returns `replayed` with the receipt state.

## Phases and gates

```
now ──▶ P0+S (running elsewhere) ──▶ P1 ──▶ P2-P5 ──▶ P6 ──▶ D1-D3 ──▶ D4' (updater, release matrix)
              │ S-B merged              │ P1 merged
              ▼                         ▼
        W0-wire (D17, D18)        W0-mirror (D15)   T0-section (agent_status = section 20)
              │
        T0-daemon (D14) ── alongside P1, after spike 1
              │
              └──▶ R1 (D11, D13, D4 host switcher + transport) ──▶ R2 (D10 + floor) ──▶ M1 (D16)
```

| Phase | Contents | Entry gate | Exit gate |
|---|---|---|---|
| W0-wire | `PROTOCOL_VERSION` in hello + capability catalogue + skew harness incl. local leg; op-id ledger, generic dedupe, receipts for PTY-effecting mutations; versioned socket symlinks | S-B merged; spike 4 number in hand; spike 8 p99 | a proto test walks a committed `MUTATING_METHODS` list and asserts each params struct embeds `MutationEnvelope`; a daemon test replays every mutating method twice and asserts one `created` and one `replayed` each; skew harness green both directions |
| W0-mirror | section naming + per-drain apply + subscription filter in `ainb-app`; listener census + fan-out bench with CI ceiling | P1 merged | census in CI under the spike-4 ceiling |
| T0-daemon | status store on the `fleet_session` family; provenance, tier, clocks, incarnation; attention projection in the same transaction; Claude-family normalizer; OSC in-band frame schema; RPC surface | spike 1 passes; spike 8 p99 | one fixture session driven hook to store: `ainb fleet needs --format json`, `GET /api/needs`, and the TUI fleet panel `TestBackend` snapshot produce identical `(host_id, session_key, state, provenance, tier, evidence_observed_at)` tuples; table test: tier-0 `waiting` then tier-5 `idle` from one pane stays `waiting` with provenance `hook`; property test: no event sequence ending in silence yields `done`; zero rows where `attention.open` disagrees with `fleet_session.attention_state` after a 1,000-event replay fixture |
| T0-section | `agent_status` as section 20 in `ainb-app` with its own drain budget | P1 merged, T0-daemon merged | fleet panel renders from the section with the same tuples |
| R1 | `HostId` ULID + `daemon_identity` + `SessionRef` through proto, schema, three clients; WS listener behind a transport trait; Noise IK; single-use invite; device registry with scope + expiry; allowlist on dispatch and subscribe; close codes; pre-auth limits; per-host reconnect with jittered backoff 1 s to 60 s, at most 2 concurrent resyncs per client, ordered by last-active host; coverage census `covered | omitted_by_cap | unreachable{since} | stale{head_revision, since}`; D4 host switcher | W0-wire, P6, spike 3 | two boxes in one desktop UI; start a session, kill the client mid-turn, wait 60 s, reconnect with `after_revision`, assert `Complete` replay and the transcript advanced; two devices connected, revoke one, its socket closes with 4403 within 1 s, its next hello is refused, the other device's subscription undisturbed |
| R2 | headless VT emulator fed by tmux control mode (or `-f ignore-size,read-only` attach per spike 2); emulator is a cache: live window 1,000 rows per session (env-tunable 100-5,000), deeper scrollback on demand from `capture-pane -e -S -<n>`; feed loss: reattach with backoff 250 ms/1 s/4 s, `data_gap{reason: feed_lost}` to every viewer, rebuild from redraw, rows `restored_unconfirmed` until next tier 0/1 event; snapshot-then-tail with monotonic seq; per-viewer batching with drain flush and `data_gap`; driver floor: generation-counted input owner, subscription-scoped resize owner, daemon-mediated viewers only | R1, spike 2, spike 7 | emulator snapshot at 40x20 vs a direct PTY driving the same fixture (alt-screen TUI, OSC 8, wide chars) byte-equal after ANSI normalisation; two daemon-mediated viewers send 1,000-byte runs concurrently and the PTY receives each run contiguous, loser rejected with `floor_denied`; pane resize counter at or below 2 per subscription change over a 60 s co-view, desktop geometry restored within 250 ms of phone detach |
| M1 | Expo app over `ainb-wire-mobile`: pair, host list, sessions with live status, transcript, send prompt, answer permission and question, cancel turn, interactive terminal behind `mobile+type`, notifications off the live socket, connection log; heartbeat 15 s with two missed probes as dead; background grace suspends rather than tears down; reconnect on foreground with `after_revision` replay; connection log persisted | R1, R2, W0-wire, spikes 5 and 6 | Maestro or Detox flow against a fixture daemon: banner tap, answer tap; attention row `answered` with `answered_by = device:<id>` and receipt `delivered`; foreground or within the OS background grace window only; suspended-app banners out of scope until the push row reopens |

Paint-into-corner order, each before the phase that consumes it:

| Item | Before |
|---|---|
| D17 versioning + capabilities | any off-box client |
| D18 op ids | mobile |
| D13 per-device tokens, single-use invite | first pairing |
| D15 section naming + subscription | second renderer |
| D11 `HostId` + `SessionRef` | second host in any UI |
| D4 rescope (host switcher + transport to R1; updater + release matrix stay in the desktop track after D3) | D4 starts |
| `HelloParams` final shape | W0-wire, once |

## Spikes

| # | What | Time | Gates | Flips |
|---|---|---|---|---|
| 1 | OSC status frame from a wrapper in a tmux pane with `allow-passthrough on`; confirm intact on a `portable-pty` rendered attach | 1 day | T0-daemon | on failure with feed (a), the in-band tier still works over feed (b) control mode; hooks alone only if both fail |
| 2 | Two feeds side by side: (a) rendered `tmux attach -f ignore-size,read-only` under `portable-pty`; (b) `tmux -C attach` control mode on a pipe with `refresh-client -A %<pane>:on` and `-f pause-after=2`. Drive alt-screen, mouse tracking, wide chars, OSC 8, a custom OSC status frame; feed a Rust VT emulator; diff snapshots against a direct PTY; measure RSS per emulator at 100 sessions and CPU during `cat` of 50 MB; confirm neither feed changes `#{window_width}` seen by a second attached client | 2-3 days | R2 | picks the feed; poor fidelity on both reopens daemon-owned PTY |
| 3 | peer WS + Noise IK over tailnet and over `ssh -L`; daemon keeps working after desktop closes; reconnect and resync measured; `cat` 50 MB through the terminal stream under both carriers against the 2 s gate; laptop sleep/wake against 5 hosts x 3 devices with connection and resync counts recorded | 2-3 days | R1, D4 rescope | go/no-go for R1 scope; a throughput miss moves the terminal stream to a second WS without Noise inside `ssh -L` |
| 4 | specta on the real 19 sections; 2,000-update burst at 100 sessions through the Solid store with a listener census; per-section sends vs one transaction per drain | 2 days | W0-mirror | if per-drain batching cannot hold a ceiling, subscription becomes mandatory locally |
| 5 | Noise IK handshake and AEAD framing in the Expo app through a uniffi-compiled Rust crate on a real iPhone and a real Android device; cold-start cost, bundle size, keychain custody of the device static key | 2 days | M1, `ainb-wire-mobile` | if uniffi is unworkable, D13 needs a JS Noise implementation review before M1, or the phone speaks TLS 1.3 with a pinned host cert as a second transport kind |
| 6 | iOS and Android background socket lifetime with a 15 s heartbeat: how long a live socket survives suspension; whether a locally scheduled needs-input banner fires after 1, 5, 30 minutes | 1 day | M1 gate wording | if grace is under 60 s, the push reopen row moves to M1+1 |
| 7 | tmux control mode flow control: one pane floods 100 MB/s while a second is idle; confirm `pause-after` pauses only the flooding pane, daemon RSS stays bounded, `%continue` resumes without loss | 0.5 day | R2 | if control mode cannot pause per pane, per-viewer batching must drop at the daemon and `data_gap` becomes routine under load |
| 8 | SQLite write path at scale: 100 sessions, hook rate 10/s, status apply + ledger insert + outbox append in one transaction; p99 commit latency and WAL growth over one hour with retention running | 1 day | T0-daemon, W0-wire | if p99 exceeds 50 ms the status store needs one transaction per drain tick, not one per event |

Spikes 1, 4, 7, 8 can start now. None blocks P0+S.

## Components

| Component | Purpose | Reuse |
|---|---|---|
| `ainb-hangar-proto` | envelope, `HostId`, `SessionRef`, capability catalogue, `MutationEnvelope`, close codes | as-is, additive |
| `ainb-hangar-store` | `event_outbox`, `after_revision` replay, `daemon_identity`, `fleet_session` family extended for status, `fleet_action_receipt` as the ledger, device registry | as-is plus migrations |
| `ainb-hangar-daemon` | transport trait over `UnixListener` and WebSocket (`rpc/mod.rs:240 bind()`, `:352 serve_conn`); Noise IK; invite redemption; allowlist; single-writer status apply; emulator; snapshot; driver floor; tmux version floor check at boot | change |
| `ainb-hangar-client` | one client for desktop, TUI, web; transport enum `Unix \| Ws`; persistent multiplexed session off-box | change: three hardcoded `UnixStream::connect` sites |
| `ainb-hangar-secrets` | host static key custody, keychain with `0600` fallback | new or extend existing |
| `ainb-fleet-core` | `Transport` enum in `send/route.rs` gains the emulator-fed path; `classify()` stays pure as tier 5 | as-is |
| hook pipeline | 30 events, `events.jsonl`, byte-cursor ingest | as-is, becomes tier 0 |
| `ainb-web` | PtyBridge points at the daemon snapshot stream; daemon client replaced by `ainb-hangar-client` in P6 | change |
| Swift fleet app | `FleetWire.swift` generated by `typeshare` from `ainb-hangar-proto` (Swift, Kotlin, TypeScript from one annotation set); `tauri-specta` stays for renderer sections only; CI byte-diffs generated files | change |
| `ainb-wire-mobile` (new) | Rust crate compiled for iOS and Android via uniffi: Noise IK over the WS frame, pairing offer parser, op-id minting, message framing, keychain-backed device key; Expo calls it through a thin TS binding; no crypto and no wire parsing in JavaScript | new; shares `ainb-hangar-proto` and `snow` with the daemon |
| `ainb-mobile` (new) | Expo app; generated TS types; build-enforced rule that wire validators never enter the bundle | new |

## Security requirements

- Local leg unchanged: `SO_PEERCRED` same-uid plus `auth/hello` token, scope `operator`.
- Off-box leg: Noise IK handshake, host static key pinned in the pairing offer, transcript binds transport kind and `HostId`; single-use invite; per-device token bound to the redeeming static key with `scope` and expiry; revocation terminates open sockets with 4403 immediately; allowlist checked before dispatch and on subscribe, not per handler.
- The daemon's tmux client never influences pane size: `-f ignore-size` on a rendered attach, or control mode.
- Never log tokens, invite secrets, pairing offers, Noise keys, or terminal bytes; log digests only.
- Relay reopen path: relay sees ciphertext only; host proof over a bound transcript; external identity provider issues the host-control token.
- Never fall back to local execution when a remote host is missing.
- Contact loss is `unverifiable` or `stale`, never `exited`; only positive proof of exit authorises stop or retry.

## Operability

- `hangar/health` gains counters: connections by scope and transport; resyncs and `SnapshotReset` by reason; `data_gap` by reason; ops by disposition; receipts in `unknown`; tier-0 silence seconds per session (p50, max); emulator RSS total; `status_unknown_event{provider,name}`.
- Retention from day one: ledger 7 d; status events 30 d or the existing `fleet_retention` byte ceiling; device registry unbounded with `last_seen_at` shown; revoked rows kept 90 d for audit.
- `ainb doctor` reports the tmux version floor (`allow-passthrough` needs 3.3; `ignore-size` and `pause-after` need 3.2) and degrades tier 2 with a warning below it.

## Edge cases

| Scenario | Expected behaviour |
|---|---|
| listing across hosts | census per host: `covered \| omitted_by_cap \| unreachable{since} \| stale{head_revision, since}`; the UI never merges a stale host's rows into a fresh list without the badge |
| phone reply lost on cellular | retry carries the same op id; daemon returns `replayed` with the committed result |
| same answer twice from two clients | `created` then `adopted` |
| different answers from two clients | `created` then `rejected{already_answered_by}`; `answered_by` names the winner |
| daemon crash between claim and `send-keys` | receipt `writing` becomes `unknown{effects_ambiguous}` at boot; attention row `delivery_unconfirmed` for an operator; retry returns `replayed` with that state |
| daemon restart with a phone watermark | status feed carries `seq` + `epoch`; epoch mismatch returns the whole retained buffer |
| unknown opcode from a newer client | negotiated at handshake; never sent to a peer lacking the capability |
| phone attaches to a running session | snapshot start, chunks, end, then tail; `data_gap{dropped}` triggers re-snapshot |
| two daemon-mediated typists | generation-counted owner; loser's keystrokes rejected with `floor_denied`, never interleaved |
| native tmux client attached while a phone holds the floor | floor cannot enforce; presence badge "native client attached, input not arbitrated" from `tmux list-clients`; the floor owner sees it before typing |
| tmux server exits | pane processes get SIGHUP; daemon marks rows `exited` only after `kill(pane_pid, 0)` fails, `unverifiable` until then; viewers get `data_gap{reason: session_gone}` and the tab closes into the recovery flow |
| daemon feed dies, tmux alive | viewers see `data_gap`, re-snapshot within 5 s; no keystroke lost because input goes through `send-keys`, not the feed |
| device revoked while its box is offline | revocation is a box-local write; the token is unusable until the box is up, then rejected at hello; expiry is the backstop; `admin` scope on any paired device may revoke |
| box reinstalled with the same hostname | new `host_id` and static key; old pairing shows "identity changed, re-pair"; old rows stay under the old host until removed |
| host removed in the desktop with live sessions | desktop forgets the pairing and asks the box to revoke its own device; the box and its agents are untouched |
| clock skew between phone and box | pairing TTL uses 30 s leeway; op ids carry no timestamp; status decay uses `mirrored_received_at` |
| desktop sidecar daemon older than the installed `ainb` | flock winner serves; loser's clients negotiate down; a client that cannot negotiate shows "daemon protocol too old, restart from the newer binary" and never spawns a second daemon |
| OSC frame eaten by tmux | spike 1 and 2 decide the feed; hooks remain authoritative regardless |
| provider CLI renames a hook event | `status_unknown_event` counter climbs, `ainb doctor` names the provider; rollback flag restores `classify()`-first for one release |

## Do not build

| Not building | Reopen when |
|---|---|
| cloud relay | a client cannot dial the box; then single cell behind managed identity and managed SQL |
| daemon-owned PTY replacing tmux | spike 2 fails on both feeds, or native Windows enters scope |
| checkpoint-and-log scrollback pipeline | tmux stops being the scrollback of record |
| native APNs / FCM push gateway | M1 ships; the gateway sends content-free pings and the phone fetches the body over the E2EE channel; spike 6 may pull this to M1+1 |
| per-connection capability authz beyond the scope table | a third-party client pairs |
| subagent roster rows | dashboard shows child rows |
| cross-box orchestration mailboxes | one box must run dispatches while the home client is offline |
| rewriting the `ainb-web` renderer onto the shared core | after M1 |

## Plan slices to write next

| Slice | Covers | `/plan` input | When |
|---|---|---|---|
| 2 | P1-P6 extraction | 2026-09-04 spec, unchanged | after P0+S land |
| 3 | D1-D3 desktop crate; D4' updater + release matrix | 2026-09-04 spec + this spec D11 | after P2 |
| 4 | W0-wire + T0-daemon (+ W0-mirror, T0-section once P1 merges) | this spec | after S-B lands; spikes 1, 4, 7, 8 done |
| 5 | R1 + R2 | this spec | after W0-wire, P6, spikes 2 and 3 |
| 6 | M1 | this spec | after R2, spikes 5 and 6 |

## Open questions

- [x] `HostId` identity source: ULID in `daemon_identity`, see D11 contract.
- [ ] Rust VT emulator crate choice (`vt100`, `alacritty_terminal`, `wezterm-term`): decide inside spike 2.
- [ ] Token expiry window and refresh flow for long-lived phone pairings.
- [ ] Which RPC subscriptions a phone opens by default (fleet, attention, transcript for the open session only).
- [ ] OSC number and payload schema for the in-band status frame.
- [ ] tmux version floor policy: hard refuse vs degrade tier 2 below 3.2.

---

*Generated through systematic interview of the plan author; amended after critique.*
