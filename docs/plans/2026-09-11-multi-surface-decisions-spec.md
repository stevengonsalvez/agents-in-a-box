# Specification: federation, mobile, agent status on the shared core

**Generated from:** docs/plans/2026-09-11-multi-surface-decisions.md
**Interview date:** 2026-09-11, 3 rounds, 11 forks, all resolved by Stevie
**Version:** 1.4 (1.1 amended 2026-09-11 after a distinguished-engineer critique, verdict CAUTION 8/10, 30 amendments folded, see `research/2026-09-11_multi-surface_CRITIQUE-amendments.md`; 1.2 folds spikes 1 and 7; 1.3 folds spikes 4, 8, 9 and the provider daemon-reuse hazard; 1.4 folds spike 3)
**Extends:** docs/plans/2026-09-04-desktop-shared-core-spec.md (D1-D9 stay locked; this adds D10-D18 and phases W0, T0, R1, R2, M1)
**Execution view:** `docs/plans/2026-09-12-desktop-programme.md` (one DAG over both specs, node states, gates)
**Integration branch:** `v2` (cut from `main` 2026-09-11). Every phase PR from this spec and the base spec targets `v2`, not `main`; `v2` merges to `main` as a whole when M1 or an earlier agreed cut lands.
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
| D10 | tmux | **Hybrid.** tmux stays PTY owner, multi-attach substrate, and truth; daemon adds a headless VT emulator as a cache for snapshot, status, per-viewer flow control. **Feed is tmux control mode** (`tmux -C attach`, raw `%output` bytes, excluded from window sizing): spike 1 showed it delivers bare and DCS-wrapped OSC frames 20/20 at server defaults while a rendered attach needs `allow-passthrough on` plus the DCS envelope and `capture-pane` never sees OSC. A rendered attach is not a fallback feed: on tmux 3.4 `-f ignore-size` does not stop the resize; only `window-size manual` plus `resize-window` pins it. Spike 2 now measures fidelity of the control-mode feed only | own daemon PTY; tmux only; rendered attach as feed | keeps restart survival, hand-started discovery, `tmux attach` escape hatch; own PTY reopens only on native Windows or a failed spike 2 |
| D11 | host model | **Peer daemon on the box** with `HostId` on every wire type and schema row; `ssh -L` is one carrier among LAN and tailnet; socket path versioned by protocol, never by build; D4's host switcher and transport move into R1. Every cross-host surface (palette, OS notification payload, deep link, mobile route, `answered_by`) carries `SessionRef { host_id, session_key }`; a bare `session_key` is a type error outside a single-host `HostApp` | client-owned control plane; both models per box | agents keep running with the laptop closed; two boxes must not collide |
| D12 | relay | **None now.** Pairing offer carries an optional `relay` field from day one. Reopen path designed in: single-cell relay behind a managed identity provider and managed SQL when a client cannot dial the box | build now | tailnet covers laptop-to-box and phone-on-tailnet; a relay is an operable service with an identity issuer dependency |
| D13 | off-box transport + auth | **WebSocket carrying today's JSON-RPC envelope, Noise IK via `snow`, host static key pinned in the pairing offer, single-use invite redeemed inside the Noise session, per-device revocable tokens bound to the device static key with `scope` and expiry, scope allowlist on methods at dispatch and on event families at subscribe.** Phone crypto and framing live in a uniffi-compiled Rust crate, never in JavaScript | shared token over TLS; mTLS; NaCl box | revoke one device, narrow a phone's surface, no LAN cert problem, transcript binding comes with the pattern |
| D14 | agent status | **Six tiers:** hook push > ACP feed > OSC frame > process > transcript > pane text. Only tiers 0/1 open a turn or assert needs-input. Silence is `unverifiable` (we hold the pane) or `idle`, never `done`. **The store IS the `fleet_session` + `fleet_event` family**, extended with provenance, tier, three clocks, incarnation; `attention` rows are a projection written by the same single-writer apply path in the same transaction; `fleet_session.attention_state` has no other producer; `sweep_once` becomes a drift assertion that logs and never mutates. Provider order: Claude-compatible family, then OSC contract, then session-state family | pane regex primary; store per surface; a third status table | one truth across TUI, web, desktop, phone; the 732-vs-7 attention drift measured today must not get a third writer |
| D15 | renderer contract | **Plan B stays** with four invariants: frames name changed sections; one store transaction per channel drain, effects after commit; root selectors return scalars, never lists or objects (spike 4: a scalar `needsInput` count is what stops one `sessions` write from re-running 100 row effects); renderers may subscribe to a section subset (available, not required locally: 1.02x when the hot section is subscribed, 1.85x when it is not; the phone and web reuse the same filter). Lands in W0-mirror after P1. Specta on the real `AppState` sits behind one workspace feature `typescript-bindings` with a CI freshness diff, uses `specta-serde`, never derives `Type` on `AppEvent`, and picks the string route for 64-bit integers. Justified by the measured renderer fan-out; the phone never consumes sections, it consumes RPC subscriptions | plan B as written; RPC catalog; specta always-on across 15 crates | P0 is running in another session; must land before the second renderer; 456 hand-maintained specta attributes across 80 files need one gate, not fifteen |
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
- Tier-0 identity never rides on launcher environment. A provider may run its hooks from a long-lived shared daemon whose environment predates the pane (Codex 0.154 attaches the interactive TUI to `~/.codex/app-server-control/app-server-control.sock`; 0.148 still runs in-process, spike 9). The hook script reads `session_id`, `cwd`, `transcript_path` from the payload and posts to a `$HOME`-derived endpoint, so delivery survives; the store key is `SessionKey::managed(provider, session_id)`. What env loss costs is pane binding: `tmux_target` today comes only from `$TMUX_PANE` in the hook process (`ainb-core/src/cli/fleet/atc.rs:2533`), and a null there breaks send-keys answer delivery (`rpc/mod.rs:5222`) and legacy-row retirement (`fleet.rs:423`), leaving a discovered pane row and a hook row for one agent.
- Pane binding is therefore daemon-side and best-effort: use the hook's `tmux_target` when present; else correlate `(provider, cwd)` against tier-5 discovered panes running that provider in that `cwd` and bind when exactly one matches; else mark the row `pane_unbound` and surface it. Legacy-row retirement keys on the resolved binding, never only on the hook-provided target. A bound row confirms on the first later event that carries a matching `process_start_fingerprint`.
- Never add per-launch provider config overrides (Codex `-c ...`, `--enable hooks`) to force a private daemon: spike 9 measured no identity difference on 0.148, and a daemon per pane re-opens the app-server orphan class documented in `docs/solutions/no-orphaned-codex-app-servers.md`.
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
| W0-mirror | section naming + per-drain apply + scalar root selectors + subscription filter in `ainb-app`; listener census + fan-out bench; `typescript-bindings` feature across the 15 crates in the `Type` closure with `AppState.ts` regenerated and diffed in CI (same shape as the `docs/tui/cli.md` freshness gate); `attention_local_since` tuple map key replaced by a named key struct; wire/host split of the five mixed sections (`tmux`, `workspace_load`, `new_session`, `logs`, `fleet`, holding 29 of the 36 top-level skips) | P1 merged | fan-out bench at 100 sessions, 2,000 frames in 2 s: at most 12,800 computation runs, at most 32.5 ms apply per 1,000 frames, longest apply unit at most 2.4 ms, apply units exactly `ceil(burst_ms / drain_ms)` (125 at 16 ms); per-send mode must fail both numeric gates; `AppState.ts` byte-equal to the committed file |
| T0-daemon | status store on the `fleet_session` family; provenance, tier, clocks, incarnation; attention projection and receipt in the same single transaction per event (never per drain tick; spike 8: a 16 ms tick coalesces 1.08 events and adds 10 ms staleness); daemon-side pane binding by `(provider, cwd)` with `pane_unbound`; retention paths for `fleet_action_receipt` and `attention` (none exist today); rate-aware `fleet_event` payload eviction under the 1 GB ceiling; Claude-family normalizer; OSC in-band frame schema; RPC surface | spike 1 and spike 8 passed | one fixture session driven hook to store: `ainb fleet needs --format json`, `GET /api/needs`, and the TUI fleet panel `TestBackend` snapshot produce identical `(host_id, session_key, state, provenance, tier, evidence_observed_at)` tuples; table test: tier-0 `waiting` then tier-5 `idle` from one pane stays `waiting` with provenance `hook`; property test: no event sequence ending in silence yields `done`; zero rows where `attention.open` disagrees with `fleet_session.attention_state` after a 1,000-event replay fixture; the managed hook script run under `env -i HOME=<fixture>` with a Codex payload produces an attributed row when exactly one discovered pane matches `(provider, cwd)` and a `pane_unbound` row when zero or two match, with no duplicate legacy row either way |
| T0-section | `agent_status` as section 20 in `ainb-app` with its own drain budget | P1 merged, T0-daemon merged | fleet panel renders from the section with the same tuples |
| R1 | `HostId` ULID + `daemon_identity` + `SessionRef` through proto, schema, three clients; WS listener behind a transport trait; Noise IK with the carrier kind and `HostId` in the prologue (spike 3: a wrong carrier, an unpinned key, or a wrong host id each fail at message 1); single-use invite; device registry with scope + expiry; allowlist on dispatch and subscribe; close codes; pre-auth limits; terminal stream flow control with credit returned before the window drains, per-stream window 2 MiB floor, 48 KiB chunks; per-host reconnect with jittered backoff 1 s to 60 s, at most 2 concurrent resyncs per client, ordered by last-active host; coverage census `covered | omitted_by_cap | unreachable{since} | stale{head_revision, since}`; D4 host switcher with a carrier column (`tailnet | lan | ssh-l`) preferring tailnet when both reach | W0-wire, P6 (spike 3 done) | two boxes in one desktop UI; start a session, kill the client mid-turn, wait 60 s, reconnect with `after_revision`, assert `Complete` replay and the transcript advanced; two devices connected, revoke one, its socket closes with 4403 within 1 s, its next hello is refused, the other device's subscription undisturbed |
| R2 | headless VT emulator fed by tmux control mode with `refresh-client -f pause-after=2` always set (parse `%extended-output`, quote `-A "%<id>:continue"`); on `%pause` the daemon keeps serving the emulator's last screen, on `%continue` it emits `data_gap{reason: paused}` and re-snapshots from `capture-pane -e` because tmux drops the paused interval; emulator is a cache: live window 1,000 rows per session (env-tunable 100-5,000), deeper scrollback on demand from `capture-pane -e -S -<n>`; feed loss: reattach with backoff 250 ms/1 s/4 s, `data_gap{reason: feed_lost}` to every viewer, rebuild from redraw, rows `restored_unconfirmed` until next tier 0/1 event; snapshot-then-tail with monotonic seq; per-viewer batching with drain flush and `data_gap`; driver floor: generation-counted input owner, subscription-scoped resize owner, daemon-mediated viewers only | R1, spike 2 | emulator snapshot at 40x20 vs a direct PTY driving the same fixture (alt-screen TUI, OSC 8, wide chars) byte-equal after ANSI normalisation; two daemon-mediated viewers send 1,000-byte runs concurrently and the PTY receives each run contiguous, loser rejected with `floor_denied`; pane resize counter at or below 2 per subscription change over a 60 s co-view, desktop geometry restored within 250 ms of phone detach |
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
| 1 | **Done 2026-09-11** (`research/2026-09-11_multi-surface_SPIKE-1-osc-through-tmux.md`). tmux 3.4, 20 frames per form. Control mode `%output`: bare 20/20, DCS 20/20 at every `allow-passthrough` setting, envelope delivered verbatim. Rendered attach: 0/20 bare at every setting; DCS 20/20 only with `allow-passthrough on` or `all`. `capture-pane -e`: 0/20 everywhere. Nothing mangled; misses were absence. `-f ignore-size` did not prevent a 120x40 to 80x23 resize; `window-size manual` plus `resize-window` did | done | T0-daemon | control mode is the tier-2 feed; the emitter sends both forms (~60 extra bytes, no tmux detection); no `allow-passthrough` requirement on the daemon; no polling fallback on `capture-pane` |
| 2 | Control-mode feed only: `tmux -C attach` on a pipe with `refresh-client -A %<pane>:on` and `-f pause-after=2`. Drive alt-screen, mouse tracking, wide chars, OSC 8, a custom OSC status frame; feed a Rust VT emulator; diff snapshots against a direct PTY; measure RSS per emulator at 100 sessions and CPU during `cat` of 50 MB; confirm `#{window_width}` seen by a second attached client never changes | 2-3 days | R2 | poor fidelity reopens daemon-owned PTY as a third execution mode |
| 3 | **Done 2026-09-12** (`research/2026-09-11_multi-surface_SPIKE-3-peer-ws-noise.md`, two real boxes over tailnet and `ssh -L`, real daemon 1.28.1 behind a Noise IK proxy). Handshake p50 44 ms tailnet, 48 ms `ssh -L` (one RTT for Noise; 1.3 ms loopback). SIGKILL mid-subscription, 60 s dead, resync in 124-128 ms with 601 events replayed contiguous to head. 50 MiB: fails the 2 s gate at the 512 KiB per-stream window on both carriers (2.99 s tailnet), passes at 2 MiB (0.85-0.89 s tailnet, 1.0-1.6 s `ssh -L`); throughput is linear in window and equals `window / (2 x RTT)` because credit returns only after the window drains. Noise costs 1-5% of stream time. Inside `ssh -L` a saturated stream raises control-plane p50 from 16 ms to 48-98 ms regardless of WS count; only a second ssh process restores it. Storm: 50 clients, 200 connections, 600 resyncs, zero failures, 7.8 MiB peak RSS per proxy | done | R1 | go for R1; per-stream window floor becomes 2 MiB or credit refills before drain (the better fix); carrier kind and `HostId` bound in the Noise prologue with three negative cases failing closed; `ssh -L` is a degraded carrier and the host switcher shows the carrier; a second unencrypted WS inside `ssh -L` is rejected as a fix |
| 4 | **Done 2026-09-11** (`research/2026-09-11_multi-surface_SPIKE-4-specta-fanout.md`). Solid store, 100 sessions, 2,000 frames in 2 s, headless Chromium: per-send 110.4 ms apply and 18,616 computation runs; per-drain 32.5 ms and 9,644 (3.40x, 1.93x); the win concentrates in root selectors (5.6x) because hot traffic fans across 100 rows; subscription filter 1.02x with the hot section in, 1.85x with it out; heap flat across 20 bursts. Specta on the real `AppState`: 250 types, 163.5 KiB TS, cold build +15.3 s (+8.3%), edit cycle +0.9 s; 80 files, 15 crates, 239 derives, 64 skips, 153 bigint remaps; 3 type-name collisions; one field cannot export (tuple map key); 8 sections clean, 5 near-clean, 5 mixed, 1 empty | done | W0-mirror | batching holds; subscription stays available, not required; specta goes behind a feature flag for blast radius, not compile time; scalar root selectors become a D15 invariant |
| 5 | Noise IK handshake and AEAD framing in the Expo app through a uniffi-compiled Rust crate on a real iPhone and a real Android device; cold-start cost, bundle size, keychain custody of the device static key | 2 days | M1, `ainb-wire-mobile` | if uniffi is unworkable, D13 needs a JS Noise implementation review before M1, or the phone speaks TLS 1.3 with a pinned host cert as a second transport kind |
| 6 | iOS and Android background socket lifetime with a 15 s heartbeat: how long a live socket survives suspension; whether a locally scheduled needs-input banner fires after 1, 5, 30 minutes | 1 day | M1 gate wording | if grace is under 60 s, the push reopen row moves to M1+1 |
| 7 | **Done 2026-09-11** (`research/2026-09-11_multi-surface_SPIKE-7-control-mode-flow.md`). tmux 3.4, slow reader, `pause-after=2`. Pane A paused 2.4 s to 8.2 s while pane B heartbeats kept a 1.00 s cadence through the pause (buffering age 1880 ms vs 56 ms). Resume SKIPPED the paused interval: 17.18 MB dropped at exactly one boundary; the pane itself stayed live at 2.99 MB/s. Server RSS 4.5 MB baseline, 18.4 MB max; reader 5.2 MB. Without `pause-after` the stream was lossless but tmux backpressured the PTY and throttled the producer from 2.8 to 0.25 MB/s. `pause-after` switches the client to `%extended-output`. `refresh-client -A %0:continue` is a parse error; the argument must be quoted | done | R2 | per-pane pause is the per-viewer flow-control primitive; the daemon always sets `pause-after`, never `off`, and on `%continue` emits `data_gap{reason: paused}` then re-snapshots |
| 8 | **Done 2026-09-11** (`research/2026-09-11_multi-surface_SPIKE-8-sqlite-write-path.md`). Real `ainb-hangar-store` crate, real PRAGMAs (WAL, `synchronous=NORMAL`, `busy_timeout` 10 s), 100 sessions. One transaction per event at 10/s for 20 min: p99 11.21 ms; at 100/s for 5 min: p99 11.14 ms; zero `SQLITE_BUSY`. Per 16 ms drain tick: coalesced 1.08 events on average and added ~10 ms mean staleness. WAL flat at 4.1 MB, held by the default `wal_autocheckpoint`, not by the retention job. Main file grew 16.5 MB/h at a 30 s retention window, all of it `fleet_action_receipt` (11,898 rows) and `attention` (1,766 rows), which nothing deletes. Today's daemon commits three transactions per event (`repo/fleet.rs:495`, `attention_ingest.rs:610`, `rpc/mod.rs:2475`), so the single transaction is cheaper than what ships | done | T0-daemon, W0-wire | one transaction per event, never per drain tick, in the daemon; the 50 ms gate has 4x headroom; receipts and attention need a retention path before T0 ships; `fleet_event` payloads at 8.3 KB mean and 10/s sustained reach the daemon's 1 GB payload ceiling in about 3.6 h under the 48 h eviction window, so payload eviction becomes rate-aware; run F confirmed today's three-commit shape is slower than the single transaction (commit-only p50 0.40 ms vs 0.14 ms); 36% of the file is freelist after deletes, so the file plateaus at its high-water mark and never shrinks without `VACUUM` |
| 9 | **Done 2026-09-11** (`research/2026-09-11_multi-surface_SPIKE-9-codex-daemon-reuse.md`). Codex 0.148.0 on this box runs the interactive TUI in-process (no connection to either pre-existing app-server socket), so the reuse path does not exist on this version; plain launch, `-c model=...` launch, and `codex exec` all delivered hook events with full identity. The ainb-owned app-server path shows the degraded shape: 1,215 of 1,215 sampled hook lines carry null `tmux_target`, correctly, since those sessions have no pane. `tmux_target` derives from `$TMUX_PANE` in the hook process only, `atc.rs:2533`, no fallback | done | T0-daemon | identity never rides on launcher env (already true, now a rule with an `env -i` test); pane binding moves daemon-side via `(provider, cwd)` correlation; no per-launch config overrides. Codex 0.154 (macOS, daemon reuse confirmed by Stevie) is the version that makes this live for tmux-launched sessions |

