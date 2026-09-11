Mode: focused-query

# Mobile Companion — Reference Implementation Analysis

**Repo** the reference implementation (local clone), analysed read-only
**Commit** `9aa0f7e77d366c23a3cc8de2da32ae550d397dc0` (`9aa0f7e7`, 2026-09-11, committed by a CI bot)
**Research query** How is the mobile companion built and connected, and what would it take to ship an equivalent?
**Analysed** 2026-09-11
**Naming** the product is never named in prose; file paths are relative to the clone root.

Every substantive claim below is tagged `[fact]` (read from the cited file) or `[inference]`.

---

## 0. Answer in one screen

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                          PHONE  (Expo / React Native)                        │
│  expo-router screens ──▶ StableLogicalRpcClient ──▶ one of two transports    │
└───────────────┬──────────────────────────────────────────────┬───────────────┘
                │ direct                                        │ relay
                │ ws(s)://<lan|tailscale>:6768                  │ wss://<cell>/v1/connect/<relayHostId>
                ▼                                               ▼
       ┌──────────────────┐                          ┌────────────────────────┐
       │ DESKTOP app      │                          │ CLOUD RELAY (cells +   │
       │ ws-transport.ts  │◀────── outbound ws ──────│ director), splices two │
       │ + E2EE channel   │                          │ opaque E2EE streams    │
       │ + device registry│                          └────────────────────────┘
       │ + method allowlist│
       └────────┬─────────┘
                │ HTTPS, host-keypair auth
                ▼
       ┌──────────────────┐        APNs / FCM        ┌──────────────────┐
       │ PUSH GATEWAY     │─────────────────────────▶│ phone OS banner  │
       │ cloud/apps/push  │                          └──────────────────┘
       └──────────────────┘
```

Four load-bearing facts:

1. **The phone is a thin RPC client over one multiplexed, end-to-end-encrypted WebSocket.** There is no phone-side business logic about agents; every read and every action is an allowlisted RPC against the desktop runtime. `mobile/src/transport/rpc-client.ts:24-47`, `src/main/runtime/runtime-rpc/runtime-rpc-mobile-method-allowlist.ts:1-291` `[fact]`
2. **Encryption is application-level, not TLS.** X25519 + XSalsa20-Poly1305 inside the WebSocket, with the desktop public key pinned by the pairing QR. TLS is optional and the relay is treated as untrusted plumbing. `mobile/src/transport/e2ee.ts:1-101`, `src/main/runtime/rpc/ws-transport.ts:1` `[fact]`
3. **Push notifications are half-built in this snapshot.** The cloud gateway and the desktop half are complete and well-tested; the phone never registers a native APNs/FCM token. Phone banners today come from local notifications scheduled off the live socket. `[fact]` — see §5.
4. **Mobile and desktop share TypeScript by relative import, not by package**: 604 import sites across 158 modules under `src/shared/`. That is the single biggest reason this architecture would not transfer unchanged to a Rust core. `[fact]` — see §6.

---

## 1. Stack

### 1.1 Frameworks and versions

```
┌─────────────┐  ┌──────────────┐  ┌───────────────┐  ┌──────────────┐
│ Expo SDK 55 │──│ RN 0.83      │──│ React 19.2    │──│ Hermes + new │
│ expo-router │  │ new arch on  │  │               │  │ architecture │
└─────────────┘  └──────────────┘  └───────────────┘  └──────────────┘
```

| Layer | Detected | Evidence | Confidence |
|---|---|---|---|
| Runtime | Expo SDK `^55.0.30`, React Native `^0.83.10`, React `^19.2.8` | `mobile/package.json:29,63,65` | High `[fact]` |
| Architecture | New architecture enabled (`newArchEnabled: true`) | `mobile/app.json:11` | High `[fact]` |
| Navigation | `expo-router ^55.0.18`, file-based, `main: "expo-router/entry"` | `mobile/package.json:5,52`; routes in `mobile/app/` | High `[fact]` |
| State | React hooks + one context per host + module-level caches. `zustand ^5.0.13` is declared but **not imported anywhere** under `mobile/src` or `mobile/app` | `mobile/package.json:70`; grep of `zustand` in `mobile/src`+`mobile/app` returns nothing | High `[fact]` |
| Shared client | `RpcClientProvider` holds one `RpcClient` per host in a ref, with per-host listener sets so state changes do not re-render the tree | `mobile/src/transport/client-context.tsx:50-93` | High `[fact]` |
| Terminal | `@xterm/xterm 6.1.0-beta.303` + webgl + unicode11 addons, inside `react-native-webview 13.16.2` | `mobile/package.json:8-11,61` | High `[fact]` |
| Crypto | `tweetnacl ^1.0.3` + `expo-crypto` for the RNG, `@noble/hashes 1.8.0` | `mobile/package.json:14,40,68`; `mobile/src/transport/e2ee.ts:5-14` | High `[fact]` |
| Validation | `zod ~4.4.3` for every wire shape the phone parses | `mobile/package.json:69`; `mobile/src/transport/types.ts:113-137` | High `[fact]` |
| Icons / UI | `lucide-react-native`, `react-native-svg`, `react-native-reanimated 4.3.4`, `react-native-gesture-handler`, `mermaid 11.17.2` in a second webview | `mobile/package.json:42-60` | High `[fact]` |
| Tooling | TypeScript `6.0.3`, `vitest ^4.1.11` (+ `happy-dom`), `oxlint`/`oxfmt`, `esbuild 0.25.4` for the webview bundles | `mobile/package.json:73-90` | High `[fact]` |
| Audio | Local private native module `expo-two-way-audio` under `mobile/packages/` (iOS + Android dirs) for dictation capture | `mobile/package.json:30`; `mobile/packages/expo-two-way-audio/` | High `[fact]` |

Two generated artefacts are built on `postinstall`, not committed as source: the xterm engine and the mermaid engine are bundled by esbuild into `.generated.ts` files. `mobile/package.json:10`, `mobile/scripts/build-terminal-webview-engine.mjs:1-20` `[fact]`

### 1.2 Size of the codebase

| Scope | Files | Lines |
|---|---|---|
| `mobile/src` + `mobile/app`, all `.ts`/`.tsx` | 1,556 | 220,990 |
| non-test only | 1,028 | 132,102 |
| tests only | 528 | 88,888 |

Test-to-source ratio is ~0.67 by line count `[fact]`. That is unusually high and it is behavioural: the suites drive reconnect races, watermark seeding races, catch-up quarantine, and delivery ordering rather than pinning internal wiring (e.g. `mobile/src/notifications/notification-watermark-seed-race.test.ts`, `mobile/src/notifications/notification-delivery-ordering.test.ts`) `[fact]`.

Non-test lines by feature directory:

| Directory | Lines | Files | What it is |
|---|---|---|---|
| `session/` | 35,056 | 408 | Agent session surface: transcript, composer, prompts, diff review, terminal tabs |
| `tasks/` | 23,443 | 167 | Issue-tracker + PR hub (GitHub, GitLab, Linear) and workspace creation |
| `components/` | 20,150 | 207 | Shared UI |
| `transport/` | 13,577 | 234 | Everything in §2 |
| `terminal/` | 8,115 | 133 | xterm webview host, input, accessory keys |
| `source-control/` | 7,421 | 84 | Status, diff, commit, push, history, PR segment |
| `browser/` | 3,072 | 40 | Remote browser screencast + input |
| `files/` | 2,790 | 40 | File explorer and preview |
| `host-screen/` | 2,267 | 14 | Per-host worktree list |
| `worktree/` | 2,250 | 53 | Worktree models |
| `agent-history/` | 1,741 | 17 | Past agent sessions browser |
| `home/` | 1,584 | 16 | Host list home screen |
| `diagnostics/` | 1,545 | 16 | Connection log, troubleshooting |
| `settings/` | 1,497 | 16 | Settings screens |
| `notifications/` | 1,089 | 15 | Local notification scheduling + catch-up |
| `hooks/`, `storage/`, `onboarding/`, `dictation/`, `navigation/`, `accounts/`, `cache/`, `theme/`, `layout/`, `stats/`, `platform/` | 1,062 / 571 / 375 / 309 / 251 / 161 / 159 / 73 / 56 / 51 / 45 | — | supporting |

`[fact]` (counts from `find`/`wc` over the directories).

### 1.3 iOS + Android parity

One codebase, both platforms, one bundle identifier reused for the iOS bundle id and the Android package, iPad supported (`supportsTablet: true`) with a split-view layout. `mobile/app.json:20-23,66-69`, `mobile/app/h/_layout.tsx:1-46` `[fact]`

Platform-specific handling is small and deliberate:

| Concern | iOS | Android |
|---|---|---|
| Local network | `NSLocalNetworkUsageDescription` + `NSAllowsLocalNetworking` | `usesCleartextTraffic: true` |
| Off-LAN ranges | ATS exception domains for `100.64.0.0/10` (CGNAT/Tailscale v4) and `fd7a:115c:a1e0::/48` (Tailscale v6) | covered by cleartext |
| Rotation | 4 orientations declared for phone and iPad | custom config plugin `plugins/android-respect-rotation-lock.js` |
| Secret storage | Keychain via `expo-secure-store`, `WHEN_UNLOCKED_THIS_DEVICE_ONLY` | Android keystore, with a "generation" fallback for a broken alias |
| Privacy manifest | `privacyManifests` with three accessed-API reasons, tracking false | `allowBackup: false` |

`mobile/app.json:24-43,45-60,62-64`; `mobile/src/transport/pairing-keychain.ts:5-30,86-90` `[fact]`

The Android keystore workaround is worth naming: `expo-secure-store` shares one keystore alias across unauthenticated items under a service, and a real failure was traced to that alias, so the code rotates through up to 8 generations of distinct keychain services to recover. `mobile/src/transport/pairing-keychain.ts:14-30` `[fact]`

### 1.4 Build and release

**Not EAS.** Local prebuild plus platform toolchains, driven by GitHub Actions. `[fact]`

```
tag mobile-ios-v*  ──▶ macos-26 runner, Xcode 26.5 ──▶ fastlane build_app
                                                   ──▶ altool upload ──▶ TestFlight external group
