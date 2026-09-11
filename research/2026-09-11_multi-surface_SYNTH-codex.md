# Synthesis: desktop/multi-surface architecture decisions

Peer read of A-process-model, B-remote-federation, C-mobile, D-agent-status, E-state-sync,
F-ours-current, against the locked handover and the 2026-09-04 spec. "the reference
implementation" = the analyzed prior art; never named otherwise. IDs `R1..R9` are new,
distinct from spec phases (P0-P6, S, D1-D4) and from the handover's locked D1-D9.

## 0. Shape

```
┌─ LOCKED (keep) ──────────────┐   ┌─ THIS SYNTH reopens ─────────┐
│ tmux hybrid, whole-mirror,   │   │ remote host model            │
│ Versioned<Section>, keymap,  │──▶│ (client-driven ssh-L owner   │
│ answer exactly-once          │   │  → daemon-as-peer)           │
└───────────────────────────────┘   └───────────────────────────────┘
        │                                        │
        ▼                                        ▼
┌─ NEW: Phase W (wire) ────────┐   ┌─ NEW: Phase H (host federation)─┐
│ host identity, op-id         │──▶│ peer WS + E2EE + per-device     │
│ envelope, capability strings │   │ tokens; ssh -L becomes a        │
│ numeric protocol version     │   │ transport carrier, not the model│
└───────────────────────────────┘   └───────────────────────┬───────┘
┌─ NEW: Phase T (status) ──────┐                            ▼
│ daemon-owned tier hierarchy  │                  ┌─ NEW: Phase M (mobile)─┐
│ hooks>structured>OSC>proc    │                  │ Expo/RN, v2, after H    │
└───────────────────────────────┘                  └─────────────────────────┘
```

## 1. Decisions

### R1 — tmux: keep, replace, or hybrid
**Recommend: hybrid, keep tmux.** tmux gives free N-viewer attach, survives our daemon's own
restart, unbounded scrollback we don't maintain, an operator escape hatch, zero socket-ownership
protocol to build [fact, A§7a]. What tmux lacks: a snapshot primitive for a cold-attaching
mobile/remote client, per-viewer backpressure, per-viewer resize, input arbitration between two
typists — the reference implementation solves all four above its own PTY, not by replacing tmux
[fact, A§7d]. Add, in value order: (1) OSC+hook agent-status ingestion (Tier 0/2, independent of
this decision, do regardless [inference, A§7c]); (2) versioned wire with explicit unknown-opcode
handling, cheapest now, most expensive after clients ship [A§7c]; (3) a headless-emulator snapshot
in the daemon fed by the tmux-attached stream, for a cold first frame [A§2.2, A§7d.1]; (4) a
driver floor / resize-owner above the bridge, only once two surfaces must co-type [A§3.2].
- **Alt A: full replace with own node-pty-style daemon+emulator.** Reference implementation's own
  model; gains snapshot/backpressure/resize natively, costs ~8 daemon modules
  (checkpoint log, torn-tail detection, recovery-freeze, cold-restore budget) [A§2.3-2.5] and
  loses the free N-viewer attach and the operator escape hatch. High cost, not chosen.
- **Alt B: tmux local-only, separate headless path for remote/mobile.** Two PTY backends to
  maintain; rejected as duplicated-implementation cost [A§7d.8, inference].
- **Cost:** ADD2 ~1-2wk, ADD4 ~3-5d, ADD1 ~2-3wk, ADD3 only once needed.
- **Blocked on:** spike — does OSC 133/status-OSC survive `tmux` + `allow-passthrough`? [A§7c,
  named as "a concrete, testable blocker, not a theoretical one"].
- **Spec change:** none to locked tmux-primary decision; adds Phase T (status) and a snapshot
  requirement to whatever phase first serves a cold remote/mobile attach.