Spikes 1, 3, 4, 7, 8 and 9 done. Remaining: 2 (control-mode emulator fidelity, before R2), 5 and 6 (phone crypto via uniffi, background socket lifetime, before M1). None blocks P0+S. Spike 4 caveat: batching gets relatively less effective per frame as sessions grow, because row collisions inside a drain get rarer; re-measure the ceiling if the target moves past 100 sessions.

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
- Off-box leg: Noise IK handshake, host static key pinned in the pairing offer, prologue binds carrier kind and `HostId` (measured: 1 RTT of handshake cost, 1-5% of stream time); single-use invite; per-device token bound to the redeeming static key with `scope` and expiry; revocation terminates open sockets with 4403 immediately; allowlist checked before dispatch and on subscribe, not per handler.
- The daemon's tmux client never influences pane size: control mode only. If any daemon path ever attaches a rendered client (`ainb-web` PtyBridge today at 80x24), it must set `window-size manual` and `resize-window` to the intended geometry first; `-f ignore-size` does not hold on 3.4 (spike 1).
- Never log tokens, invite secrets, pairing offers, Noise keys, or terminal bytes; log digests only.
- Relay reopen path: relay sees ciphertext only; host proof over a bound transcript; external identity provider issues the host-control token.
- Never fall back to local execution when a remote host is missing.
- Contact loss is `unverifiable` or `stale`, never `exited`; only positive proof of exit authorises stop or retry.