tag mobile-android-v* ──▶ ubuntu-latest ──▶ gradle APK ──▶ gh release create
PR touching mobile/** ──▶ ubuntu: pnpm typecheck, pnpm test, 3 ruby fastlane checks
```

| Aspect | Detail | Evidence |
|---|---|---|
| iOS lane | fastlane, **manual** code signing against a pre-imported distribution `.p12` plus an explicit App Store profile fetched with an ASC API key; the header documents why automatic/cloud signing was abandoned | `mobile/fastlane/Fastfile:1-18,58-66` |
| iOS runner | `macos-26` with Xcode `26.5`, because Expo 55's `expo-modules-core` uses Swift 6 syntax that Xcode 16 cannot parse; 90-minute timeout for a ~30-40 min archive | `.github/workflows/mobile-ios-release.yml:26-33,52-58` |
| iOS versioning | `fastlane/ios_release_version.rb` picks the next open patch version, skipping App Store version "trains" that are terminally closed (7 enumerated states, read from both the deprecated `appStoreState` and the newer `appVersionState`) | `mobile/fastlane/Fastfile:36-56,84-92` |
| TestFlight | external group `peeps`, 20-minute processing timeout, changelog via workflow input | `mobile/fastlane/Fastfile:32-35` |
| Android | separate workflow and separate tag so an Android ship never waits on App Store review; APK + GitHub Release, `contents: write` | `.github/workflows/mobile-android-release.yml:1-33`; `mobile/scripts/prepare-android-release.mjs` |
| Gem pinning | `BUNDLE_FROZEN: 'true'` in every job, because two jobs 25 minutes apart previously resolved different fastlane versions | `.github/workflows/mobile.yml:29-33`; `mobile-ios-release.yml:35-39` |
| PR CI | typecheck, vitest, and three Ruby tests that are the only thing in CI that loads the Fastfile | `.github/workflows/mobile.yml:58-70` |
| Dev loop | desktop on port 6768 + Metro on 8081; `scripts/mock-server.ts` plus a fixture set lets screens run with no desktop at all | `mobile/README.md:5-10`; `mobile/scripts/mock-server*.ts` (11 files) |

`[fact]` throughout.

---

## 2. Transport — how the phone reaches the desktop

### 2.1 The two paths and the logical client above them

```
                      ┌────────────────────────────┐
  screens ───────────▶│  StableLogicalRpcClient    │  stable identity across cutover
                      │  (transport/stable-        │  345 LOC
                      │   logical-rpc-client.ts)   │
                      └───────┬──────────┬─────────┘
                              │          │
              ┌───────────────▼──┐   ┌───▼────────────────────────┐
              │ DirectRpcClient  │   │ MobileRelayRpcSession      │
              │ ws(s)://host:6768│   │ wss://cell/v1/connect/<id> │
              │ E2EE v1 or v2    │   │ E2EE v2 only (framing: 2)  │
              └──────────────────┘   └────────────────────────────┘
                              ▲          ▲
                      ┌───────┴──────────┴─────────┐
                      │ MobileEndpointSupervisor   │ picks, races, upgrades,
                      │ + hysteresis + probes      │ rotates credentials
                      └────────────────────────────┘
```

`mobile/src/transport/rpc-client.ts:54-65` is the only public entry (`connect()` returns a `DirectRpcClient`); the relay path is composed by `MobileEndpointSupervisor` over `StableLogicalRpcClient`. `mobile/src/transport/mobile-endpoint-supervisor.ts:37-90` `[fact]`

A host profile carries **multiple candidate endpoints**, each tagged `lan | tailscale | relay`, up to 16 of them, plus an optional relay block:

```ts
MobileAccessEndpoint = { id, kind: 'lan'|'tailscale'|'relay', url }
```
`mobile/src/transport/mobile-relay-host-overlay.ts:4-23`, `mobile/src/transport/types.ts:94-126` `[fact]`

So: **direct LAN, direct Tailscale, and cloud relay — all three, chosen at runtime.** The default direct port is 6768. `mobile/src/transport/host-endpoint.ts:21` `[fact]`

### 2.2 Pairing flow

```
DESKTOP  Settings ▸ Mobile
   │  mint PairingOffer v2 (endpoint, deviceToken, publicKeyB64, [relay])
   │  base64url(JSON) ──▶ QR (EC level M)  and  <scheme>://pair?code=...
   ▼
PHONE    three ways in
   ├─ camera QR scan (expo-camera)            app/pair-scan.tsx
   ├─ paste the code or URL                   app/pair.tsx
   └─ external deep link (Messages, AirDrop)  app/_layout.tsx:52-71
   ▼
   app/pair-confirm.tsx  ── user confirms ──▶ 25s hard-capped connect attempt
   ▼
   token ──▶ Keychain/keystore;  metadata ──▶ AsyncStorage;  relay overlay ──▶ AsyncStorage
```

| Element | Shape / rule | Evidence |
|---|---|---|
| Offer version | `v: 2` literal | `src/shared/mobile-relay-pairing-offer.ts:9,72` |
| Offer fields | `endpoint`, `deviceToken`, `publicKeyB64` (pinned desktop X25519 key), optional `pairedDeviceId`, optional `scope: 'mobile'|'runtime'`, optional `relay` | same file `:70-81` |
| Relay block | `{v:1, directorUrl, cellUrl, assignmentEpoch, relayHostId (16 b64url), inviteToken (43 b64url), inviteExpiresAt, e2eeFraming: 2}` | same file `:47-68` |
| Invite freshness | must be in the future and ≤ 10 min out, with 30 s clock-skew leeway because the cell stamps expiry from its own clock | same file `:13-17,57-66` |
| Key canonicality | a relay offer requires a *canonical* 43+`=` base64 32-byte key, because `relayHostId` is derived from the decoded bytes | same file `:34-44,92-100` |
| Encoding | base64url, padding stripped, wrapped as a query param (not a fragment) because Android camera intents and the router preserve query params more reliably | `src/shared/pairing.ts:14-27` |
| Size guard | the encoded code is rejected above a max character budget | `src/shared/pairing.ts:21-23`; `src/shared/mobile-pairing-protocol-limits.ts` |
| Deep-link parsing | strict: scheme must match, host must be exactly `pair`, path must be empty or `/`; a prefix check that used to accept `…://pairing?…` was explicitly tightened | `src/shared/pairing.ts:40-60` |
| QR rendering | `qrcode`, error-correction level M, 4-module quiet zone, 2 px pitch | `src/main/runtime/mobile-pairing-qr.ts:5-18` |
| Connection mode | desktop offers "Anywhere" (`automatic`, relay, requires cloud sign-in) or "This network only" (`local-only`); an offer cannot be minted for Anywhere while signed out rather than silently degrading | `src/shared/mobile-pairing-connection-mode.ts:10-41` |
| Advertised address | auto-selection prefers a tailnet IPv4, then the first non-virtual interface; docker/vmnet/vEthernet bridges are filtered out unless they prove a default route | `src/shared/pairing-address-auto-selection.ts:9-41` |
| Pairing timeout | 25 s overall cap on the *initial* pair (the live client retries forever by design, but a half-broken route must surface an actionable error with the log visible) | `mobile/app/pair-confirm.tsx:22-27` |

`[fact]` throughout.

Note the duplicated parser: `mobile/src/transport/pairing.ts:3-6` states outright that it mirrors `src/shared/pairing.ts` but uses `atob`/`btoa` because Metro/Hermes ship no Node `Buffer`, with a "keep the parsing semantics in sync" comment. That is a live drift risk carried deliberately. `[fact]`

### 2.3 Auth model

```
┌──────────────────────────┐
│ DeviceRegistry (desktop) │  JSON file, hardened permissions
│  deviceId: uuid          │
│  token: 24 random bytes  │──▶ hex, per device, independently revocable
│  scope: 'mobile'|...     │──▶ gates the RPC method allowlist
│  pairingReach            │──▶ 'network' vs this-computer-only
│  relayBinding?           │
│  pushRegistration?       │──▶ survives desktop restart
└──────────────────────────┘
```

`src/main/runtime/device-registry.ts:1-4,25-40,91-99` `[fact]`

Enforcement is at the transport boundary, before dispatch:

```ts
if (device.scope === 'mobile' && !MOBILE_RPC_METHOD_ALLOWLIST.has(request.method)) { … 'forbidden' … }
```
`src/main/runtime/runtime-rpc/runtime-rpc-websocket-dispatch.ts:71-87` `[fact]`

The allowlist is an explicit 290-entry `Set`. `src/main/runtime/runtime-rpc/runtime-rpc-mobile-method-allowlist.ts:1-291` `[fact]` Revocation terminates sockets without a close handshake, on purpose: `src/main/runtime/rpc/ws-transport.ts:110-119` `[fact]`

Phone-side token custody: the device token was deliberately **split out of AsyncStorage** into the OS keychain as of the app's `v0.0.3`; the persisted profile schema no longer has a `deviceToken` field and it is joined in at load time. `mobile/src/transport/types.ts:128-137`, `mobile/src/transport/host-store.ts:34-42` `[fact]`

### 2.4 E2EE handshake — two generations

**v1 (legacy direct).** Plaintext hello, then everything encrypted.

```
phone                                        desktop
  │── ws open
  │── {type:'e2ee_hello', publicKeyB64}  ───▶      (plaintext)
  │◀── {type:'e2ee_ready'}               ────      (plaintext)
  │── enc{type:'e2ee_auth', deviceToken} ───▶
  │◀── enc{type:'e2ee_authenticated'}    ────
  │── enc{id, deviceToken, method, params} ──▶  … multiplexed by id
```

- ephemeral X25519 keypair per socket; shared key = `nacl.box.before(serverPublicKey, ourSecret)`
- JSON RPC rides text frames as `base64(nonce‖ciphertext)`; terminal/screencast data rides binary frames as the raw byte bundle
- `mobile/src/transport/rpc-client-socket-session.ts:104-248`; `mobile/src/transport/e2ee.ts:1-4,65-101` `[fact]`

Two Hermes-specific workarounds live in the crypto file and are worth copying as warnings: `tweetnacl` needs `crypto.getRandomValues`, which Hermes lacks, so the PRNG is wired to `expo-crypto`; and values crossing the native bridge can fail `instanceof Uint8Array` despite being byte arrays, so every input is re-wrapped. `mobile/src/transport/e2ee.ts:8-23` `[fact]`

**v2 (relay, and negotiated direct).** A typed, transcript-bound handshake.

| Element | Detail | Evidence |
|---|---|---|
| Hello | `{type:'e2ee_hello', v:2, clientPublicKeyB64, clientNonceB64, capabilities:{framing:[2], payloadKinds:['text','binary']}, context}` | `src/shared/mobile-e2ee-v2-contract.ts:15-22` |
| Ready | echoes the client nonce, adds `desktopNonceB64` and a `selection` | same file `:24-32` |
| Context | `{protocol:'…-mobile-e2ee', initiator:'mobile', responder:'desktop', transport:'direct'|'relay', relayHostId?}` — both sides' contexts must be byte-equal, and `relayHostId` is mandatory exactly when transport is relay | same file `:7-13,148-175,201-209` |
| Validation | `isExactRecord` rejects any extra or missing key; capabilities and selection must match exactly; base64 must be canonical (re-encode and compare) | same file `:45-108,177-199,211-226` |
| Transcript | 24 length-prefixed named fields hashed into the session, including protocol, both nonces, framing, payload kinds, and relay host id | same file `:110-146` |
| Frame | `secretbox(nonce, header‖payload)` where the 42-byte header = 32-byte sessionId ‖ direction byte ‖ payloadKind byte ‖ 64-bit counter, and the nonce is derived from the same four values — so a replay, a direction flip, or a kind confusion fails to open | `src/shared/mobile-e2ee-v2-framing.ts:6-60` |

`[fact]` throughout. This is a materially stronger design than v1: v1 has no transcript binding and no counter, so v2 exists because the relay is in the path `[inference]`.

### 2.5 Relay path

```
phone ── wss://<cell>/v1/connect/<relayHostId>
  │── {type:'relay-auth', v:1, mode:'connect', credential}         (plaintext, first frame)
  │◀── {type:'relay-hello', ok:true, credentialKind:'invite'|'resume', leaseExpiresAt, …}
  │                                    (or ok:false with a 4000-4999 code)
  │── E2EE v2 handshake ──────────────────────────────────▶ desktop (spliced by the cell)
  │◀── {type:'relay-moved', v:1, cellUrl, assignmentEpoch}  ──▶ re-dial the new cell
```

`mobile/src/transport/mobile-relay-physical-client.ts:91-156,211-216`; `src/shared/mobile-relay-phone-protocol.ts:5-51` `[fact]`

The cell splices two independently-dialled outbound sockets; neither side accepts inbound connections. `cloud/README.md:3-7` `[fact]` Its state machine is explicit and forward-only, with a rule that a client may only be acknowledged once both forwarding handlers exist, "success before both forwarding handlers exist can strand a client on a fake splice":

`pre-auth-admitted → credential-lease-reserved → host-notified → attach-pending → host-attached → client-acknowledged → spliced → e2ee-confirmable → teardown`

`cloud/packages/relay-contract/src/splice-state-machine.ts:1-34` `[fact]`

Relay protocol budget:

| Limit | Value | Note |
|---|---|---|
| max frame | 8 MiB | raised because a worktree catalog response already exceeded 1 MiB (~775 KiB at 415 worktrees); the comment names desktop-side pagination as the real fix |
| connections per host | 8 | |
| idle timeout | 10 min | |
| invite TTL / attempts / cooldown | 10 min / 5 / 2 s | plus a 15 s reservation lease |
| host attach deadline | 10 s | |
| resume TTL | 30 days | with a 30 s resume-confirmation deadline |
| relay token TTL | 5 min | |
| control ping / silence | 15 s / 75 s | |
| first frame deadline | 2 s | |

`cloud/packages/relay-contract/src/protocol-limits.ts:1-23` `[fact]`

Credential rotation is a journalled, idempotent two-phase exchange over the RPC channel itself: `pairing.provisionRelay {reqId, newResumeTokenHash, expectedCurrentHash?}` then `pairing.getEndpoints {installReqId?, resumeConfirmReqId?}` returning an install status of `not-found | committed`. The `reqId` survives process death so a crash mid-rotation cannot strand the device. `src/shared/mobile-relay-credential-contract.ts:17-85`; `mobile/src/transport/mobile-relay-pairing-journal-store.ts` `[fact]`

### 2.6 Reconnect, liveness, and endpoint selection

| Mechanism | Value / rule | Evidence |
|---|---|---|
| connect timeout | 12 s (no TCP/WS handshake → forced close, logged as `connect-timeout`) | `mobile/src/transport/rpc-client-socket-session.ts:15,250-270` |
| handshake timeout | 5 s (no `e2ee_ready`/`e2ee_authenticated`) | same file `:16,272-289` |
| liveness probe | client sends `status.get` with a `mobile-liveness-` id prefix; responses to it are dropped before normal routing | `mobile/src/transport/direct-rpc-client.ts:23,249-251,299-308` |
| server heartbeat | 15 s ping sweep; bounds half-open mobile sockets to ~60 s instead of the ~2 h OS keepalive, and requires consecutive unanswered probes | `src/main/runtime/rpc/ws-transport.ts:24-25` |
| pre-auth reaper | 10 s: a silent auto-ponging client cannot hold a finite mobile slot | same file `:14,304-313` |
| capacity | 128 WS connections, 256 TCP, 1 MiB max payload; over-capacity gets close 1013 then a 1 s force-terminate | same file `:9-13,215-221` |
| foreground nudge | three reasons — `focus` probes a healthy relay, `app-resume` likewise, `network-change` marks the socket suspect enough to replace | `mobile/src/transport/types.ts:90-92`; `direct-rpc-client.ts:174-203` |
| stale dial | a foreground event abandons a dial that is too old rather than waiting it out | `mobile/src/transport/rpc-stale-dial.ts` via `direct-rpc-client.ts:188-195` |
| relay→direct upgrade | hysteresis: 3 direct successes inside a 30 s observation window, 60 s minimum dwell, 60 s failure cooldown | `mobile/src/transport/mobile-endpoint-supervisor.ts:33-36,60-65` |
| background grace | relay is suspended (not torn down) on backgrounding, with a grace timer | `mobile/src/transport/mobile-relay-background-grace.ts` |
| request semantics | `{timeoutMs, budgetSpansConnect, failWhenDisconnected}` — a caller chooses replay-after-reconnect or fail-fast | `mobile/src/transport/rpc-client.ts:10-16` |
| stream replay | subscriptions are marked for replay on drop and re-issued after re-authentication | `direct-rpc-client.ts:242,272` |
| auth-failure latch | `unauthorized` retries a bounded number of times, then latches `auth-failed` (pairing revoked) rather than reconnecting forever | `direct-rpc-client.ts:252-255,268-285` |
| diagnostics | a 15-code enum (`connect-timeout`, `handshake-timeout`, `authentication-rejected`, `liveness-timeout`, `relay-dial-failed`, `direct-connected`, …) with a `lan|tailscale|relay` path tag, surfaced in a user-visible connection-log screen and persisted | `mobile/src/transport/types.ts:42-80`; `mobile/app/connection-log.tsx`; `persisted-connection-log-store.ts` |

`[fact]` throughout.

`transport/` is 234 files and 13,577 non-test lines. That is not incidental complexity: it is the cost of a phone that must survive backgrounding, network changes, two transports, cell migration, and credential rotation `[inference]`.

### 2.7 Offline cache and local storage

| Store | Mechanism | Policy | Evidence |
|---|---|---|---|
| Home snapshot | AsyncStorage key `…:home-snapshot:v1` | worktree info + per-host account usage; writes throttled 250 ms; exists so cold start and resume paint last-known-good instead of flashing empty for ~1 s | `mobile/src/cache/home-snapshot-cache.ts:1-59` |
| Worktree list | in-memory `Map` | 30 s TTL, 20-entry LRU, and a `proven` flag — only a list the host itself returned may prove a worktree *absent*; a cold-start seed may not | `mobile/src/cache/worktree-cache.ts:5-64` |
| Repo list | in-memory | `mobile/src/cache/repo-cache.ts` |
| Preferences | AsyncStorage | sidebar width, push opt-in, terminal/session view prefs | `mobile/src/storage/preferences.ts` (233 LOC), `session-view-preferences.ts` |
| Notification watermark | persisted per host | seq + epoch, see §5 | `mobile/src/notifications/notification-reconnect-catchup.ts` |
| Credentials | Keychain / keystore | device token only; plus a pending-cleanup journal for unpaired hosts | `pairing-keychain.ts`, `host-credential-cleanup.ts` (265 LOC) |

There is **no general offline write queue** `[fact]` — the caches exist to make reconnects feel instant, not to let the app work disconnected. Mutations either replay after reconnect or return `unknown` for the caller to surface (§3.3).

---

## 3. Feature surface

### 3.1 Route map

```
app/_layout.tsx  (root Stack, notification-tap routing, deep-link routing, splash)
│
├── index.tsx ───────────────────── src/home/MobileHomeScreen          host list
├── pair.tsx / pair-scan.tsx / pair-confirm.tsx                        pairing
├── mobile-onboarding.tsx, notification-opt-in.tsx
├── settings.tsx, terminal-settings.tsx, voice-settings.tsx,
│   browser-settings.tsx, native-chat-settings.tsx, notifications.tsx, about.tsx
├── connection-log.tsx, troubleshoot.tsx                               diagnostics
│
└── h/_layout.tsx  (host stack; tablet split view with a resizable sidebar)
    └── [hostId]/
        ├── index.tsx ──────────── src/host-screen                     worktrees for one host
        ├── edit.tsx                                                   endpoint edit, no re-pair
        ├── accounts.tsx                                               agent accounts / usage
        ├── tasks.tsx ──────────── src/tasks (38 staged hooks)         issues + PRs + new workspace
        ├── session/[worktreeId] ─ src/session/MobileSessionSurface     THE main screen
        ├── source-control/[worktreeId] ─ src/source-control           changes | history | PR
        ├── review/[worktreeId] ── src/session diff review             diff review
        ├── files/[worktreeId] (+ files/preview/…) ─ src/files          file explorer
        ├── agent-history/[worktreeId] ─ src/agent-history              past sessions
        ├── pr/[worktreeId] ────── redirect → source-control?tab=pr
        └── history/[worktreeId] ─ redirect → source-control?tab=history
```

`mobile/app/**` `[fact]`. The PR and history routes are now thin redirects into a single source-control hub, kept only so existing deep links land. `mobile/app/h/[hostId]/pr/[worktreeId].tsx:3-8` `[fact]`

### 3.2 Per-screen reads and actions

Methods below are the literals actually present in each directory `[fact]` (grep of `sendRequest('…')` / `subscribe('…')`).

| Screen | Reads | Actions it can send back | Notable RPCs |
|---|---|---|---|
| **Home** (`src/home`) | host catalog from local storage; cached home snapshot; account usage stream | open host, pair new host, host action sheet (disconnect, forget, edit) | `accounts.subscribe` |
| **Host screen** (`src/host-screen`) | repo list, worktree list, UI state | activate a worktree, put one to sleep, remove one, set worktree fields | `repo.list`, `worktree.activate`, `worktree.set`, `worktree.sleep`, `worktree.rm`, `ui.get`, `ui.set` |
| **Session** (`src/session`, 35k LOC) | structured agent transcript stream, session tab inventory, terminal streams, worktree detail, settings, quick commands, native-chat session | **send a follow-up prompt** (`agentSession.send`), **approve/deny a permission prompt** (`agentSession.respondToApproval`), **answer a question prompt** (`agentSession.respondToQuestion`), cancel a turn, hold/release a session, set options, run a conversation command, create/close/rename/focus terminals, send terminal bytes, clear buffer, set display mode, stage a hunk, read/save a markdown tab, upload an image in chunks | `agentSession.subscribe/create/send/cancel/respondTo*/hold/release/setOption/options/history/conversationCommand`, `session.tabs.*`, `terminal.subscribe/send/close/rename/focus/clearBuffer/setDisplayMode/updateViewport`, `clipboard.*ImageUpload*`, `git.diff/stage/status/branchDiff/branchCompare`, `files.read/list/searchPaths/openDiff`, `markdown.readTab/saveTab`, `nativeChat.subscribe/readSession`, `preflight.detectAgents` |
| **Tasks** (`src/tasks`, 23k LOC) | GitHub / GitLab / Linear work items, project views and tables, repo hooks, sparse presets, SSH targets, preflight | create an issue, update an issue/MR state, comment, reply to a review thread, merge a PR/MR, set auto-merge, request reviewers, rerun checks, mark files viewed, **create a new worktree/workspace from a task** | `github.*` (~40 methods), `gitlab.*`, `linear.*`, `worktree.create`, `repo.saveSparsePreset`, `ssh.getState`, `preflight.check` |
| **Source control** (`src/source-control`) | git status, diff, history, commit compare, upstream status, base-ref default, hosted-review eligibility | stage/unstage/bulk stage, discard, commit (with AI-generated message + cancel), push, pull, fetch, checkout, fast-forward, rebase from base, abort merge/rebase, create a hosted review | `git.*` (~28 methods), `hostedReview.*`, `repo.baseRefDefault` |
| **Files** (`src/files`) | directory listing, file read, chunked read, preview, doc preview, terminal-artifact read, path resolution | create a file, write a terminal artifact, open a diff | `files.readDir/read/readPreview/list/resolveTerminalPath/writeTerminalArtifact/createFile`, `git.diff` |
| **Agent history** (`src/agent-history`) | past agent sessions across repos and folder workspaces, resolved titles | prepare a session resume (which reveals a chat tab on the desktop) | `aiVault.listSessions/resolveSessionTitles/prepareSessionResume`, `folderWorkspace.list`, `projectGroup.list`, `worktree.ps` |
| **Browser** (`src/browser`) | JPEG/PNG screencast frames over the binary channel | full remote input: goto, back, forward, reload, mouse down/up/move/click/wheel, keypress, insert text, accept/dismiss dialogs, create a tab, set viewport | `browser.screencast` + `browser.*` (18 methods) |
| **Dictation** (`src/dictation`) | model list, setup state (polled) | download/delete a speech model, start a dictation, stream audio chunks, finish or cancel — transcription runs **on the paired desktop**, not the phone | `speech.dictation.setup/start/chunk/finish/cancel`, `speech.models.*` |
| **Accounts** (`app/h/[hostId]/accounts.tsx`) | account list and usage | select a Claude/Codex account, consume a Codex reset credit | `accounts.list/selectClaude/selectCodex/selectCodexForTarget/consumeCodexResetCredit` |
| **Stats** (`src/stats`, 51 LOC) | a summary total for the home card | none | `stats.summary` |
| **Notifications / settings** | desktop settings, notification preferences | update settings, update terminal quick commands, toggle local push opt-in | `settings.get/update`, `notifications.subscribe/unsubscribe/getMissedSince` |
| **Diagnostics** | connection log ring buffer, persisted log, memory diagnostics | force reconnect, disconnect, forget host | `diagnostics.memory`, `status.get` |
| **Worktree** (`src/worktree`) | process list per worktree, retired names | activate | `worktree.ps/activate/listRetiredNames` |

The phone's total wire vocabulary is ~200 distinct method literals `[fact]`, against a 290-entry allowlist — the desktop allows more than the phone currently calls.

Read/write asymmetry is worth stating plainly: this is **not a monitoring app**. It can create worktrees, commit, push, merge PRs, drive a remote browser, and send raw bytes to a PTY `[fact]`.

### 3.3 Mutation safety — the pattern to copy

Every structured session mutation carries a client-generated operation id and an expected fence, and the result is a three-way outcome:

```ts
{ status: 'accepted', value, sameFence } | { status: 'rejected' } | { status: 'unknown' }
```

- `clientOperationId` is a 32-hex durable id, with a Hermes fallback because RN has no guaranteed `crypto.randomUUID`
- retained operation ids are bounded **by expiry, not by count**: "every retained id belongs to a send whose outcome is still unknown, so dropping one turns the user's retry into a second message on the host"
- prompt responses carry `expectedRevision`, so answering a stale prompt is refused rather than mis-applied
- `unknown` surfaces to the user as "Response unconfirmed — check chat before retrying" instead of silently retrying

`mobile/src/session/mobile-structured-agent-session-rpc.ts:17-32,52-96,98-120`; `mobile/src/session/use-mobile-structured-prompt-responses.ts:42-116` `[fact]`

---

## 4. Terminal on mobile

```
┌──────────────────────────── React Native ────────────────────────────┐
│ session screen                                                        │
│  ├─ TerminalWebView (396 LOC)  ──postMessage──▶ ┌──────────────────┐ │
│  │    write coalescer, pending queue,            │ WebView document │ │
│  │    ready watchdog, theme, fit measure         │  xterm 6.1 +     │ │
│  │  ◀──onMessage── taps, selection, modes,       │  webgl + uni11   │ │
│  │      haptics, key input, url/file taps        │  (esbuild bundle,│ │
│  │                                                │   chrome74)      │ │
│  ├─ direct input capture (hidden TextInput)      └──────────────────┘ │
│  └─ accessory key bar (Ctrl/Alt/Shift, F1-F12, arrows, Esc, Tab)      │
└───────────────────────────────────────────────────────────────────────┘
             ▲ binary frames, kind 0x74 v1                │ terminal.send
             │ Output / Snapshot{Start,Chunk,End} /        ▼
             │ Resized / Error / Metadata            desktop PTY