### R2 — host model: peer daemon vs client-driven ssh -L
**Recommend: reopen the locked line "remote = ssh-forwarded Unix socket owned by the desktop."**
Make the remote box run the same daemon binary in a peer role, with its own control plane; `ssh
-L` becomes one transport *carrier* for reaching it, not the ownership model [B§8.6, fact:
`docs/reference/ssh-execution-boundary.md:102` — "for work that must continue while you are
offline, use the peer/headless-runtime model... instead of the direct-SSH model"]. Hard evidence
to reopen: the reference implementation's own SSH-model equivalent has a named failure — socket
path namespaced by build hash means every client update permanently orphans live remote work
(`running, unreachable, never exited`) [B§1.5, fact, `ssh-execution-boundary.md:43-49`]. A
client-owned control plane also means nothing on the box runs while the laptop is closed,
contradicting the spec's own "Operator away from box" persona. The reference already models
our planned carrier as a legitimate leg: `connectionDependency: 'ssh-tunnel'` on a peer
[B§8.6, fact, `runtime-environments.ts:33`].
- **Alt A: keep locked plan as-is** (client-driven, desktop owns the forwarded socket). Cheapest
  today, zero new daemon work. Breaks "runs while laptop closed"; breaks on every app update per
  the version-lock failure mode above.
- **Alt B: full peer + relay from day one.** Overkill; no target user lacks tailnet reach today.
- **Cost:** ~5,300 Rust LOC / 6-9 engineer-weeks for the full federation layer (host identity,
  wire crate, peer transport, pairing+E2EE, flow-controlled streaming, session authority, UI
  aggregation) [B§8.4, inference scaled from TS LOC]. Much of this is shared with R4 and R9.
- **Blocked on:** `ainb-hangar-client` hardcodes `UnixStream` [F: Redesign inputs, "Reusable with
  change"]; no host/machine identity anywhere in the wire schema today [F§0, fact].
- **Spec change:** rewrites "Hosts + daemon" table's remote row and D4's ssh-forward scope.

### R3 — relay: yes/no
**Recommend: no relay.** Tailnet + peer WebSocket covers laptop-to-box; the reference implementation's
own relay is scoped to phone-cannot-dial-desktop only, never desktop-to-peer federation
[B§8.2, fact: `cloud/README.md:3-7`, a `relay` block on a `runtime`-scope pairing offer is a hard
validation error]. Reopen trigger: a client that genuinely cannot dial the box (mobile off
tailnet, on cellular, no VPN profile) [B§8.3]. Minimum single-cell relay if ever needed:
~2,000 Rust LOC / 2-3wk; full multi-cell relay (what the reference actually ships) is 18.5k LOC
and should never be the starting design point [B§8.5].

### R4 — off-box transport + auth
**Recommend: WebSocket, app-level E2EE (X25519/NaCl or Noise `IK`), per-device revocable tokens
with a scope field, replacing bare `SO_PEERCRED`.** `SO_PEERCRED` has no off-box equivalent and
is today's only trust boundary [F§0, fact]. Per-device tokens (not one shared daemon token) are
what make a phone shippable later without rewrite — you cannot revoke one device or give it a
smaller surface under a shared secret [C§A.2]. E2EE at the app layer removes the "TLS cert for a
laptop on a LAN" problem and lets a relay exist later without trusting it [C§A.3]. Reuse, don't
rebuild: `ainb-hangar-proto`'s 159-method envelope + semver gate and `ainb-hangar-store`'s
revision-cursor `fleet/subscribe{after_revision}` + `SnapshotReset` are already the hard 80% and
are "reusable as-is" [F: Redesign inputs].
- **Alt A: shared secret only** (today's token file). Cheapest, but unrevocable per-device,
  blocks R7 outright [C§A.2].
- **Alt B: mTLS.** Heavier ops, consumer-mobile cert distribution problem, no upside over
  app-level E2EE for our trust model.
- **Cost:** folded into R2's 5,300 LOC estimate (~800 LOC for pairing+E2EE alone [B§8.4 item 4]).
- **Spec change:** extends "Hosts + daemon" auth row from token+`SO_PEERCRED` to token+E2EE for
  the remote leg only; local leg unchanged.

### R5 — status signal hierarchy + store location
**Recommend: adopt the six-tier hierarchy verbatim, daemon owns one store per host, every surface
mirrors it.** `hook/plugin push > structured feed > OSC frame > process evidence > transcript >
pane text`, with the two hard rules: only tiers 0/1 may open a turn or assert needs-input, and
silence never becomes `done` [D§7.1, inference from `agent-status-observation.ts` ordering].
Not a rewrite for us — our existing hook pipeline (30 events, `events.jsonl`, byte-cursor ingest)
is already "the authoritative status tier... already the best signal source, reusable as-is" [F:
Redesign inputs], and `classify()` (tmux pane-text) already exists as the Tier-5 fallback. Missing
piece: a single daemon-owned store with write-time precedence and provenance stamped per row, so
readers never re-adjudicate [D§7.3, fact: "The execution host owns agent status, in one store,
and every reader subscribes to it."]. The reference implementation's counter-example is the cost
of skipping this: three copies of the same row inside one process, keyed differently — "the same
pane can legitimately read differently on the desktop, on the phone, and in the CLI" [D§6 Avoid #7].
- **Alt A: keep tmux capture-pane regex as primary today.** Cheap, but tier-5-as-default is
  exactly the inversion the reference calls out as costly [D§7.1].
- **Alt B: each surface re-derives status independently.** Rejected — the three-copies failure
  mode above.
- **Cost:** mostly consolidation of what exists; new work is the store's single-writer apply path
  and provenance/freshness fields (`agentWait`-style three-state: present-with-evidence /
  present-null / absent, never collapse absent into null [A§4.2, fact]).
- **Spec change:** none to locked decisions; fills the "board" / attention section's data source.

### R6 — renderer contract: whole-mirror vs section subscription
**Recommend: keep the locked whole-mirror (decision B), but make two things day-one invariants,
not later optimizations.** (1) A channel frame names which sections changed; the renderer applies
**one** store transaction per drain, effects strictly after commit. (2) Add a per-section
subscription filter as the remote/mobile escape hatch — a phone declares which of the 19 sections
it wants, never all 19 for one agent's status [E: Decision Q1]. Evidence for urgency: the
reference implementation's flat mirror measured 9,279 listeners, ~160 publications/s, ~11.6
renderer-CPU points from fan-out alone, with **zero** long tasks to warn a naive jank check [E§Q1,
fact, `renderer-agent-status-performance.md:237-267`]. Their after-the-fact fix (batch to one
transaction/burst) got 19.6× throughput; our lever (19 sections + section-scoped apply) gets that
for free if built in day one [E§Q1, inference].
- **Alt A: whole-mirror, no section filter, ever.** Fine for desktop-only; breaks the moment a
  phone or a remote host is a consumer [E§Q1: "our 19 sections give us a lever the reference
  implementation did not have... mobile is the sharp case"].
- **Alt B: full RPC-catalog model (per-reference).** Wins on selectivity, loses on drift-control —
  610 methods needed a generator, an identity-matching bundle, and a byte-diff CI gate, and still
  drifted (3 methods outside the shared layer, a client that ignores generated types) [E§Q1].
- **Cost:** low — a listener census + fan-out benchmark with a CI ceiling, ~50 LOC pattern to
  copy [E: Adopt #5].
- **Spec change:** none to the locked renderer-contract choice; adds a subscription filter
  requirement, due before whichever of D4 (host switcher) or Phase M (mobile) ships first.

### R7 — mobile stack
**Recommend: Expo (React Native), terminal as xterm-in-webview.** One codebase for iOS+Android;
`expo-secure-store`/`camera`/`notifications`/`crypto` cover keychain, QR pairing, banners, and a
real CSPRNG out of the box; the reference implementation proves the design at 132k LOC on this
stack [C§C, inference]. We lose the reference's biggest structural win — 604 shared-module TS
import sites between desktop and phone — replaced with Rust-generated TS types via specta, never
hand-shared. iOS release needs a hosted Mac runner with current Xcode, pinned and moving every SDK
bump [C§C, fact].
- **Alt A: Tauri mobile.** Reuses SolidJS components, but phone UX isn't desktop UX so the
  reusable fraction is small, and the mobile plugin ecosystem lacks Expo's maturity for
  notifications/secure-store/camera — we'd write those bindings ourselves [C§C].
- **Alt B: native Swift+Kotlin,** sharing models with the existing `ainb-fleet-macos` Swift app.
  Best platform fit but two UIs to maintain for a 132k-LOC-equivalent surface; the model-sharing
  argument inverts the moment Android ships [C§C].
- **Cost:** v1 minimal feature set is 8 items (pair, session list, transcript+reply, answer
  prompts, cancel, read-only terminal attach+snapshot, notifications, connection log) [C§B].
- **Spec change:** none — mobile is already out of v1 scope; this sequences it for v2/Phase M.

### R8 — wire versioning / capabilities
**Recommend: numeric protocol version + additive capability strings + explicit unknown-opcode
handling + generate-then-byte-diff CI gate**, extending the pattern the spec already plans for
`tauri-specta`/`tsc` to also cover the off-box wire. `ainb-hangar-proto` already has a 159-method
envelope with a semver gate — "reusable as-is" [F: Redesign inputs] — this is "extend it," not
"build it." Copy verbatim: a new binary opcode is not safe without negotiation; an unknown opcode
is silently dropped and "the feature behind it appears to hang" [B§8.7; E: Adopt #4]. Bump only
on removing a method/param, changing field meaning, or changing encrypted/framing/auth semantics —
never for new methods or optional fields [B§1.2, fact].
- **Alt A: no explicit versioning, evolve informally.** Fails the first time desktop and a
  peer/mobile update independently, which "is the normal state, not an edge case" [B§1.7, fact].
- **Cost:** low — mostly process + a CI gate, not new runtime code.
- **Spec change:** adds a CI step to whichever phase first ships an off-box wire (Phase W).

### R9 — idempotent mutation envelope
**Recommend: client-minted operation id + host commit-then-reply-with-replay, closed disposition
vocabulary (`created` / `adopted` / `replayed`), applied to every mutation before any remote or
mobile surface ships, not just attention answers.** We already have the hard 20% built: `answer.rs`
exactly-once with C1 misroute guards is "the hard-won correctness... keep verbatim" [F: Redesign
inputs]. Today it's scoped to attention answers only; a phone on cellular will lose replies to
*any* mutation, and without this, "approve permission" or "send follow-up prompt" double-fire —
on an about-to-run-a-command agent that's a correctness bug, not a UX bug [C§A.5].
- **Alt A: retrofit later, per-endpoint, as remote/mobile features land.** Named explicitly as the
  expensive path — "the expensive retrofit" [C§A.5].
- **Cost:** ~700 LOC in the reference implementation's equivalent module [B§8.4 item 6]; ours is smaller
  since the pattern already exists for answers.
- **Spec change:** generalizes the existing answer-RPC pattern into the mutation envelope for
  Phase W; no change to locked "reject-second on answers" behavior, which becomes the first
  instance of the general rule rather than a special case.

## 2. Sequencing

```
P0+S (locked, running) ──▶ P1 ──▶ P2 ──▶ P3 ──▶ P4 ──▶ P5 ──▶ P6
      │                                                          │
      ├─ Phase W (R4,R8,R9: identity, wire crate, op-id envelope)│  parallel,
      │      daemon-side, no ainb-app dependency                 │  gate: extend
      │                                                          │  159-method
      ├─ Phase T (R5: daemon status store, tier hierarchy)       │  semver gate
      │      parallel with P0-P3, daemon+event-pipeline only     │
      │                                                          ▼
      │                                    D1 ─▶ D2 ─▶ D3 ─▶ D4 (host switcher)
      │                                                     ▲
      │                                          gate: R6 section-subscription
      │                                          filter must land before D4
      │                                          OR before Phase M, whichever first
      ▼
Phase H (R2,R3: peer WS, replaces ssh-L-only)
   gate: after P6 (client transport abstraction) + Phase W; spike #2 below
      │
      ▼
Phase M (R7: mobile, Expo) — v2, after Phase H; gate: spike-verified E2EE+idempotency
```

**Paint-into-corner flags:**
- D4 as currently scoped ("host switcher + ssh forward") assumes the locked client-driven ssh -L
  model. If R2 is accepted, D4's remote-host code must target peer WebSocket + tailnet first,
  ssh -L retained only as a transport carrier — rewrite D4's scope before it starts, not after.
- R6's section-subscription filter is cheap now, expensive once D4 (first multi-host UI) or
  Phase M ships without it — same shape as the reference implementation's own "add capability
  field from day one, don't discover you need an HTTP header later" lesson [B: Adapt table].

## 3. Do-not-build (with reopen triggers)

| Item | Reopen when |
|---|---|
| Cloud relay (director/cells/splice/migration/fencing) | a client cannot dial the box at all — no tailnet, no LAN [R3] |
| Push gateway (APNs/FCM server) | mobile ships and background banners are required (poll/foreground first) |
| Regional multi-cell placement | never, unless relay is built AND has >1 cell |
| Cross-host orchestration federation (dispatch while home client offline) | multi-box dispatch while operator offline becomes a product requirement |
| Full tmux replacement (own multiplexer) | native Windows daemon required, or the OSC-passthrough spike fails |
| Native Swift+Kotlin mobile | decide iOS-only forever (contradicts stated Android intent) |
| Full driver-floor input arbitration (ADD3, R1) | two surfaces must literally co-type into one PTY concurrently |
| Ticket import as session creation | unchanged, already out of scope in spec |

## 4. Contradictions resolved

1. **Renderer contract.** Not a contradiction — E's Q1 refines, doesn't reopen, the locked
   whole-mirror choice: keep it, but make section-scoped once-per-drain apply and a
   subscription filter day-one invariants rather than optional (R6).
2. **Host model.** Real contradiction: locked spec says client-owned ssh-forwarded socket; B's
   evidence (version-lock orphaning, "must continue while offline") argues the opposite. Resolved
   by reopening per R2, with named hard evidence, not by ignoring the lock.
3. **tmux framing.** A frames tmux-keep as a clean win (free N-viewer). F (our own code) sharpens
   this: only 2 of 6 providers have a non-tmux answer path (ACP) today, so remote answering stays
   blocked regardless of R1 until ACP coverage widens — a precondition for Phase H, not a
   contradiction of R1.

## 5. Top 3 spikes

| # | Spike | What result flips |
|---|---|---|
| 1 | OSC 133 / status-OSC through `tmux` with `set -g allow-passthrough` | R1's ADD2 scope: if passthrough survives, hooks+OSC stay tmux-agnostic as assumed; if not, some providers need a non-tmux ingestion path before Phase T lands for them |
| 2 | Peer WebSocket + E2EE prototype over direct tailnet and over `ssh -L`, measuring reconnect/resync latency, confirming the daemon-as-peer keeps working after the desktop closes | R2/Phase H go/no-go — the concrete evidence that justifies spending the reopened-decision engineering weeks before committing D4's scope |
| 3 | Whole-mirror fan-out cost on our real 19-section `AppState` under TUI+desktop+web concurrently, with and without once-per-drain batching | Whether R6's section-subscription filter is needed before D4 (as flagged) or can wait for Phase M — and whether once-per-drain batching alone is sufficient without it |