## Operability

- `hangar/health` gains counters: connections by scope and transport; resyncs and `SnapshotReset` by reason; `data_gap` by reason; ops by disposition; receipts in `unknown`; tier-0 silence seconds per session (p50, max); emulator RSS total; `status_unknown_event{provider,name}`.
- Retention from day one: ledger (`fleet_action_receipt`) 7 d; `attention` rows 30 d after close; status events 30 d or the existing `fleet_retention` byte ceiling with payload eviction made rate-aware (spike 8: 10/s at the 8.3 KB corpus mean fills the 1 GB payload ceiling within hours, well inside the 48 h eviction window); device registry unbounded with `last_seen_at` shown; revoked rows kept 90 d for audit. Spike 8 measured `fleet_action_receipt` and `attention` as the only growing tables; neither had a delete path.
- SQLite settings stay as shipped: WAL, `synchronous=NORMAL`, `busy_timeout` 10 s, no `auto_vacuum`; after a large retention drain run one `VACUUM INTO` a sibling file and swap, never a periodic `VACUUM`. Under NORMAL the D18 receipt written in the apply transaction survives a daemon crash but not a power loss; a receipt found in `writing` after power loss is handled by the same `unknown{effects_ambiguous}` path, so no setting change is needed.
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
| one pane floods while a viewer reads slowly | tmux pauses that pane's stream only (spike 7); idle panes keep their cadence; the agent in the flooding pane is never throttled; on resume the viewer gets `data_gap{reason: paused}` and a fresh snapshot |
| daemon forgets `pause-after` | tmux backpressures the PTY and throttles the agent to a fraction of its output rate (2.8 to 0.25 MB/s measured); a boot-time assertion fails if the control client is not in pause mode |
| device revoked while its box is offline | revocation is a box-local write; the token is unusable until the box is up, then rejected at hello; expiry is the backstop; `admin` scope on any paired device may revoke |
| box reinstalled with the same hostname | new `host_id` and static key; old pairing shows "identity changed, re-pair"; old rows stay under the old host until removed |
| host removed in the desktop with live sessions | desktop forgets the pairing and asks the box to revoke its own device; the box and its agents are untouched |
| clock skew between phone and box | pairing TTL uses 30 s leeway; op ids carry no timestamp; status decay uses `mirrored_received_at` |
| desktop sidecar daemon older than the installed `ainb` | flock winner serves; loser's clients negotiate down; a client that cannot negotiate shows "daemon protocol too old, restart from the newer binary" and never spawns a second daemon |
| OSC frame emitted by a CLI under tmux | control mode delivers it verbatim (spike 1); an operator watching the same pane in a rendered client sees it only with `allow-passthrough on`, which the daemon does not require; hooks remain authoritative regardless |
| provider CLI renames a hook event | `status_unknown_event` counter climbs, `ainb doctor` names the provider; rollback flag restores `classify()`-first for one release |
| provider runs hooks from a shared daemon with no pane env | events still land with payload identity; daemon binds the pane by `(provider, cwd)` when unique, else `pane_unbound`; the fleet panel shows one row, not two; answering a `pane_unbound` ASK offers the structured path (ACP or app-server) or names the pane it could not find |
| two sessions of one provider in one cwd, both env-less | both rows `pane_unbound`; `ainb doctor` names the cwd; operator disambiguates by attaching once from the TUI, which stamps the binding |

## Do not build

| Not building | Reopen when |
|---|---|
| cloud relay | a client cannot dial the box; then single cell behind managed identity and managed SQL |
| daemon-owned PTY replacing tmux | spike 2 fails on the control-mode feed, or native Windows enters scope |
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