```

| Question | Answer | Evidence |
|---|---|---|
| xterm in a webview? | Yes. `@xterm/xterm` + webgl + unicode11, bundled by esbuild to a `chrome74` syntax target and inlined as a generated TS module | `mobile/package.json:8-11`; `mobile/scripts/build-terminal-webview-engine.mjs:11-13,26-33` |
| Old-WebView survival | Three runtime shims are injected because esbuild lowers syntax but not runtime APIs: `WeakRef`, `structuredClone` (via JSON round-trip), `Element.replaceChildren` | same script `:38-60` |
| Read-only or interactive? | **Fully interactive.** Two input modes: a hidden capture field that forwards keystroke bytes straight to the PTY (direct), and a visible command box that sends on Enter (buffered). Direct is the default for a newly-seen terminal | `mobile/mobile-terminal-direct-input-default.md:5-14,17-24`; `mobile/src/terminal/terminal-live-input.ts` |
| Keyboard handling | ~25 special keys mapped to escape sequences (arrows, F1-F12, Home/End/PgUp/PgDn, Insert/Delete/Tab/Esc/Backspace); Enter is deliberately left on `onSubmitEditing` to avoid double carriage returns; accessory bar composes Ctrl/Alt/Shift with a key | `mobile/src/terminal/terminal-live-input.ts:7-45`; `terminal-accessory-keys.ts:15-40`; `terminal-key-definitions.ts` |
| Scrollback replay | On subscribe the host sends `SnapshotStart` (JSON meta) → N `SnapshotChunk` (text) → `SnapshotEnd`; the client reassembles and emits one `{type:'scrollback'|'resized', serialized}` event | `mobile/src/transport/rpc-client-terminal-binary-frame.ts:41-75` |
| Frame format | 16-byte header: kind `0x74`, version 1, opcode, reserved, `uint32 streamId`, 64-bit little-endian `seq` split across two `uint32`s | `mobile/src/transport/terminal-stream-protocol.ts:1-57` |
| Throughput | A busy PTY delivers ~200 frames/s; writes are coalesced before crossing the bridge because per-frame bridge + WebKit IPC + paint "runs the phone hot" | `mobile/src/terminal/TerminalWebView.tsx:89-94` |
| Sizing | The client measures fit dimensions inside the webview after `term.open()`, then rewrites the live subscription's `viewport` params and calls `terminal.updateViewport` | `mobile/src/terminal/terminal-viewport-refit.ts`; `mobile/src/transport/rpc-client-terminal-subscription.ts:12-34` |
| Extras in the webview | touch gesture surface, selection overlay, mouse reporting and scroll routing, smooth scroll + cell geometry, text scaling, OSC link ranges, file-path tap → open file, URL tap → open browser, haptics | `mobile/src/terminal/terminal-webview-html/*` (10 modules), `terminal-path-tap.ts`, `terminal-webview-url-tap.ts` |
| Failure mode | A ready watchdog plus an engine-error overlay: if the webview never reports ready, the user sees a diagnosable error rather than a blank pane | `mobile/src/terminal/terminal-webview-ready-watchdog.ts`, `terminal-webview-engine-error-state.tsx` |
| Capability gates | The phone must not forward xterm query replies unless the host advertises `terminal.query-reply-input.v1`, because older hosts strip `inputKind` and the reply would land as ordinary floor-taking input | `src/shared/protocol-version.ts:99-103` |

`[fact]` throughout. The same webview trick is reused for mermaid diagram rendering `[fact]` (`mobile/scripts/build-mermaid-webview-engine.mjs`).

---

## 5. Push notifications

### 5.1 The finding that matters most

**In this snapshot the phone has no remote-push registration at all.** `[fact]`

Evidence:
- a case-insensitive grep for `apns`, `fcm`, `devicePushToken`, `getDevicePushToken` across all of `mobile/` (ts, tsx, js, json) returns **zero** files
- `notifications.registerPush` and `notifications.unregisterPush` are in the desktop allowlist (`runtime-rpc-mobile-method-allowlist.ts:175,177`) but appear **nowhere** in the union of ~200 method literals the phone actually references
- the phone's only notification path is `expo-notifications` *local* notifications scheduled from the live `notifications.subscribe` stream, gated on a locally stored opt-in preference (`mobile/src/notifications/local-notification-scheduling.ts:3,85,118`)
- the desktop side is complete and tested (`src/main/runtime/push/`, 30 files including 14 test files), and the protocol constant exists (`NOTIFICATIONS_REMOTE_PUSH_RUNTIME_CAPABILITY = 'notifications.remote-push.v1'`, `src/shared/protocol-version.ts:215-216`)

`[inference]` The cloud gateway and desktop half shipped first and the phone wiring is either newer than this snapshot or lives in a build not represented here. Either way, **the shipped phone experience in this tree only raises banners while the app is running with a live socket.** That is a real product gap, and it is exactly the gap a push gateway exists to close.

### 5.2 What the live-socket path does today

```
desktop notification dispatched
   │
   ├──▶ socket fan-out ──▶ notifications.subscribe stream ──▶ phone
   │                                                            │
   │                                    expo-notifications local banner
   │
   └──▶ PushDispatcher ──▶ push gateway ──▶ APNs/FCM   (desktop side only, today)

on reconnect / cold open:
   phone ──▶ notifications.getMissedSince {lastSeenSeq, epoch} ──▶ bounded retained buffer
```

The catch-up protocol is the most carefully-reasoned code in the mobile tree and is directly reusable as a design:

| Rule | Why (quoted intent) | Evidence |
|---|---|---|
| Watermark = `{seq, epoch}` persisted per host | the epoch is the desktop counter's lifetime; sending it lets the desktop reject a watermark from a counter it no longer has and return the whole retained buffer instead of nothing | `mobile/src/notifications/mobile-notifications.ts:100-113,142-165` |
| Watermark advances **after** the banner lands, never before | advancing first means a process death in between silently drops it, and the next launch asks for a seq the user never saw | same file `:100-104` |
| A failed catch-up **quarantines** the watermark | the abandoned range stays unrecovered until some later catch-up succeeds; a live seq persisting past it meanwhile would make the desktop cut it forever | same file `:166-172,195-201` |
| The whole missed batch is ONE queue entry | awaiting per event returns to the event loop, so a live seq 11 can slot between replayed 6 and 7 and persist a watermark past an unshown notification | same file `:173-178` |
| The catch-up *request* stays outside the queue | it waits up to 30 s, and holding the chain would stall live delivery on a slow link | same file `:175-177` |
| Cold open catches up only if this device delivered for this host before | a first-ever pairing must not be handed the desktop's whole retained buffer | same file `:246-250` |
| `notificationId` dedup claimed before enqueue, not inside the task | inside the queued task the first delivery has already finished, so the overlap is no longer observable | same file `:47-55` |

`[fact]`

### 5.3 The push gateway (cloud, desktop-authenticated)

```
DESKTOP                                   PUSH GATEWAY (Hono on Cloud Run)
  │ POST /v1/host/challenge {v:1, hostPublicKeyB64}
  │                                         ├─ issue: ephemeral X25519 key, 24-byte nonce,
  │◀── {challengeId, gatewayEphemeralPublicKeyB64, nonceB64, ciphertextB64, expiresAt}
  │                                         │  box(transcript ‖ 32-byte secret)
  │ (decrypt, HMAC-SHA-256 the transcript)
  │ POST /v1/host/session {challengeId, proofB64}
  │◀── {sessionToken (43 b64url), expiresAt, hostFingerprint (16 b64url)}   24 h
  │
  │ POST /v1/devices    {v:1, deviceId, platform, token, apnsEnvironment?}  Bearer
  │◀── {registrationId}
  │ POST /v1/send       {v:1, registrationIds[≤20], notification}           Bearer
  │◀── {results: [{registrationId, status: queued|dead|rate_limited|error}]}
  │
  └── DELETE /v1/devices/:registrationId     (durable outbox, retried)
                                              │
                                    DurablePushStore → DurablePushWorker
                                              ├─▶ ApnsClient  (HTTP/2, JWT from .p8)
                                              └─▶ FcmClient   (v1 API, runtime SA token)
```

`cloud/apps/push/src/push-server.ts:130-298`; `cloud/packages/push-contract/src/*` `[fact]`

| Property | Value | Evidence |
|---|---|---|
| Who authenticates | **The desktop host, with the same X25519 key it uses for the relay.** The phone never holds a cloud credential for the gateway | `cloud/README.md:33-40`; `push-server.ts:178-208` |
| Proof construction | 10 length-prefixed named fields (protocol domain, version, gateway origin, gateway ephemeral key, challenge nonce, challengeId, issuedAt, expiresAt, host fingerprint, host public key) → HMAC-SHA-256 over `domain‖"ack"‖transcript`; the encrypted random 32-byte secret makes the public transcript insufficient to forge the ack | `cloud/packages/push-contract/src/push-host-proof-transcript.ts:1-90` |
| Sign-in independence | Built alongside the relay service but **deliberately not gated on cloud sign-in** — the gateway authenticates with the host keypair, so accountless hosts push too | `src/main/runtime/push/desktop-push-service.ts:1-4` |
| Triggering events | `agent-task-complete`, `terminal-bell`, `plugin`; plus a `dismiss` kind for live dismissal | `cloud/packages/push-contract/src/device-registration-messages.ts:6-11`; `src/main/runtime/push/push-dispatcher.ts:122-146` |
| Agent state mapping | the desktop's richer status collapses to exactly two values before it leaves: `blocked`/`waiting`/`needs-input` → `needs-input`; `done`/`finished`/absent → `finished`; anything else suppresses the push | `src/main/runtime/push/push-dispatcher.ts:41-52`; `src/shared/mobile-push-contract.ts:7-10` |
| Delivery filters | per-registration `{onlyWhenDesktopAway?, sound?}`; a desktop-suppressed notification is not pushed | `src/shared/mobile-push-contract.ts:18-21`; `push-dispatcher.ts:54-63` |
| Burst control | a shared cooldown keyed by `[deviceId, worktreeId|'global']` | `push-dispatcher.ts:164-172` |
| Collapse key | `sha256([hostFingerprint, notificationId ?? [epoch, seq]])`, used as the APNs collapse header | `cloud/apps/push/src/push-delivery-message.ts:26-34` |
| Payload budget | title ≤ 80 chars, body ≤ 180, whole notification JSON ≤ 3,000 bytes (enforced by a zod refine), HTTP body ≤ 16 KiB, 5-minute TTL | `cloud/packages/push-contract/src/push-limits.ts:1-15`; `send-messages.ts:27-41` |
| Quotas | 300 host events / 15 min; 20 registration ids per send; 64 devices per host; 30 unauthenticated req/min/IP; 6,000 authenticated req/min/IP; 600/min/host | `push-limits.ts:4-20` |
| Dead tokens | a registration is retired as soon as Apple or Google reports the token unregistered; the desktop drops it from its registry on a `dead` result | `cloud/README.md:37-39`; `push-dispatcher.ts:246-270` |
| Durability | each delivery is one persisted notification event; PostgreSQL in production, SQLite for tests; a delete queued while the gateway was unreachable survives restarts via a durable outbox with 30 s → 10 min backoff | `cloud/README.md:41-44,60-63`; `desktop-push-service.ts:18-19,182-270` |
| Known weakness | FCM notification messages are collapsible while offline with a small concurrent collapse-key budget, so **not every pending alert is guaranteed** | `cloud/README.md:44-47` |

**Privacy of payloads** `[fact]`:
- the gateway logs **aggregate counters only**; tokens, notification titles, bodies, and full host fingerprints never reach a log line (`cloud/README.md:69-71`); the error handler deliberately logs only `error.name` because a Postgres error carries the offending row in `detail` (`push-server.ts:117-128`)
- **but** the notification itself is not end-to-end encrypted: `title`, `body`, `worktreeId`, `notificationId`, `source`, `agentState`, and `hostFingerprint` travel as cleartext JSON to the gateway and on to APNs/FCM (`push-delivery-message.ts:36-75`). The title/body are clipped and whitespace-normalised on the desktop, but they are real content (`push-dispatcher.ts:36-39,188-189`).
- the phone's identity at the gateway is an opaque `registrationId`; the host is an opaque 16-char fingerprint `[fact]`

`[inference]` If we want push payload privacy we would have to send a content-free "something happened, id X" ping and have the phone fetch the body over the E2EE channel. The reference chose deliverability over that.

### 5.4 Mobile-side notification plumbing that does exist

| Piece | What it does | Evidence |
|---|---|---|
| Foreground handler | `setNotificationHandler` with banner + list + sound true, because expo silently drops foreground notifications otherwise; set at module load before anything is scheduled | `mobile/app/_layout.tsx:22-34` |
| Tap routing | cold start via `getLastNotificationResponse()`, warm start via a listener; non-default action identifiers are cleared; a FIFO-capped 256-entry set dedups taps since the root layout never unmounts | `mobile/app/_layout.tsx:73-125` |
| Target resolution | `{hostId, worktreeId?}` → a host-stack route; an unknown `hostId` routes nowhere; a missing credential routes to re-pair, a temporarily-unavailable one to retry | `mobile/src/notifications/notification-routing.ts:46-91` |
| Opt-in gate | a local preference plus OS permission state, with a dedicated `notification-opt-in` screen | `mobile/src/notifications/notification-opt-in-gate.ts:1-27`; `mobile/app/notification-opt-in.tsx` |
| Scheduling cap | a bounded scheduled-notification registry (`setScheduledNotificationsMaxForTests`) | `mobile/src/notifications/local-notification-scheduling.ts` |

`[fact]`

---

## 6. Shared code and wire compatibility

### 6.1 How code is shared

```
repo root
├── src/shared/            ◀─── 604 import sites from mobile, 158 distinct modules
│   └── rpc-contract/      ◀─── TYPE-ONLY from mobile, enforced by a test
├── src/main/runtime/      (desktop RPC server, not imported by mobile)
├── mobile/                imports the above by RELATIVE PATH: '../../../src/shared/…'
└── cloud/                 separate pnpm workspace; push-contract duplicates
                           wire scalars rather than importing relay-contract
```

There is **no package boundary** between mobile and desktop: mobile reaches up out of its own workspace with `../../../src/shared/…` specifiers. `[fact]` Top shared modules by import count:

| Module | Sites |
|---|---|
| `native-chat-types` | 39 |
| `github/pull-request-types` | 26 |
| `runtime-types` | 23 |
| `diff-comment-types` | 23 |
| `tui-agent` | 19 |
| `ai-vault-types` | 18 |
| `github/work-item-types` | 16 |
| `hosted-review`, `github/check-types` | 14 each |
| `mobile-relay-credential-contract`, `execution-host`, `agent-status-types` | 13 each |

`[fact]`

Is the RPC contract shared? **The types are; the schemas are not, on purpose.** `src/shared/rpc-contract/` holds ~60 `*-params.ts` files of zod schemas, and a dedicated vitest suite walks every file under `mobile/app` and `mobile/src` with the TypeScript AST and fails the build if any import, re-export, `require`, or dynamic `import()` of that directory would emit a runtime require. The stated reason: bundling one would let client code call `parse()`, and the shared `requiredString` helper is `z.unknown().transform(...)` which coerces a non-string to `''` instead of rejecting it — "silently changing the bytes the phone puts on the wire." `mobile/src/rpc-params-contract-type-only-boundary.test.ts:1-143` `[fact]`

That is a genuinely good piece of engineering and the single most transferable idea in this section: **share the shape, never the coercion.**

### 6.2 How wire compatibility is kept across versions

Two independent mechanisms, both in `src/shared/protocol-version.ts` `[fact]`:

**(a) A coarse protocol version with a compatibility window.**

```
RUNTIME_PROTOCOL_VERSION            = 3
MIN_COMPATIBLE_RUNTIME_CLIENT_VERSION = 2
MIN_COMPATIBLE_RUNTIME_SERVER_VERSION = 2
```

The file states the rules explicitly: bump on removing a method or required param, on changing the *meaning* (units, nullability) of a field clients read, or on changing encrypted framing / terminal stream framing / auth. Do **not** bump for adding methods, adding optional fields, or adding ignorable event types. "Exact app-version equality is never required." `src/shared/protocol-version.ts:15-36` `[fact]`

**(b) ~90 named string capabilities, negotiated both directions.**

```
phone authenticates
   │
   ├─▶ runtime.clientCapabilities.update  (what the PHONE can decode)
   │      one-way advisory; an unanswered request is treated as
   │      "capabilities unavailable, proceed" — only a frame that never
   │      reached the wire force-closes the socket
   │
   └─◀ status.get → { capabilities: string[] }   (what the HOST can do)
          retried with backoff, and re-asked promptly after a
          relay→direct cutover, because a one-shot probe would latch
          capability-gated UI hidden until the screen remounts
```

`mobile/src/transport/mobile-runtime-capability-negotiation.ts:10-57`; `mobile/src/transport/runtime-capability-probe.ts:6-67`; `src/shared/protocol-version.ts:220-321` `[fact]`

The capability comments are a catalogue of the failure modes this solves `[fact]`, and they are the best argument I found for adopting the pattern:

- *older hosts strip an unknown optional field* → `terminal.query-reply-input.v1`: without it a mobile xterm query reply lands as ordinary floor-taking input (`:99-103`)
- *a strict union rejects an unknown key* → `agent-session.structured.resume-history.v1`: an older host rejects the added `resumeFrom` as a schema error, which a client cannot distinguish from a real refusal; worse, without probing the client cannot tell whether a host that accepted the call adopted the conversation or quietly started a blank one (`:158-163`)
- *additive to an already-shipped surface* → `agent-session.status-feed.v1`: a host advertising the parent capability may still answer with `method_not_found`, so clients must probe or "reconnect forever and never show any status at all" (`:164-167`)
- *unknown enum member* → `agent-session.kimi-resume.v1`: growing an enum makes older hosts answer `invalid_argument`, a code the launch fallback does not retry (`:185-188`)
- *idempotency fields stripped* → `worktree.create-idempotency.v1`, `terminal.create-idempotency.v2`: without the capability a client must not replay an ambiguous mutation (`:112-121`)
- *client-side differences* → `NATIVE_REMOTE_RUNTIME_CLIENT_CAPABILITIES` vs `ELECTRON_…`, because only the renderer runs the retirement-proof ledger; CLI and mobile must keep full lists (`:220-239`)
- there is even a named compat alias block with a removal date for mobile builds still reading old constant names (`:323-326`)

`[inference]` The lesson: a single integer version is not enough once a mobile client ships to an app store and cannot be force-upgraded in lockstep with the desktop. Named capabilities are what let them add methods and fields weekly without breaking phones in the field.

---

## 7. What I would and would not copy

**Adopt**

1. **Capability negotiation on top of a coarse protocol version**, with the "probe before offering the action" discipline. `src/shared/protocol-version.ts` `[fact]`
2. **Type-only sharing of wire shapes, with a build-enforced boundary against sharing the validators.** `mobile/src/rpc-params-contract-type-only-boundary.test.ts` `[fact]`
3. **Per-device revocable tokens + a scope-gated method allowlist, enforced at the transport boundary.** `runtime-rpc-websocket-dispatch.ts:71-87` `[fact]`
4. **Application-level E2EE with the host key pinned in the pairing offer**, so a relay can be untrusted and TLS is optional. `mobile-e2ee-v2-contract.ts` `[fact]`
5. **The v2 framing header** (sessionId + direction + payload-kind + monotonic counter, all bound into the nonce). Cheap, and it kills replay, direction-flip, and kind-confusion in one move. `mobile-e2ee-v2-framing.ts:6-60` `[fact]`
6. **Three-way mutation outcomes with client operation ids and expected revisions**, and expiry-bounded (not count-bounded) retained ids. `mobile-structured-agent-session-rpc.ts:17-96` `[fact]`
7. **The notification seq + epoch watermark with `getMissedSince` and quarantine-on-failure.** `mobile-notifications.ts:100-201` `[fact]`
8. **Multi-endpoint host profiles with racing and hysteresis**, and a user-visible connection log with a coded diagnostic enum. `mobile-endpoint-supervisor.ts:33-36`, `types.ts:42-80` `[fact]`
9. **The xterm-in-a-webview terminal with binary snapshot replay and a write coalescer.** `TerminalWebView.tsx:89-94`, `terminal-stream-protocol.ts` `[fact]`
10. **A mock server with fixtures so phone screens develop without the real backend.** `mobile/scripts/mock-server.ts` + 10 fixture modules `[fact]`

**Adapt**

- **The push architecture is right; the payload policy is a choice.** Host-keypair auth with no phone-held cloud credential is the correct shape. Whether title/body cross the gateway in cleartext is a separate decision we should make consciously (§5.3).
- **Local notifications off the live socket are a decent v1** and the catch-up machinery makes them almost-reliable, but they cannot fire with the app suspended. Ship them first, ship push second, and design the seq/epoch feed so both consume the same stream.

**Avoid**

- **Relative-path imports across workspace boundaries.** 604 import sites reaching `../../../src/shared/` is the tightest possible coupling, and it forced a hand-mirrored second copy of the pairing parser with a "keep in sync" comment (`mobile/src/transport/pairing.ts:3-6`). For us this is moot in the worst way: a Rust core cannot be imported by TypeScript at all, so we must solve it properly from day one.
- **Shipping the relay in v1.** 18,514 non-test lines in the relay app, 41 source files, a 990-line wire contract, a Terraform root, a fence-broker service, an ops console, and 25 CI workflows — every one of them gated off by an unset repository variable. `cloud/apps/relay/src`, `cloud/README.md:63-95` `[fact]`
- **A 290-entry hand-maintained allowlist with no generator.** It works, and the enforcement point is right, but it is a list a human must remember to edit.
- **Letting one screen reach 35,000 lines.** `session/` is 408 files; the tasks route composes **38 sequentially-chained hooks** in one component (`mobile/app/h/[hostId]/tasks.tsx:41-77`) `[fact]`. The naming is disciplined and each hook is small, but a reader cannot tell what state exists without reading all 38.
- **Four stray design notes committed at the mobile root** (`issue-5049-unresponsive-session-findings.md`, `terminal-output-streaming-findings.md`, `mobile-terminal-direct-input-default.md`, plus two HTML mocks) `[fact]`. Useful content, wrong place.

---

## 8. Cost estimate for an equivalent

Derived from the reference's own line counts `[inference]`, assuming we reuse its architecture but not its code:

| Component | Reference size | v1 target | Note |
|---|---|---|---|
| Transport (pair, E2EE, reconnect, multi-endpoint) | 13,577 LOC | ~3,000-4,000 | drop relay, drop credential rotation, keep E2EE + reconnect + LAN/Tailscale racing |
| Terminal (webview host, input, replay) | 8,115 | ~2,000 | keep xterm + snapshot replay + direct input; drop mouse reporting, selection overlay, path taps |
| Session surface | 35,056 | ~4,000-6,000 | transcript + composer + prompt responses only |
| Host/home/worktree list | 5,435 | ~1,500 | |
| Notifications | 1,089 | ~800 | the catch-up logic does not compress much; it is all edge cases |
| Source control (read-only) | 7,421 | ~1,000 | status + diff view, no mutations in v1 |
| Components/theme/storage/nav | ~21,000 | ~3,000 | |
| Tasks, browser, dictation, files, agent-history, stats | ~33,800 | 0 | defer entirely |
| **Mobile total** | **132,102** | **~16,000-19,000** | plus tests |
| Daemon-side protocol additions | — | see §9 | the expensive part is idempotency + status feed |
| Push gateway | 2,411 (+761 contract) | ~2,500 | if and when we do remote push |
| Relay | 18,514 (+990 contract + IaC) | 0 | not in v1 |

`[inference]` A focused v1 on a solid daemon protocol is a few engineer-months of mobile work. Retrofitting idempotency and a sequenced status feed into a daemon that did not plan for them is the part that turns into a quarter.

---

## Decision inputs for us

### A. What the core/daemon must expose so a phone client is possible later

These are the things the reference could not have added cheaply after the fact `[inference]`, derived from the cited mechanisms above.

**1. Protocol — one multiplexed, bidirectional, streaming channel.**
- request/response keyed by `id`, plus long-lived subscriptions on the same socket, plus an explicit `unsubscribe` (`mobile/src/transport/rpc-client.ts:24-47`) `[fact]`
- a separate **binary lane** for PTY output and image/screencast frames, with its own typed frame header carrying `kind`, `version`, `opcode`, `streamId`, and a **monotonic 64-bit seq** (`terminal-stream-placeholder` → `mobile/src/transport/terminal-stream-protocol.ts:1-57`) `[fact]`. JSON-encoding PTY bytes is the mistake this avoids.
- **snapshot-then-tail** on subscribe: `SnapshotStart` (meta) → chunks → `SnapshotEnd`, so a phone attaching to a running terminal gets scrollback without a second RPC `[fact]`
- per-request timeout and an explicit "fail rather than replay" flag, chosen by the caller `[fact]`
- **decision for us:** our Rust daemon needs its frame format specified *before* the Tauri desktop is the only client, or the desktop will end up depending on framing details that make the phone a rewrite. Define frames in Rust, generate TypeScript types, and never hand-write the phone's copy.

**2. Auth — per-device, revocable, scoped.**
- a persisted device registry (`deviceId`, random token, `scope`, `pairedAt`, `lastSeenAt`, reach), with file permissions hardened, and revocation that terminates sockets immediately (`src/main/runtime/device-registry.ts:25-40,91-99`; `ws-transport.ts:110-119`) `[fact]`
- **a per-scope method allowlist checked before dispatch**, not per-handler `[fact]`
- **decision for us:** a single shared daemon token (the obvious first design) makes a phone companion unshippable — you cannot revoke one device, and you cannot give the phone a smaller surface than the desktop. Per-device tokens with a `scope` field cost almost nothing now.

**3. Encryption — application-level, key pinned by the pairing offer.**
- ephemeral X25519 per connection, authenticated encryption, host public key delivered in the QR, transcript-bound handshake, counter in the frame header `[fact]`
- **decision for us:** this is what lets a relay exist later without trusting it, and it removes the "how do we get a TLS cert for a laptop on a LAN" problem entirely. Do it in v1 even if v1 is LAN-only.

**4. Pairing — one offer blob with an optional relay slot from day one.**
- `{v, endpoint(s), deviceToken, hostPublicKey, scope, relay?}`, base64url JSON, delivered as a QR **and** a custom-scheme deep link **and** a pasteable code (three ways in, one parser) `[fact]`
- **multi-endpoint candidates** tagged `lan | tailscale | relay`, raced at connect time `[fact]`
- bounded invite TTL with explicit clock-skew tolerance `[fact]`
- **decision for us:** reserve the optional relay field in the offer schema in v1. If we ship a LAN-only offer with no room for it, every device paired before the relay lands has to re-pair.

**5. Idempotency — the expensive retrofit.**
- every mutation takes a **client operation id** and an **expected revision/fence**; the daemon keeps tombstones long enough to answer a retry; the reply distinguishes accepted / rejected / unknown `[fact]`
- **decision for us:** a phone on cellular will lose replies. Without this, "send follow-up prompt" and "approve permission" double-fire, which on an agent that is about to run a command is a correctness bug, not a UX bug. Build it into the daemon's mutation envelope now.

**6. Status feed — one subscription plus a catch-up RPC.**
- a notification/status stream carrying `{notificationSeq, notificationEpoch, source, agentState, title, body, worktreeId, notificationId}` `[fact]`
- a `getMissedSince(lastSeenSeq, epoch)` RPC over a **bounded retained buffer**, which cuts at `seq > lastSeenSeq` and returns the whole buffer when the epoch no longer matches `[fact]`
- agent state collapsed to a tiny closed set at the boundary (`needs-input | finished`) so clients never re-derive it `[fact]`
- **decision for us:** the `epoch` is the non-obvious part. Without a counter-lifetime id, a daemon restart makes every phone's watermark a lie and you cannot tell "nothing missed" from "I no longer have that range."

**7. Push — a separate service the daemon authenticates to.**
- host keypair → challenge/response → 24 h bearer session; the daemon registers each phone's native token and owns retirement; **the phone holds no cloud credential** `[fact]`
- the daemon needs: a durable registration store that survives restart, a durable unregister outbox, a per-device+scope burst cooldown, and dead-token handling `[fact]`
- **decision for us:** this is additive and can come after v1, *provided* the daemon already has the status feed from (6) and a place to persist a `pushRegistration` per device. Reserve that field.

**8. Do not build a relay in v1.**
- LAN + Tailscale covers the realistic case (the reference's own iOS config carries explicit ATS exceptions for the Tailscale v4 and v6 ranges, i.e. they expect that path to be used) `[fact]`
- the relay is a product: 18.5k LOC, a director/cell split, an assignment epoch, cell migration, a splice state machine, invite and resume credentials with rotation, Terraform, and an ops console `[fact]`

### B. Minimal v1 mobile feature set

```
┌─ Tier 0 (ship or it is not a companion) ──────────────────────────────┐
│ 1. Pair via QR + deep link + paste; multi-host list                   │
│ 2. Per-host session list with live agent status                       │
│ 3. Session view: transcript + send follow-up prompt                   │
│ 4. Approve / deny a permission prompt; answer a question prompt       │
│ 5. Cancel a turn                                                      │
│ 6. Terminal: attach, snapshot replay, live tail (read-only is fine)   │
│ 7. Notifications: agent finished / needs input, with tap-to-session   │
│ 8. Connection log + troubleshoot screen                               │
└───────────────────────────────────────────────────────────────────────┘
┌─ Tier 1 (next) ───────────────────────────────────────────────────────┐
│  interactive terminal input · git status + diff (read-only)           │
│  create a worktree · remote push notifications                        │
└───────────────────────────────────────────────────────────────────────┘
┌─ Defer (the reference's 33.8k LOC of optional surface) ───────────────┐
│  issue-tracker/PR hub · remote browser screencast · dictation         │
│  file explorer · PR review actions · stats · agent history            │
└───────────────────────────────────────────────────────────────────────┘
```

Items 3-5 are the ones that make a phone worth opening: a user who gets a "needs input" banner must be able to answer it in two taps. Item 6 read-only is defensible for v1; the reference itself shipped a buffered command box before direct input `[fact]`.

### C. Framework: Expo / React Native, not Tauri mobile, not native

**Recommendation: Expo (React Native), with the terminal as xterm in a webview.** `[inference]`

| Option | For | Against | Verdict |
|---|---|---|---|
| **Expo / RN** | one codebase for iOS + Android; `expo-secure-store`, `expo-camera`, `expo-notifications`, `expo-crypto` cover keychain, QR, banners, and a real CSPRNG out of the box; xterm-in-webview is a proven terminal path; the reference proves the whole design works on this stack at 132k LOC; our SolidJS desktop skills transfer as *JSX and reactive-UI* skills even though components do not | a second language runtime in the org; Hermes gaps are real (no `crypto.getRandomValues`, no `Buffer`, `instanceof` across the bridge) and cost the reference three documented workarounds; native module for audio if we ever do dictation; iOS release needs a Mac runner with a current Xcode | **Pick this** |
| **Tauri mobile (v2)** | would let us reuse SolidJS components and keep one Rust core in-process | phone UX is not desktop UX, so the reusable fraction is small; the mobile plugin ecosystem has no equivalent of expo-notifications / expo-secure-store / expo-camera maturity, so we would write those bindings; a phone does **not** want the Rust core in-process — it wants a thin client talking to the core on the laptop, which is exactly the thing Tauri mobile does not help with; and it puts our mobile releases behind Tauri's mobile maturity | **No** |
| **Swift + Kotlin native** | best platform fit; could share Swift model types with the existing macOS fleet app | two UIs to build and maintain for a surface the reference needed 132k LOC to fill; sharing with the fleet app only pays off if we ship iOS-only, and the reference's own Android workflow exists precisely because Android ships independently of App Store review | **No**, unless we consciously decide iOS-only forever |

The existing Swift macOS fleet app is the one real argument for native, and it is weaker than it looks: it shares *models*, not screens, and the moment we want Android the saving inverts. `[inference]`

Two consequences of picking Expo that we must plan for `[inference]`:

1. **No TypeScript sharing with a Rust core.** The reference's 604 shared-module import sites are its cheapest win and our unavailable one. Replace it with generated bindings: define the protocol once in Rust, emit TypeScript types (and ideally the runtime validators for the *phone* side only) into a published internal package. Then port the reference's best idea — the build-enforced boundary that keeps coercing validators out of the client bundle — as a check that the phone never hand-writes a wire type.
2. **iOS release needs the current Xcode on a hosted Mac runner.** The reference is pinned to `macos-26` / Xcode 26.5 purely because Expo 55's `expo-modules-core` uses Swift 6 syntax `[fact]`. Budget for that runner and expect the pin to move every SDK bump.

### D. The three things that will paint us into a corner if we skip them now

1. **Per-device revocable tokens with a scope field.** Retrofitting this means re-pairing every device and auditing every handler.
2. **A client operation id + expected fence on every mutation.** Retrofitting this means either double-firing prompts on flaky networks or rewriting the mutation path.
3. **A sequenced status/notification feed with an epoch and a bounded retained buffer.** Retrofitting this means notifications that silently drop across daemon restarts, and no path to remote push at all.

Everything else in this report — relay, push gateway, browser screencast, tasks hub, dictation — is genuinely additive and can wait.

---

## Analysis summary

- **Confidence: High** on stack, transport, terminal, pairing, auth, shared-code mechanics, and the push gateway's contract and policy. Every claim in those sections is cited to a file and line I read.
- **Confidence: Medium** on the feature-surface table's "actions" column: it is built from the RPC method literals present in each directory, which is authoritative about what the code *can* call but not about which button is wired to which call.
- **% of query answered: ~95%.** All six numbered sub-questions are answered from source.

**What I could not verify**
- Whether any phone build outside this snapshot registers an APNs/FCM token. In this tree it demonstrably does not (§5.1), but absence in one commit is not proof about the shipped app.
- Runtime behaviour of anything: this is a static read. No app was built, no daemon was started, no test suite was run. Where I say "survives backgrounding" or "recovers from cell migration", that is the code's stated intent plus its test names, not an observed run.
- The desktop `notifications.subscribe` / `getMissedSince` handlers themselves. I read the phone's client of them and the contract, not the server implementations, so the retained-buffer bound is inferred from the client's comments rather than measured.
- The relay server internals. I read its contract, limits, splice state machine, and README; I did not read the 18.5k lines of `cloud/apps/relay/src`, since the brief scoped me to `mobile/`, `cloud/apps/push`, and `cloud/packages/push-contract`.
- Bundle size, cold-start time, and battery cost on a real device. Not measurable from source, and all three are the usual reasons a React Native companion app gets complaints.

**Open questions a maintainer must answer**
1. Is the phone's push-token registration intentionally absent, or is this snapshot mid-migration?
2. Is the duplicated pairing parser (`mobile/src/transport/pairing.ts` vs `src/shared/pairing.ts`) checked for divergence by anything other than the comment asking a human to keep them in sync?
3. What bounds the desktop's retained notification buffer, and what happens to a phone whose watermark falls off the end while the epoch still matches?
4. `MIN_COMPATIBLE_RUNTIME_CLIENT_VERSION` is 2 while the current version is 3 — what is the actual support window in wall-clock time for an app-store build that cannot be force-upgraded?
