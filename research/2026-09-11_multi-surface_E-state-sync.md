Mode: focused-query

# Reference Implementation Analysis: state sync, RPC contract, web mode, plugins

**Repo** local clone of the reference implementation (Electron + TypeScript desktop app for running many coding agents in parallel)
**Commit** `9aa0f7e77d366c23a3cc8de2da32ae550d397dc0` (`9aa0f7e7`, 2026-09-11 12:36:35 +0000, committed by a release bot)
**Research query** What is the single contract that lets the same UI code run as desktop, browser ("web mode"), and against a remote daemon, and how do they keep it from drifting?
**Analyzed** 2026-09-11
**Scale** 21,351 TS/TSX files; ~172,773 lines of non-test TS/TSX [fact, `find`/`wc`]

Every claim below is tagged `[fact]` (backed by a cited file:line or a command output in this session) or `[inference]`. Relative paths are all relative to the clone root.

---

## 0. The one-line answer

There is **no single contract**. There are **two stacked contracts plus one tunnel between them**, and the drift control differs at each layer.

```
┌────────────────────────────────────────────────────────────────────────────┐
│ LAYER 1 — the UI-facing contract: PreloadApi (a TypeScript type)           │
│           src/preload/api-types.ts:69  →  ~90 namespaces, one `window.api` │
│                                                                            │
│   The renderer NEVER speaks RPC or IPC directly. It calls window.api.*.     │
│   Three implementations satisfy the SAME TS type:                           │
└────────────────────────────────────────────────────────────────────────────┘
        │                         │                          │
        ▼                         ▼                          ▼
┌────────────────┐      ┌──────────────────┐       ┌───────────────────────┐
│ DESKTOP        │      │ WEB MODE         │       │ REMOTE (desktop UI    │
│ src/preload/   │      │ src/renderer/src │       │ pointed at a daemon)  │
│   api/*-bridge │      │   /web/preload-  │       │ same preload bridges, │
│   .ts (138 f.) │      │   api/* (50 f.)  │       │ routed by hostId      │
│                │      │                  │       │                       │
│ ipcRenderer    │      │ callRuntimeResult│       │ runtime.call(...)      │
│ .invoke(       │      │ ('git.status',…) │       │ → main → WS/relay      │
│  'git:status') │      │                  │       │                       │
└────────────────┘      └──────────────────┘       └───────────────────────┘
        │                         │                          │
        ▼                         ▼                          ▼
┌────────────────┐      ┌───────────────────────────────────────────────────┐
│ Electron IPC   │      │ LAYER 2 — the host-facing contract:               │
│ ~340 string    │      │   the RPC method registry, 610 methods, zod params│
│ channels.      │      │   src/main/runtime/rpc/methods/index.ts:52        │
│ UNTYPED,       │      │   TYPED at authoring, ERASED at the registry      │
│ UNVERSIONED,   │      │   VERSIONED by RUNTIME_PROTOCOL_VERSION = 3        │
│ NOT in the RPC │      │        + 65 named capability strings              │
│ catalog.       │      │   src/shared/protocol-version.ts:32               │
└────────────────┘      └───────────────────────────────────────────────────┘
                                          │
              ┌───────────────────────────┼───────────────────────────┐
              ▼                           ▼                           ▼
      ┌──────────────┐           ┌──────────────┐           ┌──────────────┐
      │ Unix socket  │           │ WebSocket    │           │ Relay        │
      │ (CLI)        │           │ (web/mobile/ │           │ (NAT-pierce, │
      │ 221 LOC      │           │  remote desk)│           │  E2EE)       │
      │              │           │ 355 LOC      │           │ 311 LOC      │
      └──────────────┘           └──────────────┘           └──────────────┘
              └───────────── same RpcRequest / RpcResponse envelope ────────┘
                             src/main/runtime/rpc/core.ts:23-54
```

**The tunnel** is `PreloadApi['runtime']`: `runtime.call({method, params})` and `runtime.subscribe({method, params}, cb)` (`src/preload/api/runtime-bridge.ts:18-43`). This one namespace is a generic, stringly-typed passthrough from the renderer to the RPC registry. Web mode is essentially "reimplement the other 89 namespaces of `PreloadApi` on top of that tunnel" [fact].

**The drift controls, ranked by how much they actually buy:**

| Control | What it catches | Strength |
|---|---|---|
| `satisfies PreloadApi['x']` on each bridge (79 of 138 preload files) | desktop bridge diverging from the shared type | Strong, compile-time |
| `PreloadApi` as the web impl's return type (`src/renderer/src/web/web-preload-api.ts:63`) | web impl diverging **in shape** | Medium (it is `Partial<PreloadApi>`, so omission is legal) |
| `withFallback` proxy (`.../web/preload-api/web-fallback-api.ts:3`) | nothing; it **hides** omissions at runtime | Negative for drift, positive for uptime |
| `verify:rpc-params-catalog` byte-diff gate | host registry vs the shared params catalog | Strong, but the catalog has **zero functional consumers** today |
| `RUNTIME_PROTOCOL_VERSION` + 65 capability strings | wire-level mixed-version skew | Strong, and documented as a rule set |
| `check:runtime-electron-ratchet` (baseline must stay **empty**) | the runtime becoming un-portable off Electron | Strong, and the best single idea in the repo |

---

## 1. The RPC method catalog

### 1.1 Shape

```
    AUTHORING                      REGISTRY                    DISPATCH
┌──────────────────┐         ┌────────────────────┐       ┌──────────────────┐
│ defineMethod({   │         │ buildRegistry()    │       │ dispatch(req)    │
│   name: 'git.—', │──erase──▶ ReadonlyMap<       │──get──▶ 1 parse params    │
│   params: Zod,   │  types  │   string,          │       │ 2 streaming? 400  │
│   handler        │         │   RpcAnyMethod>    │       │ 3 invoke          │
│ })               │         │ throws on dup name │       │ 4 wrap in envelope│
└──────────────────┘         └────────────────────┘       └──────────────────┘
   core.ts:136                  core.ts:235                  dispatcher.ts:55
```

| Property | Value | Evidence |
|---|---|---|
| Total methods in the registry | **610** | catalog entries 607 + 3 uncataloged, `src/shared/rpc-contract/rpc-params-catalog.generated.ts` [fact] |
| Streaming (subscription) methods | **16** | `grep defineStreamingMethod` across `src/main/runtime/rpc/methods/` [fact] |
| Unary methods | 594 | 610 − 16 [inference, arithmetic] |
| Methods with `params: null` (no args) | 41 | `': null'` count in the generated catalog [fact] |
| Methods whose schema cannot live in `shared` | 3 (`emulator.install`, `orchestration.send`, `orchestration.taskUpdate`) | `RPC_METHODS_WITHOUT_SHARED_PARAMS`, generated catalog [fact] |
| Params-schema modules in the shared contract dir | 83 files | `ls src/shared/rpc-contract/*.ts` [fact] |
| Method-implementation modules | 237 entries under `src/main/runtime/rpc/methods/` | `ls | wc -l` [fact] |

### 1.2 Naming: strict `namespace.verb`, biggest namespaces first

| Namespace | Methods | Namespace | Methods |
|---|---:|---|---:|
| `browser.` | 89 | `agentSession.` | 22 |
| `github.` | 50 | `gitlab.` | 21 |
| `orchestration.` | 41 | `repo.` | 19 |
| `linear.` | 41 | `emulator.` | 19 |
| `terminal.` | 36 | `worktree.` | 17 |
| `git.` | 35 | `skills.` | 15 |
| `files.` | 30 | `computer.` | 15 |
| `jira.` | 24 | `session.` | 13 |

[fact, derived by `grep -oE "^  '[a-zA-Z]+[._]"` on the generated catalog]

Two-level namespacing appears where a namespace got big: `session.tabs.subscribe`, `browser.clientHost.attach`, `host.wsl.isAvailable`. Subscriptions are named by convention, not by type: `*.subscribe`, `*.subscribeAll`, `*.watch`, `*.multiplex`, `*.screencast`, `*.getIssueStream` [fact, streaming-method name list below].

The 16 streaming methods:

| Method | Module |
|---|---|
| `terminal.subscribe` | `methods/terminal/terminal-subscribe-method.ts:13` |
| `terminal.multiplex` | `methods/terminal/terminal-multiplex-method.ts:15` |
| `agentSession.subscribe` | `methods/structured-agent-session.ts:269` |
| `agentSession.subscribeStatus` | `methods/structured-agent-session-status-stream.ts:45` |
| `session.tabs.subscribe` | `methods/session-tabs.ts:83` |
| `session.tabs.subscribeAll` | `methods/session-tabs.ts:174` |
| `files.watch` | `methods/files.ts:173` |
| `accounts.subscribe` | `methods/accounts.ts:108` |
| `notifications.subscribe` | `methods/notifications.ts:17` |
| `nativeChat.subscribe` | `methods/native-chat.ts:92` |
| `runtime.clientEvents.subscribe` | `methods/client-events.ts:9` |
| `browser.screencast` | `methods/browser-screencast.ts:9` |
| `browser.clientHost.attach` | `methods/browser-client-host.ts:20` |
| `network.browserTunnel` | `methods/browser-network-tunnel.ts:24` |
| `jira.getIssueStream` | `methods/jira.ts:101` |
| `jira.issueCommentsStream` | `methods/jira.ts:140` |

[fact]

**16 subscriptions carry 610 methods' worth of liveness.** Everything else is request/response plus a separate `on*` event on the preload API. That ratio is the single most transferable number in this report [inference].

### 1.3 Typing: preserved at authoring, erased at the boundary, on purpose

`defineMethod` is generic in the literal name, the zod schema, and the handler's return type (`src/main/runtime/rpc/core.ts:130-144`). `RpcParsedParams` maps a `null` schema to `void` so a no-arg handler cannot read its first argument (`core.ts:125-127`).

Then the types are deliberately thrown away exactly once:

```ts
// core.ts:221-227
// Why: the one place the parsed-params type is dropped — contravariance makes it uncastable by
// assignment, and the dispatcher only ever calls a handler with an already-parsed `unknown`.
export function eraseRpcMethods(methods: readonly RpcAnyMethodDeclaration[]): readonly RpcAnyMethod[] {
  return methods as readonly RpcAnyMethod[]
}
```

The erasure is itself type-tested: `src/main/runtime/rpc/core-typed-method-contract.test.ts:1-2` asserts with `expectTypeOf` that a name never widens to `string` and a result never widens to `unknown`, so the failure is at typecheck, not runtime [fact].

The envelope is plain and small (`core.ts:23-54`):

```ts
type RpcRequest  = { id, authToken, method, params?, orchestration*? }
type RpcSuccess  = { id, ok: true,  result, streaming?: true, _meta: { runtimeId } }
type RpcFailure  = { id, ok: false, error: { code, message, data? }, _meta }
```

`RpcContext` (`core.ts:64-120`) is the interesting part: 30+ optional fields, each with a `// Why:` comment, carrying connection identity (`connectionId`, `clientId`, `pairedDeviceId`), client class (`clientKind: 'mobile' | 'runtime'`), negotiated capabilities, an `AbortSignal` for long polls, and binary-frame escape hatches (`sendBinary`, `registerBinaryStreamHandler`). Authority rides in the authenticated envelope, never in user params: `// Why: Dispatch authority rides in the authenticated RPC envelope, never in user payload fields.` (`core.ts:82`) [fact].

### 1.4 Versioning: one integer plus 65 capability strings

`src/shared/protocol-version.ts:32-34`:

```ts
export const RUNTIME_PROTOCOL_VERSION = 3
export const MIN_COMPATIBLE_RUNTIME_CLIENT_VERSION = 2
export const MIN_COMPATIBLE_RUNTIME_SERVER_VERSION = 2
```

The file's header comment is the actual policy (`protocol-version.ts:15-31`):

| Bump the integer when | Do NOT bump when |
|---|---|
| removing a method or required param | adding a new method |
| changing the meaning (units, nullability) of a field clients read | adding an optional field |
| changing encrypted framing, terminal stream framing, or auth | adding an ignorable event type |

Everything additive is negotiated by **65 named capability constants** instead (`grep -c "^export const .*_CAPABILITY"` = 65 over 326 lines). They are strings with explicit versions: `'worktree.linked-work-item-context.v1'`, `'orchestration.federation.v1'`, `'browser.headless.v1'`, `'aiVault.session-titles.v1'`. Two flavours, distinguished in comments:

- **static**: registered unconditionally, advertised automatically by `getStatus()` (e.g. `AI_VAULT_RUNTIME_CAPABILITY`, `protocol-version.ts` comment: "Registered unconditionally for every build, so it is a STATIC capability")
- **dynamic**: advertised only when the backing facility exists (`BROWSER_HEADLESS_RUNTIME_CAPABILITY`: "Advertised only when that backend is actually available")

Capabilities flow **both** ways. Client → server is `remoteRuntimeClientCapabilities()` (`src/shared/remote-runtime-client-capabilities.ts:17-35`), a fixed list bound to the authenticated socket. `RpcContext.updateClientCapabilities` exists so a capability upgrade "must mutate only the authenticated socket after auth" (`core.ts:80-81`) [fact].

The written rules live in `docs/reference/remote-wire-compatibility.md`. Its two load-bearing rules:

- **Rule 1**: a new optional JSON field is safe, because every decoder strips unknown keys (zod `.strip()` on params, `JSON.parse` on stream frames). But "the field is safe only for as long as every reader treats it as optional" (`remote-wire-compatibility.md:26-27`).
- **Rule 2**: a new binary stream opcode is **not** safe. `decodeTerminalStreamFrame` returns `null` for an unknown opcode and the frame is silently dropped, so "the sender never learns" and the feature appears to hang (`remote-wire-compatibility.md:31-43`). New opcodes must be announced in the subscribe handshake and echoed by the host before use; `SetOutputPaused` (opcode 16) is the named pattern. Opcode numbers are permanent even after the feature is removed [fact].

### 1.5 Transport abstraction: same contract, four transports

`src/main/runtime/rpc/transport.ts` is 22 lines and deliberately almost empty:

```ts
export type RpcTransport = { start(): Promise<void>; stop(): Promise<void> }
```

Its header comment explains the design: each transport owns its own connection lifecycle and **overrides `onMessage` with a richer signature** (Unix adds an abort signal; WebSocket adds the `ws` handle for auth association), and "consumers hold a concrete transport type, not `RpcTransport`, when they need those extensions" (`transport.ts:1-8`). So the shared interface is lifecycle-only; the message path is per-transport by design [fact].

| Transport | LOC | Used by | Notes |
|---|---:|---|---|
| `unix-socket-transport.ts` | 221 | the CLI | adds `RpcMessageContext { signal, startKeepalive }`; keepalive is opt-in per long-poll request so short RPCs pay no timer |
| `ws-transport.ts` | 355 | web mode, mobile, desktop→remote | per-device token auth, 1 MiB message cap, 128 WS / 256 TCP connection caps, 10 s pre-auth timeout, 15 s ping/pong sweep, and it **also serves the static web bundle** (`staticRoot`) |
| `relay-transport.ts` | 311 | NAT-pierced remote + mobile | E2EE (see `e2ee-channel-v2.test.ts`, `mobile-e2ee-v2-key-schedule.ts`) |
| in-process |, | desktop main | `clientKind` undefined ⇒ "treat as full-class (no clip)" (`core.ts:76`) |

The dispatcher is transport-blind. It has exactly two entry points, `dispatch()` and `dispatchStreaming(request, reply, options)` (`dispatcher.ts:55`, `:119`), and it refuses a streaming method arriving on the unary path with `method_not_supported` (`dispatcher.ts:77-84`) [fact]. Notably, `ws-transport.ts:41` carries a scar comment about pinned vs fallback ports: "devices paired while the fallback port was active point at it, so it must bind first on later launches or those pairings strand".

### 1.6 What `rpc-params-catalog` verifies: and the honest caveat

`config/scripts/generate-rpc-params-catalog.mjs` does something clever:

```
 1. glob every params module in src/shared/rpc-contract + every shared module
    the RPC dir imports                                        (:42-60)
 2. esbuild-bundle the registry AND those modules into ONE cjs bundle, so
    schema OBJECT IDENTITY is shared                           (:64-95)
 3. require() the bundle, walk ALL_RPC_METHODS, and look up each method's
    `params` object in an identity Map built from the modules' exports
                                                               (:99-113)
 4. emit `RPC_PARAMS_BY_METHOD` mapping 607 method names to their schema
    import, plus RPC_METHODS_WITHOUT_SHARED_PARAMS for the 3 that reach
    into src/main                                              (:180-207)
 5. run the emitted source through oxfmt so the drift gate can byte-compare
                                                               (:212-226)
 6. --check: byte-diff against the checked-in file, exit 1 on mismatch
                                                               (:228-247)
```

The identity trick is the point: `// Why: schema objects are compared by identity, not by shape — two structurally identical schemas are still two different wire contracts.` (`:97-98`) [fact].

The generated file's own header states the contract for clients (`rpc-params-catalog.generated.ts:185-190`):

> Why: the host parses params with these schemas, so a client that matches this map matches the dispatcher. **Clients must import it for types only**, parsing a params schema client-side runs the coercing transforms and rewrites the wire bytes.

And the type export is `z.output`, not `z.input`, with a stated reason: `requiredString` is `z.unknown().transform(...)`, so `z.input` would admit any value and lose optional/default semantics (`:200-206`) [fact].

**Caveat, and it matters.** The catalog currently has **no functional consumer**. A repo-wide grep for `RPC_PARAMS_BY_METHOD`, `RpcMethodName`, `RpcParams`, and `rpc-params-contract` returns only: the generated file, the generator, and one type-only re-export at `mobile/src/transport/rpc-params-contract.ts:5-8`, and nothing in `mobile/src` imports that re-export. The web client calls RPC with `method: string` and casts the result (`src/renderer/src/web/preload-api/web-runtime-calls.ts:29-47`, `:73-85`) [fact, confirmed by grep].

So `verify:rpc-params-catalog` is a real, cheap, non-drifting gate guarding a bridge nobody has driven across yet. It is infrastructure ahead of use. That is a reasonable bet, not a mistake, but do not copy it expecting type safety you are not yet consuming [inference].

---

## 2. Web mode

### 2.1 How the identical renderer runs in a browser

```
                     src/renderer/  (ONE renderer source tree)
                   ┌──────────────┴──────────────┐
    index.html ────┤                             ├──── web-index.html
    (electron-vite)│      src/renderer/src/      │     (vite.web.config.ts)
         │         │        App.tsx, store/,     │            │
         ▼         │        components/, lib/    │            ▼
   src/renderer/   └─────────────────────────────┘     src/renderer/src/
     src/main.tsx                                        web/main.tsx
         │                                                    │
         │ window.api injected by                             │ installWebPreloadApi()
         │ Electron contextBridge                             │ sets window.api =
         ▼                                                    ▼  withFallback(…)
   ┌──────────────┐                                   ┌──────────────────┐
   │ preload/     │                                   │ web/preload-api/ │
   │ index.ts:1   │                                   │ 50 modules       │
   │ contextBridge│                                   │ → callRuntime*   │
   └──────────────┘                                   └──────────────────┘
                                                              │
                                                              ▼
                                              WebRuntimeClient → encrypted WS
                                              web-runtime-client.ts:27
```

`vite.web.config.ts` sets `root: 'src/renderer'`, `input: 'src/renderer/web-index.html'`, `outDir: 'out/web'`, and crucially `base: './'` with the comment "pairing URLs may live under a reverse-proxy path prefix like /ref/web-index.html, so built assets must resolve relative to the page" [fact].

`src/renderer/src/web/main.tsx:67` is the whole switch: after a pairing environment exists, call `installWebPreloadApi()` and then lazily import the identical `../App`. Before that, it renders `WebConnect` for pairing. The dynamic import of `App` is deliberate, `main.tsx:98-104` notes the web entry "deliberately keeps the whole App graph — pane manager included — out of its own startup chunk" [fact].

`installWebPreloadApi()` (`web-preload-api.ts:55-61`) does three things: set `window.__ref_WEB_CLIENT__ = true`, stub `window.electron` entirely with a fallback proxy, and install `window.api`.

### 2.2 What differs, concretely

| Facility | Desktop | Web mode | Citation |
|---|---|---|---|
| **Auth** | implicit (same process, contextBridge) | pairing offer + `deviceToken`, then NaCl `box` E2EE over WS | `web/web-e2ee.ts:9-31`, `web-runtime-client.ts:175` |
| **Pairing entry** | n/a | token in the URL, auto-saved for runtime-scoped offers, then scrubbed from the address bar | `web/main.tsx:29-41`, `web-pairing.ts` |
| **File dialogs** | native `dialog.*` over IPC | every picker returns `null`: `pickAttachment`, `pickImage`, `pickAudio`, `pickDirectory`, `pickRepoIconImage` | `web/preload-api/web-shell-api.ts:24-28` |
| **Open in Finder / editor** | native | `{ ok: true }` lie, or `window.open(..., 'noopener,noreferrer')` | `web-shell-api.ts:8-16` |
| **Platform info** | real `os.*` | `{ platform: <from userAgent>, osRelease: '', arch: '', shell: '', displayServer: null }` | `web-platform-api.ts:6-13` |
| **Native menus / `window.electron`** | real | a fallback proxy for the whole object | `web-preload-api.ts:59` |
| **Memory profiler** | real heap stats | `createEmptyMemorySnapshot()` | `web-preload-api.ts:101-103` |
| **Plugins** | full (6 RPC methods + panel host) | **namespace absent** ⇒ falls through to the fallback proxy | `web-preload-api.ts:63-138` has no `plugins:` key [fact] |
| **Stats** | real | RPC call with a zeroed catch-fallback | `web-preload-api.ts:92-100` |
| **Static hosting** | n/a | the same WS server serves the bundle: allowlist is exactly `/web-index.html` + `/assets/*` | `runtime/rpc/static-web-client-handler.ts:6-7` |

### 2.3 The fallback proxy: uptime bought with a drift blind spot

`src/renderer/src/web/preload-api/web-fallback-api.ts` wraps the web `window.api` in a recursive `Proxy`. Any property that is **not** implemented returns another proxy; calling it returns a heuristic default chosen **by the method-name prefix** (`web-fallback-api.ts:39-63`):

| Name pattern | Fallback value |
|---|---|
| `on*` | `noopUnsubscribe` |
| `is*`, `has*`, `pathExists` | `Promise.resolve(false)` |
| `list*`, `detect*` | `Promise.resolve([])` |
| `preview*` | `Promise.resolve({ found: false, diff: {}, unsupportedKeys: [] })` |
| `get*Status` | `Promise.resolve([])` |
| `write`, `resize`, `reportGeometry` | `undefined` |
| `getZoomLevel`, `declarePendingPaneSerializer` (0 args) | `0` |
| everything else | `Promise.resolve(undefined)` |

**[Feathers] This is the repo's weakest seam.** It means a renderer feature can be added, work perfectly on desktop, and silently do nothing in web mode with no type error and no runtime error. The web impl's return type is `Partial<PreloadApi>` (`web-preload-api.ts:63`), so omission is legal by construction. The observable proof is the plugin namespace: web mode calls `plugins.list()`, the name starts with `list`, so it resolves `[]` and the UI renders "no plugins" rather than "plugins unsupported here", even though 6 `plugins.*` RPC methods exist on the host [fact, `web-preload-api.ts` + `methods/plugins.ts`].

**[Cunningham] intent → drift.** The intent is legible: web mode shipped later and incrementally, and the proxy let it boot before all ~90 namespaces existed. The drift is that nothing now measures the remaining gap. There is no "web parity" ratchet analogous to `check:runtime-electron-ratchet`, and no test enumerating which `PreloadApi` keys are unimplemented. The existing `web-runtime-client-export-parity.test.ts` pins only the 4-member public surface of `WebRuntimeClient` (`call | close | subscribe | statusOwner`), not API coverage [fact].

### 2.4 Per-runtime feature gating

Three distinct mechanisms, and they are not interchangeable:

1. **Capability strings** (see §1.4). The host advertises via `status.get`; clients must check before using a facility. 65 constants.
2. **Graceful degradation payloads.** `src/shared/runtime-capability-degradation.ts:32-44` defines `RuntimeDegradation { code, capability, message, reason?, detail? }`. The contract is explicit: `code` is an **open vocabulary**, so "clients must render `message` and must not switch exhaustively on this or the `reason` field" (`:33-36`). The reasons carry real operator text, e.g. `libc_floor`: "This host's node-pty binary was built against a newer C library than the host provides, so the dynamic loader refuses it" (`:51`) [fact].
3. **`check:runtime-electron-ratchet`**: the structural gate, below.

### 2.5 `check:runtime-electron-ratchet`: the best idea in the repo

`config/scripts/check-runtime-electron-ratchet.mjs` esbuild-bundles three entry points (`runtime/ref-runtime.ts`, `runtime/runtime-rpc.ts`, `refd/main.ts`), reads the **metafile**, and collects every module that imports `electron` or `electron/*`. It diffs that set against `config/runtime-electron-baseline.txt`. A new entry fails the build; a removed entry also fails, forcing the baseline to be re-tightened (`:140-163`).

The baseline file is **empty**, and says so:

```
# This list is EMPTY and must stay that way: the runtime boots on plain Node
# (see `pnpm run build:refd`). Any entry means the runtime got less portable;
# migrate the module behind a host port instead (src/main/host/).
```
[fact, `config/runtime-electron-baseline.txt`]

The script's own rationale names why a lint rule cannot do this job: "`ref-runtime.ts` reaches ~50 modules that import `electron`, and the number silently grows whenever someone adds an import several hops away, because no single reviewer sees the transitive edge. … This is a reachability check, not a lint rule: the point is precisely the edges that no per-file rule can see." (`:6-17`) [fact].

Two scars worth stealing: it marks all `.node` files external because optional native deps made the gate pass locally and hard-fail in CI (`:56-67`); and it compares `import.meta.url` to `pathToFileURL(process.argv[1]).href` rather than a `file://` template, because on Windows the template never matches and "the gate would exit 0 without checking anything — a lint gate that fails open" (`:168-171`) [fact].

`refd` (the Node-only daemon, `src/main/refd/`) is the payoff, and `smoke:serve-terminal` is its acceptance test: it boots the **built** server, pairs a real client over the advertised endpoint, creates a terminal, runs a command, and asserts the output returns. Its header explains why a boot probe is not enough, "a boot probe, a port bind, and a `host.platform` call all pass against a server whose terminals are dead. Only a PTY round trip catches it", and states that the same script must pass unchanged against `refd`, "because it drives nothing but the public pairing + RPC surface" (`config/scripts/runtime-serve-terminal-smoke.mjs:1-22`) [fact].

---

## 3. Renderer state model

### 3.1 Shape: one flat zustand store, 42 slices, spread-merged

```
┌─────────────────────────────────────────────────────────────────────┐
│ useAppStore = create<AppState>()(                                   │
│   withDevelopmentStoreProbes(        ← identity-churn probe, DEV    │
│     withReactCommitCascadeWriteProbe( ← always on, crash telemetry  │
│       (...a) => {                                                   │
│         installStoreListenerCensus(a[2])  ← counts live subscribers  │
│         return { ...createRepoSlice(...a),                          │
│                  ...createWorktreeSlice(...a),                      │
│                  ... 40 more ... }        ← ONE FLAT OBJECT         │
│ ))))                                                                │
└─────────────────────────────────────────────────────────────────────┘
     AppState = RepoSlice & SparsePresetsSlice & … (42-way intersection)
                        store/types.ts:47-89
```

| Measure | Value | Evidence |
|---|---:|---|
| Slices merged into one store | 42 | `store/index.ts:75-119` [fact] |
| Files under `store/slices/` | 492 | `ls | wc -l` [fact] |
| Files at `store/` top level | 45 | `ls | wc -l` [fact] |
| Top-level `store/*.ts` LOC | 4,882 | `wc -l` [fact] |
| Separate ancillary stores | at least 1 (`plugin-panels.ts:2` calls `create` again) | [fact] |

`AppState` is a 42-way TypeScript intersection (`store/types.ts:47-89`). There is no section/version concept. `store/index.ts:125-141` registers two memory-profile contributors that name the fattest slices in crash breadcrumbs, with a scar attached: "counts miss value-weight growth (97b9e86d leaked ~700MB while its biggest slice grew by 4 entries); sampled KB names what got FAT" [fact].

### 3.2 Mirrored vs fetched: both, chosen per domain

There is no uniform sync model. 144 distinct `api.<ns>.on*(` subscription call sites exist in the renderer [fact, grep]. Two patterns coexist:

```
 A. PUSH-SNAPSHOT (mirror a whole section)
    main ──keybindings:changed──▶ preload ──▶ setKeybindingSnapshot(snapshot)
    hooks/ipc-events/settings-sidebar-ipc-bridge.ts:140-142

 B. INVALIDATE-AND-REFETCH
    main ──repos:changed──▶ preload ──▶ await state.fetchRepos(localOwner)
    hooks/ipc-events/project-catalog-ipc-bridge.ts:12-18
```

Pattern A is used where the payload is small and self-contained (the keybindings file snapshot, agent-status deltas). Pattern B is used where the state is large or host-scoped, and the repos bridge shows why the choice is not free: under an active runtime environment it refreshes only the **local** slice and keeps runtime slices, because "the all-host sidebar shows local repos even under a runtime" (`project-catalog-ipc-bridge.ts:15`) [fact]. 54 files live under `hooks/ipc-events/` doing this wiring [fact].

**[Cunningham] The absence of a uniform model is the debt, and it is load-bearing debt.** The reference pays for 54 hand-written bridge modules and gets, in exchange, the freedom to pick push-vs-pull per domain. Our plan pays one mirror mechanism and gives up that freedom. §3.3 is the number that decides whether that trade is affordable.

### 3.3 Selector fan-out: the benchmark, and why it exists

`config/scripts/zustand-selector-fanout-benchmark.mjs` models the one structural cost of a single flat store: **zustand notifies every subscriber synchronously on every write, regardless of what changed.**

```
     one unrelated setState()
              │
              ▼
   ┌────────────────────────┐
   │ zustand iterates its   │
   │ listener Set directly  │
   └────────────────────────┘
              │
     ┌────────┴────────┬─────────────┬──────── … 2,500 of them
     ▼                 ▼             ▼
  selector run     selector run   selector run    ← ALWAYS
     │                 │             │
     ▼                 ▼             ▼
  Object.is same?   same?         same?           ← 0 re-renders if stable
```

What it asserts (`:6-10`, `:58-81`):

| Knob | Default | Meaning |
|---|---:|---|
| `SUBSCRIBERS` | 2,500 | simulated mounted subscriptions |
| `WRITES` | 2,000 | unrelated store writes |
| `MAX_MILLISECONDS_PER_WRITE` | 5 | the CI ceiling under `--check` |

Three hard assertions, and the second is the important one:

1. observed selector runs **must equal** `SUBSCRIBERS × WRITES` = 5,000,000, else "update the fan-out model", i.e. the benchmark refuses to silently benefit from a zustand internals change (`:58-62`);
2. `renderInvalidations` **must be 0**, a reference-stable projection must survive unrelated writes (`:63-67`);
3. median ms/write ≤ 5 under `--check` (`:76-81`).

It takes the **median of 5 rounds after a warm-up round** (`:50-54`), which is the right statistic for a JIT'd microbenchmark [inference].

### 3.4 The production version of that cost

`docs/reference/renderer-agent-status-performance.md` is the real-world instance, and it is the most decision-relevant document in the clone. The cost model it states (`:27-29`):

```text
burst work ~= status events x store listeners x selector work
```

Measured baseline on `main` at `077f5a11cd4` (macOS arm64, 16 CPUs, headless Electron, 100 worktrees at lineage depth 99, 100 seeded agent rows, `:237-255`):

| Measure | Value |
|---|---:|
| Mounted worktree cards | 100 |
| **Live store listeners** | **9,279** |
| No-op publications requested over 2 s | 2,000 |
| Median wall time to drain them | 12,325.7 ms |
| Sustained publication rate | ~160 / s |
| p50 / p95 scheduling drift | 5,150.3 ms / 9,802.9 ms |
| Renderer mean / p95 CPU | 18.25% / 32.59% |
| Idle control at same scale (0 publications) | 6.63% mean CPU |
| **CPU attributable to publication fan-out alone** | **~11.6 points of mean renderer CPU** |
| Long tasks recorded | **zero, in all three runs** |

The zero-long-tasks finding is the trap: "Each publication is individually short … so the cost surfaces as scheduling drift and sustained CPU rather than as discrete long tasks. **Compare drift and CPU here, not long-task counts.**" (`:256-261`) [fact].

The fix is **batching the writes, not restructuring the store**: fold a burst of status events in event order inside one zustand transaction, then publish once (`:110-131`). Results (`:280-296`):

| Single 2,000-update burst | Sequential | Batched | Delta |
|---|---:|---:|---:|
| Status-state publications | 2,000 | **1** | −99.95% |
| Store action time | 3,692.0 ms | **188.7 ms** | −94.9% |
| Update throughput | 541.7/s | **10,598.8/s** | ×19.6 |
| Renderer mean CPU | 36.2% | **2.9%** | −92.0% |
| Renderer p95 CPU | 107.3% | **8.2%** | −92.4% |
| p95 long task | 4,653 ms | **216 ms** | −95.4% |

Two structural insights in that document generalise beyond zustand:

- **Virtualizing a root row does not virtualize its descendants.** A 100-worktree lineage mounts 100 cards at once (`:17-20`). The listener count is a property of the data shape, not of the viewport [fact].
- **Effects must run after the transaction commits, never inside the updater.** "Invoking store actions from inside a Zustand updater would re-enter the store, while running an effect before the commit would let it observe stale state." (`:154-156`) [fact].

The listener census that produced "9,279" is itself worth copying. `store/store-listener-census.ts:1-13` patches `api.subscribe` from **inside** the state creator, because patching the bound hook afterwards counts only the 16 imperative `useAppStore.subscribe()` callers and "silently misses every React hook subscription (~2.2k sites) — exactly the ones that scale with agent rows". Cost is per-mount, never per-`setState` [fact].

---

## 4. Keybindings: keymap as data, and it works

```
 KEYBINDING_DEFINITIONS (data)          user file (data)
 ┌──────────────────────────┐      ┌────────────────────────┐
 │ id, title, group, scope, │      │ overrides,             │
 │ searchKeywords,          │      │ commonOverrides,       │
 │ defaultBindings{darwin,  │      │ platformOverrides{…},  │
 │   linux, win32},         │      │ diagnostics[]          │
 │ allowInTerminal?,        │      └────────────────────────┘
 │ allowBareKeybindings?,   │                 │
 │ allowShiftOnly…?,        │                 │
 │ conflictGroup?           │                 │
 └──────────────────────────┘                 │
            └──────────────┬───────────────────┘
                           ▼
        getEffectiveKeybindingsForAction(id, platform, overrides)
                    effective.ts:27-55
                           │
                           ▼
        keybindingIsActiveInContext(def, { context, terminalShortcutPolicy })
                    effective.ts:84-96
                           │
                           ▼
        keybindingMatchesInput(binding, input, platform)  matching.ts:19-40
```

| Property | Value | Evidence |
|---|---|---|
| Module LOC | 2,575 over 15 files | `wc -l src/shared/keybindings/*.ts` [fact] |
| Core action definitions | 88 (25 + 28 + 30 + 5 across 4 definition files) | `grep -c "id: '"` [fact] |
| Plus generated per-agent actions | one `tab.newAgent.<agent>` per agent in `ALL_TUI_AGENTS`, all with **empty** default bindings | `definitions.ts:18-34` [fact] |
| Scopes | 8: `global, tabs, terminal, browser, editor, fileExplorer, composer, settings` | `types.ts:3-12` [fact] |
| Contexts (runtime focus) | 3: `app, terminal, browser` | `types.ts:13` [fact] |
| Plugin action ids | `` `plugin:${string}` `` template literal in the union | `types.ts:25`, `:117` [fact] |
| Location | `src/shared/` ⇒ main, renderer, web and CLI share it | [fact] |

**Scope vs context is the key distinction.** `KeybindingScope` is an authoring-time grouping (8 values, used for UI grouping and terminal-conflict classification). `KeybindingContext` is the runtime focus target (3 values). They are separate types and only `context` participates in matching (`effective.ts:84-96`) [fact].

### 4.1 Overrides

`KeybindingOverrides = Partial<Record<KeybindingActionId, string[]>>` (`types.ts:119`). The file snapshot carries three override layers plus diagnostics (`types.ts:128-136`): `overrides` (effective), `commonOverrides`, and `platformOverrides` keyed by platform. `diagnostics: { severity, message, actionId?, section? }[]` means a malformed user file **loads with warnings** rather than failing closed [fact].

Override resolution is replace-not-merge: an override array wins entirely over the defaults; a per-binding normalization failure drops only that binding via `flatMap` (`effective.ts:46-52`). Digit-index actions get canonicalized to `<mods>+1` so "display/conflict stay consistent even if a hand-edited file stored a different digit" (`effective.ts:36-45`) [fact].

### 4.2 Terminal-focus conflict: the part most relevant to us

Two policies, one default (`effective.ts:70-96`):

| Policy | Behaviour in a focused terminal |
|---|---|
| `ref-first` (**default**) | every app shortcut still fires inside terminals |
| `terminal-first` | only definitions with `scope === 'terminal'` or `allowInTerminal === true` fire; the rest pass through to the shell/TUI |

```ts
// effective.ts:76-82
export function isKeybindingAllowedInTerminal(def) {
  return def.scope === 'terminal' || def.allowInTerminal === true
}
export function isKeybindingPotentialTerminalConflict(def) {
  return def.scope !== 'terminal' && def.allowInTerminal !== true
}
```

The second predicate is the interesting one: the app can **enumerate its own conflict surface** and show the user which shortcuts a terminal would swallow, rather than discovering it as a bug report [fact].

### 4.3 Conflict detection

Conflicts are computed on a **platform-resolved canonical identity**, not on the authored string (`matching.ts:42-63`): `Mod` is resolved to the physical modifier for the platform, modifiers are emitted in fixed `Meta+Control+Alt+Shift` order, and double-tap bindings get their own namespace `DoubleTap:<Modifier>`. Digit-index actions expand to all 9 digits so `Mod+1` and `Mod+5` collide correctly (`matching.ts:65-81`). `conflictGroup` on a definition (`types.ts:154`) lets intentional overlaps be declared rather than reported [fact].

Double-tap is modelled as a first-class input, not a hack: `KeybindingInput.doubleTapModifier` is "Set only by the double-tap detector; always a physical token (never 'Mod')" (`types.ts:171-172`), and matching is strictly exclusive, a double-tap binding matches only a double-tap input and vice versa (`matching.ts:28-37`) [fact].

---

## 5. Plugins: a separately versioned facade, not a state topic

```
┌──────────────────────────────────────────────────────────────────────────┐
│ ref-plugin.json  (manifest v1, zod-validated in src/shared)             │
│   id, publisher, name, version, engines.ref ">=x.y.z", pluginApi: 1,    │
│   main: "main.mjs",                                                      │
│   contributes: { panels[], commands[], events[], keybindings[],          │
│                  agentProfiles[], languagePacks[], vmRecipes[] }         │
│   capabilities: [{ kind }]   ← closed set of 7                           │
└──────────────────────────────────────────────────────────────────────────┘
        │                                          │
        ▼  PANEL (renderer)                        ▼  WORKER (host)
┌────────────────────────────────┐        ┌──────────────────────────────┐
│ sandboxed iframe, srcdoc       │        │ main.mjs in a worker host     │
│ HOST-PREPENDED CSP:            │        │                               │
│  default-src 'none'            │        │ may call ALL 13 methods       │
│  connect-src 'none'  ← no exfil│        │                               │
│  img-src data:                 │        │                               │
│ + 20 allowlisted design tokens │        │                               │
│ + inline ping/pong watchdog    │        │                               │
└────────────────────────────────┘        └──────────────────────────────┘
        │  postMessage bridge                      │  direct
        │  only methods with panel:true (3 of 13)  │
        └──────────────────┬───────────────────────┘
                           ▼
        ┌──────────────────────────────────────────────────┐
        │ PLUGIN_HOST_API_V0 — 13 methods, params AND      │
        │ result schemas, capability + scope + mutation    │
        │ + panel flag per method                          │
        │ shared/plugins/plugin-host-api.ts:122            │
        └──────────────────────────────────────────────────┘
                           ▼
        ┌──────────────────────────────────────────────────┐
        │ capability gate → runtime services               │
        │ THE 610-METHOD RPC REGISTRY IS NEVER EXPOSED     │
        └──────────────────────────────────────────────────┘
```

### 5.1 The API surface: 13 methods

`src/shared/plugins/plugin-host-api.ts:122-250`:

| Method | Capability | Scope | Mutation | Panel-callable |
|---|---|---|---|---|
| `workspace.readContext` | `workspace:read` | active-worktree | no | **yes** |
| `terminal.sendText` | `terminal:send` | explicit-terminal | yes | **yes** |
| `notifications.show` | `notifications:show` | desktop | yes | **yes** |
| `storage.get` / `.set` / `.delete` / `.keys` | `storage` | plugin-private | no/yes/yes/no | no |
| `secrets.get` / `.set` / `.delete` | `secrets` | plugin-private | no/yes/yes | no |
| `settings.get` / `.set` | `settings:own` | plugin-private | no/yes | no |
| `events.subscribe` | `events:subscribe` | host-events | no | no |

The table is explicitly the single source of truth, and the panel action list is **derived** from it so the two cannot diverge (`plugin-host-api.ts:260-265`):

```ts
/** Derived from the spec table so the panel surface can never drift from the gate. */
export const PLUGIN_PANEL_ACTIONS = PLUGIN_HOST_API_V0.filter((e) => e.panel).map((e) => e.name)
```

The header states the versioning boundary plainly (`:5-17`): "the separately-versioned public facade plugins call. … **The raw runtime-RPC registry is never exposed: its methods have no result schemas and evolve at internal velocity.**" And "Electron-free by design: desktop main, headless serve, the relay conformance path, and tests all import it." [fact]

Note what is **not** there: no UI-state read, no store access, no arbitrary RPC, no filesystem, no network. Every method carries a **result** schema as well as a params schema, unlike the 610 internal RPC methods, which have params schemas only (`:9-10`) [fact].

### 5.2 Capability model

7 capability kinds in a closed set (`plugin-capabilities.ts:15-23`), each with verbatim user-facing consent copy (`:33-43`). Two design notes worth stealing:

- **A closed set, so a typo fails validation.** "v0 is a closed set of unscoped kinds so a typo (or a capability from a newer ref) fails manifest validation instead of silently granting nothing." (`:9-11`) [fact]
- **An object, not a bare enum,** "so scoped fields can be added per-kind later without changing the manifest shape", `net:fetch` hosts and `process:exec` globs are the named future (`:27-28`, `:12`) [fact]
- **Consent is fingerprinted over a canonical serialization** that is order- and duplicate-insensitive and key-sorted, "so consent is stable across manifest reformatting" and "future scoped fields cannot produce two encodings of the same grant" (`:45-58`) [fact]

`terminal.sendText` requires an explicit `terminalId` and the schema comment states the rule: "Never 'the active terminal': a focus change must not redirect a delayed plugin write into another pane" (`plugin-host-api.ts:46-47`) [fact]. That is a real capability-confusion bug class, closed by schema.

Storage is capped to keep it honest: 256 KiB/value, 5 MiB total, 1,024 keys, and `__proto__`/`prototype`/`constructor` are reserved keys (`:61-79`) [fact].

### 5.3 Sandboxing

`plugin-panel-shell.ts:1-19` is the panel isolation, and its reasoning is the kind of thing that only comes from being burned:

> The shell's job is to make the CSP parse BEFORE any plugin content does: an opaque-origin sandboxed iframe without a CSP can still `fetch()` CORS-permissive endpoints and beacon data out via `<img>`. Prepending works because (a) a CSP `<meta>` applies from the moment it parses and cannot be un-applied by later markup or DOM removal, and (b) a second CSP meta from the plugin can only intersect (tighten), never loosen.

The CSP is `default-src 'none'; connect-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; base-uri 'none'; form-action 'none'` (`:20-22`) [fact]. Inline script is allowed; **all** network is not.

Theming is an explicit allowlist of 20 CSS custom properties, "Deliberately NOT all of main.css (~257 custom properties): freezing every token as public API would lock future refactors. Grow additively; renaming or dropping an entry here is a plugin-facing breaking change." (`:30-56`) [fact].

Liveness: an inline ping responder is injected so "the renderer watchdog demotes the panel when pongs stop arriving" (`:58-60`), with a message budget and call-admission layer alongside (`plugin-panel-message-budget.ts`, `plugin-panel-call-admission.ts`) [fact]. There is a `plugin-hostile-fixture.test.ts` and an `examples/plugins/hostile-panel/` adversarial example [fact].

Also present: a `plugin-kill-list.ts` (remote disable), `plugin-install-lockfile.ts`, `plugin-path-safety.ts` (every declared artifact must stay inside the plugin root; "realpath containment separately rejects symlink escapes when the artifact is read", `plugin-manifest-fields.ts:11-12`), and `plugin-marketplace.ts` [fact].

### 5.4 Reach to web and mobile: aspirational, not shipped

| Surface | Plugin reach today |
|---|---|
| Desktop renderer | full: 6 RPC methods + panel host + `store/plugin-panels.ts` + right-sidebar route |
| Headless serve / relay | the shared modules are Electron-free and validate identically **by design** (`plugin-manifest.ts:25-27`) |
| **Web mode** | **none.** `web-preload-api.ts` has no `plugins:` key ⇒ fallback proxy ⇒ `plugins.list()` returns `[]` [fact] |
| **Mobile** | no plugin references found under `mobile/src` [fact, grep] |

`plugin-manifest.ts:28-29` is candid: "Everything here is EXPERIMENTAL: no compatibility promises until pluginApi v1 freezes." The `manifestVersion: 1` / `pluginApi: 1` split is deliberate, manifest format and API surface version independently [fact, `examples/plugins/hello-ref/ref-plugin.json`].

Plugin panels live in their **own** zustand store (`store/plugin-panels.ts:2` calls `create` a second time), with bounded retry (2 attempts, 250 ms × 2^n backoff) and a per-panel health flag (`:28-44`). Keeping plugin state out of `AppState` means a misbehaving plugin cannot widen the 9,279-listener fan-out of the main store [inference, but the structural separation is fact].

---

## 6. Workspace / worktree / session / task model

### 6.1 The domain objects

```
 Project (projectId, durable)
   └── Repo (repoId, canonical-repo-key'd)
         └── Worktree  id = `${repoId}::${path}`         ← THE core object
               │  identity: WorktreeIdentity (immutable host/instance)
               │  hostId: ExecutionHostId   (local | wsl | ssh | runtime:<envId>)
               │  instanceId, projectHostSetupId, creatorProvenance
               │  linked*: GitHub PR/issue, GitLab MR, Linear, Jira, Bitbucket, …
               │  lineage: parentWorktreeId + origin + capture-confidence
               └── Session / Tabs / Panes
                     └── Terminal (paneKey, terminalHandle, PTY)
                           └── Agent (agent status, provider session)

 Orchestration overlay (independent id space):
   Run (durable namespace + coordinator inbox)
     └── Task (the work)
           └── Dispatch (ONE authoritative Task attempt)
                 └── Worker (an agent in a terminal in a worktree)
```

`Worktree` is a wide record (`src/shared/worktree/types.ts:60+`, 226 lines) with a striking amount of provenance: `identity`, `instanceId`, `creatorProvenance`, `displayNameMode`, `createdWithAgent`, `lineage`. Several fields carry migration comments ("Optional while legacy rows migrate"), which is honest about the schema being grown, not designed [fact].

Two design decisions in the type are worth noting:

- **Parallel link fields, not a discriminated union.** `linkedPR`, `linkedGitLabMR`, `linkedBitbucketPR`, `linkedAzureDevOpsPR`, `linkedGiteaPR` are separate optional fields. The stated reason: "so the persistence layer is unambiguous when a user has remotes from several providers on the same repo, and so the existing GitHub renderer code keeps reading linkedPR / linkedIssue unchanged" (`types.ts:85-96`) [fact]. This is explicitly a backward-compatibility choice over a cleaner model.
- **Git-level and app-level are separate types.** `GitWorktreeInfo` (`:22-38`) is what `git worktree list --porcelain` says, including `prunable` with a Git-version fallback ("Detected via the `prunable` porcelain field (Git ≥ 2.36) or a path-existence probe on older Git"). `Worktree` is the enriched app object. `WorktreeHeadIdentity` (`:44-50`) is a third, cheaper projection read "from Git metadata files without spawning Git" [fact].

`src/main/git/` is 197 entries and includes a capability-probing layer (`git-capability-state.ts`, `git-worktree-command-capabilities.ts`, `git-merge-tree-capability.ts`, `git-fetch-head-capability.ts`) plus read-leases (`git-status-read-lease-owner.ts`, `git-upstream-status-read-owner.ts`), i.e. it probes the local Git's feature set and serializes concurrent status reads [fact].

`src/shared/new-workspace/` (17 files) is the *creation* path, and it is mostly work-item resolution: `smart-workspace-source-query.ts`, `smart-workspace-linear-intent.ts`, `work-item-lookup-text.ts`, `worktree-create-retry-policy.ts`. A workspace is typically created **from** a ticket or URL, not from a branch name [fact, inferred from module names + `WorkspaceSource`].

`WorktreeLineage` (`lineage-types.ts:19-31`) records `parentWorktreeId`, `origin: 'orchestration' | 'cli' | 'manual'`, and a `capture: { source, confidence: 'explicit' | 'inferred' }`. Seven capture sources are enumerated, from `explicit-cli-flag` down to `active-workspace` (`:5-13`), with three warning codes for when parent context is missing, conflicting, or stale (`:47-56`). **The system records how confident it is about its own parentage inference.** That is unusual and good [fact].

### 6.2 The fan-out: it lives in a skill guide, not in app code

This is the finding most likely to surprise. There is **no** "fan one prompt across N agents and pick a winner" primitive in the codebase. Searches for `winner`, `compare`, `best of`, `variants`, `competitors`, and `parallelAgents` across `src/shared`, `src/main`, and `src/renderer` return nothing relevant [fact].

What exists instead: 41 `orchestration.*` RPC methods and a 201-line **prompt document** telling a coordinator agent how to use them. `skill-guides/orchestration.md:105-109` is the literal fan-out:

```text
ref orchestration run-create --objective "<objective>" --json
ref orchestration worker-start --spec "<worker A task>" --worktree current --agent codex --json
ref orchestration worker-start --spec "<worker B task>" --worktree current --agent claude --json
ref orchestration check --wait --types "worker_done,escalation,question" --timeout-ms 900000 --json
```

So: **fan-out = N sequential `workerStart` calls made by an LLM coordinator.** Convergence = the coordinator drains a FIFO inbox with `orchestration.check --wait` and decides. `orchestration.reply`, `orchestration.workerRelease`, and `orchestration.gateResolve` are the settlement verbs [fact].

Note `--worktree current`: two competing agents can share one worktree. Worktree-per-agent is the *default*, not an invariant [fact].

The authority model is where the engineering actually went (`skill-guides/orchestration.md:50-56`, `src/shared/orchestration-rpc-contract.ts`):

| Concept | Definition (verbatim where cited) |
|---|---|
| **Run** | "a durable namespace and coordinator inbox; it does not schedule or place workers" |
| **Task** | "work" |
| **Dispatch** | "**one** authoritative Task attempt" |
| Authority source | "the active Dispatch, not a terminal title, copied ID, old database row, provider transcript, or visible pane" |
| Liveness | layered: `worker-list.projection.liveness` is the fleet verdict; `worker-show.observation.status` is PTY liveness only. "A live terminal can still hold a dead or stuck agent." |
| Safe failure | "preserve work and authority and report the state as unknown or `unverifiable`. Only positive proof of exit authorizes stop, abandon, or retry" |

`isOrchestrationMutation()` (`orchestration-rpc-contract.ts:51-65`) classifies 22 methods as durable mutations, **plus two content-dependent cases**: `orchestration.check` is a mutation unless explicitly read-only or carrying an `ack`, and `orchestration.dispatch` is a mutation unless `dryRun` is true. `isTerminalPromptMutation()` (`:67-80`) additionally treats a `terminal.send` with `agentPrompt: true && enter: true && !interrupt && client.type === 'desktop'` as durable [fact].

That durability classification drives an idempotency ledger: `RpcContext` carries `recordMutationReceipt`, `replayedMutationReceipt`, `markMutationEffectPossible`, and `markWorkerDoneMutationEffectFree`, whose comments describe the exact hazard, "prompt receipts may retry only until the PTY write boundary makes effects ambiguous" (`core.ts:88-89`), and "a prompt retry with --wait-submit observes its durable receipt instead of writing again" (`core.ts:97-98`) [fact]. Supporting files: `orchestration-mutation-ledger.test.ts`, `orchestration-mutation-receipt.ts`, `orchestration-mutation-executor.ts`.

**[Cunningham] intent → drift, and this one is a genuine achievement rather than debt.** The original intent was presumably "let a coordinator agent start other agents". What it became is a distributed-systems authority protocol, fencing (`orchestration-contract-fence.ts`), FIFO delivery with acknowledgement, retry receipts, federation across daemons (13 `orchestration.federation*` methods), and a documented `live`/`unverifiable`/`exited` trichotomy where **contact loss is explicitly not process death**. The reference did not build a fan-out feature; it built the consistency substrate a fan-out feature needs, then wrote the fan-out as a prompt [inference, well supported].

Depth is bounded: "nested workers obey the depth limit, and a new Run does not reset the caller's depth" (`skill-guides/orchestration.md:117-118`), and the guide advises "prefer parallel waves over chains deeper than three or four steps" [fact].

---

## 7. Quality ratchets: all four, and which to copy

All four run on every `pnpm lint`, alongside 8 other verify steps (`package.json` `lint` script) [fact].

```
 pnpm lint
   ├─ oxlint
   ├─ audit:code-quality:native / :type-aware
   ├─ check:reliability-gates        ← manifest schema validator (121 gates)
   ├─ check:max-lines-ratchet        ← 11-line baseline, may only shrink
   ├─ check:ts-nocheck-ratchet       ← 176-line baseline, may only shrink
   ├─ check:runtime-electron-ratchet ← baseline MUST STAY EMPTY  ★ copy this
   ├─ verify:rpc-params-catalog      ← byte-diff a generated file  ★ copy this
   └─ 5 × verify:localization-*
```

| Gate | Mechanism | Baseline size | Verdict |
|---|---|---:|---|
| `check:runtime-electron-ratchet` | esbuild **metafile reachability** from 3 entry points; collect `electron` importers; diff vs baseline; both additions **and** removals fail | **0** (must stay empty) | **Copy it.** Transitive-edge gates catch what no per-file lint can see |
| `verify:rpc-params-catalog` | regenerate from the live registry via object identity, oxfmt, byte-compare | n/a | **Copy the pattern.** Generate-then-byte-diff is the cheapest possible drift gate |
| `check:max-lines-ratchet` | freeze the set of files carrying an `oxlint-disable max-lines` or a per-file `max` bump; may only shrink | 11 lines | Copy if you have a line-limit rule. Budgets: 300 general / 400 `.tsx` / 600 `.mjs` / 800 test |
| `check:ts-nocheck-ratchet` | freeze the set of files with a leading `@ts-nocheck`; may only shrink | 176 files | Situational. Exists because one PR split a **43,928-line class** into ~172 modules whose mixin chain cannot express forward references |
| `verify:renderer-boot-graph` | parse built `index.html`, sum the entry + `modulepreload` graph, and grep those chunks for forbidden payload **signatures** | 2 forbidden payloads | Copy the *idea*. It is a startup-cost gate, not a bundle-size gate |
| `check:zustand-selector-fanout` | the §3.3 benchmark, `--check` at ≤5 ms/write | n/a | **Not** in `lint`; run separately [fact] |
| `check:reliability-gates` | validates a 121-gate JSONC manifest: invariant, oracle, commands, testFiles, assertionRefs, evidenceRuns, maturity, coverage | 121 gates, **all 121 at `experimental`** | See below |

Three details that make these gates actually work rather than decorate:

1. **Removals fail too.** The electron ratchet errors on a *shrunk* set with "nice. Refresh the baseline so the gate keeps its new, tighter floor" (`check-runtime-electron-ratchet.mjs:153-160`). A ratchet that only blocks additions drifts upward forever as the floor is never re-tightened [fact].
2. **The gate must not fail open.** Both the electron and max-lines ratchets compare `import.meta.url` against `pathToFileURL(process.argv[1]).href` because on Windows a `file://` template never matches and "the gate would exit 0 without checking anything — a lint gate that fails open" (`:168-171`) [fact].
3. **The gate must not police itself.** Both ratchets exclude their own source and test from scanning, because those files contain the directive text as data (`check-max-lines-ratchet.mjs:18-23`) [fact].

`verify:renderer-boot-graph` deserves a note on technique: it identifies a forbidden payload by "a literal only that payload emits", and for the i18n catalog it **derives** that literal at check time, the longest English string present in the full catalog but absent from the runtime-required subset, "so the guard keeps working as copy changes" (`renderer-boot-graph.mjs:51-87`). The `bootGraphForbiddenPayloads` list also documents two deliberate *non*-entries: zod (six other boot-path modules still import it, "so there is nothing to ratchet yet") and emojibase-data (deferring it made a `:wink:` submitted during the load window persist literally) (`:31-50`) [fact].

On `reliability-gates.jsonc`: a 121-entry manifest where each gate names an invariant, an oracle, exact reproduction commands, test files, specific assertion strings, and dated evidence runs with runner/platform/result. Promotion to `blocking` requires 100 soak runs, 14 days, and 0 unexplained flakes (`:6-10`). **Every one of the 121 is still `experimental`**, and none is `blocking` [fact]. So it is a rigorous, well-specified test-provenance database that is not yet gating anything. Copy it only if you will actually promote gates; otherwise it is a documentation burden with a schema validator attached [inference].

---

## Adopt / Adapt / Avoid

**Adopt**

1. **The two-layer contract split.** A UI-facing typed facade (`PreloadApi`) over a transport-neutral host contract (the RPC registry), joined by one generic `call`/`subscribe` tunnel. The facade is what makes the renderer portable; the tunnel is what makes new host surfaces cheap.
2. **`check:runtime-electron-ratchet`, verbatim in spirit.** An esbuild-metafile reachability gate on a platform dependency, with an **empty** baseline and failures in both directions. For us, read: nothing reachable from the Rust-core-facing TS may import Tauri-only APIs. This is the highest-value 175 lines in the clone.
3. **Generate-then-byte-diff for any cross-boundary contract.** `generate-rpc-params-catalog.mjs` bundles the real registry and matches schemas **by object identity**, then byte-compares against the checked-in output after running the formatter. Our specta-generated types should get the same `--check` gate in CI.
4. **Additive capability strings + one integer protocol version,** with a written rule table for what does and does not bump the integer (`protocol-version.ts:15-31`), and the sharp companion rule that **new binary opcodes must be handshake-negotiated because unknown opcodes are silently dropped** (`remote-wire-compatibility.md` Rule 2).
5. **A subscriber census wired inside the store constructor.** `store-listener-census.ts` is ~50 lines and is the only reason "9,279 listeners" is a fact rather than a guess. Any mirror-to-renderer design needs this number from day one.
6. **The keybinding data model**, nearly as-is: scope (authoring) separate from context (runtime focus), three override layers, diagnostics instead of load failure, platform-resolved canonical conflict identity, and `isKeybindingPotentialTerminalConflict` so the app can enumerate its own conflict surface.
7. **Effects after commit, never inside the updater** (`renderer-agent-status-performance.md:154-156`). Applies to any mirror-apply function we write.
8. **Lineage with capture confidence.** Record *how* parentage was determined (`explicit` vs `inferred`, plus the source) and warn on conflict. Cheap; prevents a class of silent mis-attribution.

**Adapt**

9. **The plugin host API, but freeze it earlier.** 13 methods, params **and result** schemas, capability + scope + mutation + panel flag per entry, with the panel action list **derived** from the table. Adapt: our "plugins publish free-form state on a topic" has no equivalent of the `panel: boolean` column, i.e. no way to say "workers may do this, sandboxed UI may not". Add that column before shipping, not after. And keep the closed capability set with room for per-kind scoping.
10. **The panel sandbox.** Host-**prepended** CSP with `connect-src 'none'`, an explicit 20-token theme allowlist rather than all tokens, and a ping/pong watchdog. Adapt to whatever webview primitive we use; the prepend-not-append reasoning and the "second CSP can only tighten" property hold anywhere.
11. **Batching over restructuring.** Their answer to fan-out cost was one transaction per burst, not a different store. For us, that is one mirror-apply per channel drain, never one per section.
12. **Orchestration's authority model, shrunk.** Run / Task / Dispatch with "one authoritative attempt", `live`/`unverifiable`/`exited` as distinct verdicts, contact-loss ≠ death, and durable mutation receipts keyed to a retry-safety boundary. Adapt by taking the *vocabulary* and the retry-boundary idea while skipping federation until a second daemon exists.
13. **Max-lines / directive ratchets.** Only if you also commit to shrinking the baseline. A 176-entry `@ts-nocheck` baseline is a monument to a bad week, not a quality gate.

**Avoid**

14. **A prefix-heuristic fallback proxy.** `web-fallback-api.ts` turns every unimplemented API into a plausible lie chosen by whether the method name starts with `is`, `list`, or `on`. It bought web mode an early boot and cost the project any measure of how far web mode lags. If we need a degradation layer, make it **explicit per method** and **enumerable**, so a test can assert the gap and CI can ratchet it down.
15. **A single flat store with ~10^4 synchronous subscribers.** 9,279 listeners and 160 publications/second consuming ~11.6 points of renderer CPU is the documented cost. Do not reproduce the topology and hope.
16. **Two parallel contracts where one would do.** ~340 untyped, unversioned Electron IPC channel strings live beside 610 zod-validated RPC methods. The renderer's portability depends entirely on `PreloadApi` papering over that split. We have the chance to have exactly one host contract; take it.
17. **Building contract infrastructure with no consumer.** The params catalog is generated, formatted, byte-diffed in CI, and imported by nothing. Wire the first consumer in the same PR as the generator, or the gate guards a hypothesis.
18. **121 `experimental` reliability gates, 0 `blocking`.** A test-provenance manifest that never promotes is a second source of truth to maintain.

---

## Decision inputs for us

### Q1: Does a whole-state mirror (our plan B) or an RPC-catalog + subscription model scale better to remote + mobile + web?

**For remote and mobile, the RPC-catalog + narrow-subscription model wins clearly. For web, it is close to a tie. For the renderer's own CPU, our mirror is the riskier design and the reference has already paid for the measurement that proves it.**

The decisive evidence:

- **Fan-out is a property of the mirror, not of the transport.** `zustand-selector-fanout-benchmark.mjs:55` asserts selector runs `= SUBSCRIBERS × WRITES` as an invariant, i.e. every write costs every subscriber a selector run. Production numbers: 9,279 listeners, ~160 publications/s, ~11.6 points of mean renderer CPU purely from publication fan-out, with **zero** long tasks to warn you (`renderer-agent-status-performance.md:237-267`) [fact]. Whole-state mirroring maximises writes-per-unit-of-change, which is exactly the multiplicand you cannot reduce later without redesign.
- **Our 19 sections give us a lever the reference did not have.** Mirroring *whole sections* over a channel is not the same as mirroring whole state: if a channel frame names which sections changed and we apply one store transaction per drain, our publication count is bounded by `min(sections changed, 1 per drain)` rather than by event count. Their batching fix (2,000 publications → 1, 19.6× throughput, −92% CPU) is precisely this move, arrived at after the fact [fact + inference]. **Make section-scoped, once-per-drain application a day-one invariant, not an optimization.**
- **Remote and mobile are where the mirror breaks.** Their 16 streaming subscriptions carry liveness for 610 methods; everything else is request/response. A whole-state mirror inverts that: every remote client pays for every section's churn regardless of what it shows. Mobile is the sharp case, `RpcContext.clientKind` exists specifically so handlers "gate mobile payload truncation to phones only" (`core.ts:76`), and `mobile-e2ee-outbound-memory-budget.ts` exists because outbound payload volume to a phone is a budgeted resource [fact]. A phone showing one agent's status should not receive 19 sections. A section-subscription model (client declares which sections it wants) recovers most of the RPC model's selectivity while keeping our single mechanism.
- **On the other side of the ledger, the mirror wins on drift, decisively.** 610 methods needed a code generator, an esbuild identity-matching bundle, and a byte-diff CI gate, and still ended up with 3 methods whose schemas cannot live in the shared layer, plus a stringly-typed web client that does not use the generated types at all [fact]. Our specta-generated types over 19 sections are strictly less surface to keep in sync. Their 610-method catalog is also a 610-method API to version: 65 capability constants exist mostly to make additive change safe.

**Recommendation.** Keep the mirror, with two non-negotiable constraints borrowed from their measurements: (a) frames name changed sections and the renderer applies **one** store transaction per channel drain, effects strictly after commit; (b) ship a listener census and a fan-out benchmark with a CI ceiling **before** the second consumer surface exists. Add section-level subscription (client declares interest) as the mobile/remote escape hatch. Their §3.4 is the counterfactual we get for free: the mirror's failure mode is sustained CPU and scheduling drift with **no long tasks**, so a naive "is it janky?" check will not find it.

### Q2: What contract shape lets web and mobile reuse the desktop renderer?

**A single named TypeScript facade that the renderer is only allowed to call through, with one generic tunnel inside it, plus a mechanically enumerable list of what each host does not implement.**

What actually works in the reference [fact]:

1. **One import surface.** The renderer touches `window.api` and nothing else. Web mode swaps the implementation of that one object (`web/main.tsx:67`) and reuses `App.tsx`, the whole store, and every component unchanged. One vite config difference and one HTML entry point is the entire build divergence.
2. **A generic tunnel inside the facade.** `runtime.call({method, params})` / `runtime.subscribe(...)` means a new host capability needs no facade change. Web mode is built almost entirely on this one namespace.
3. **Host-shaped shims, not conditionals.** There is no `if (isWeb)` in the components. Web returns `null` from `pickDirectory` and `{platform, osRelease: '', arch: ''}` from `platform.get` (`web-shell-api.ts:24-28`, `web-platform-api.ts:6-13`). The renderer already had to handle those values; the shim keeps the branch out of the UI.
4. **Capability strings for what genuinely cannot be shimmed.** 65 of them, negotiated at connect, bound to the authenticated socket.

What to do differently:

5. **Make the unimplemented set explicit and ratcheted.** Their `Partial<PreloadApi>` + prefix-heuristic proxy makes the gap invisible and unmeasurable, the plugin namespace silently reports zero plugins in web mode [fact]. Our version: a single exported `UNSUPPORTED_ON<Host>` list, a test that asserts the facade's key set equals implemented ∪ unsupported, and a ratchet that only lets the unsupported list shrink. That is the missing sibling of their electron ratchet.
6. **Put the facade in shared, Electron-free.** Everything they got right structurally follows from `src/shared/` being importable by main, renderer, web, CLI, relay, and tests: keybindings, plugin manifest and host API, protocol version, worktree types. The plugin manifest header says it out loud, shared "so the desktop app, the headless `ref serve` runtime, the relay, and the CLI validate manifests identically (SSH/remote parity)" (`plugin-manifest.ts:25-27`) [fact].
7. **For us specifically:** the facade is the boundary at which the Rust core stops. Keymap-as-data, plugin manifests, and section schemas belong on the shared side of it so the daemon, the Tauri host, and a browser client validate identically. The `runtime.call` analogue is our channel: one send, one subscribe, and a section-scoped apply.

### Q3: What does the reference's worktree-per-agent model imply for our tmux-session-per-agent model?

**Worktree-per-agent is their default, not their invariant, and the hard part of their design is not the worktree at all, it is authority and liveness. Those port to tmux sessions directly, and they are what we would otherwise reinvent badly.**

Five concrete implications:

1. **Isolation unit and process unit are separate, and both are optional.** `worker-start --worktree current` puts two competing agents in one worktree (`skill-guides/orchestration.md:106-107`), and the guide states "Folder workspaces are valid; never require Git or assume a worktree" (`:63`) [fact]. Our tmux session is a process/isolation unit that gives us nothing filesystem-level. So we need the second axis explicitly: **(workspace, session)** as a pair, where the workspace may be a worktree, a plain folder, or shared. Do not let `tmux-session-id` become the de facto workspace identity.

2. **Liveness must be layered, and the layers must not be collapsed.** Their rule: `projection.liveness` is the fleet verdict for the *agent*; `observation.status` is PTY liveness only; "A live terminal can still hold a dead or stuck agent" (`:59-61`) [fact]. For us: `tmux has-session` is the *pane* verdict, and it is emphatically not the *agent* verdict. Model `live` / `unverifiable` / `exited` as three states from the start, and treat contact loss as `unverifiable` rather than `exited`. Their safe-failure rule is the one to copy verbatim: "Only positive proof of exit authorizes stop, abandon, or retry" (`:33-35`).

3. **Authority must come from a dispatch record, never from the session handle.** "Lifecycle authority comes from the active Dispatch, not a terminal title, copied ID, old database row, provider transcript, or visible pane" (`:52-53`) [fact]. A tmux session name is exactly the kind of reusable, guessable, copyable handle that rule exists to reject. We need a Dispatch-equivalent id, fenced (their `orchestration-contract-fence.ts`), with the session name as a *lookup key only*.

4. **Retry needs a write-boundary receipt.** Their sharpest detail: a prompt retry is safe only until the PTY write happens, after which effects are ambiguous, hence `markMutationEffectPossible` and `markWorkerDoneMutationEffectFree` (`core.ts:86-89`), and a durable receipt so "a prompt retry with --wait-submit observes its durable receipt instead of writing again" (`core.ts:97-98`) [fact]. `tmux send-keys` has the identical hazard and no natural idempotency. Build the receipt ledger with the send path, not after the first double-submitted prompt.

5. **Fan-out and winner-selection are not app features; budget for the substrate instead.** They shipped zero winner-selection code. The fan-out is four CLI lines in a prompt document, and the engineering went into the Run/Task/Dispatch ledger, FIFO delivery with acknowledgement, gates, and depth limits [fact]. If "fan one prompt across N agents and compare" is a product goal for us, the honest reading is: the comparison UI is small, and the thing that makes it *trustworthy* is the authority ledger underneath. Also inherit their two cheap guardrails, a nesting depth limit that a new Run does not reset, and "prefer parallel waves over chains deeper than three or four steps" (`:117-118`).

One more, on state: their per-worktree agent status is the single highest-frequency state in the system and is what forced the batching redesign (`renderer-agent-status-performance.md:21-29`). Under tmux-session-per-agent with N sessions, agent status is our equivalent hot section. It should be its own mirror section with its own once-per-drain apply and its own listener budget, not folded into a general workspace section.

---

## Analysis Summary

- **Confidence: High** on the RPC contract shape and versioning, web-mode mechanics and its drift blind spot, the store's fan-out cost, the keybinding model, the plugin API and sandbox, and the four ratchets. All are read from source or from a measured document in the clone, cited above.
- **Confidence: Medium** on the runtime/relay/E2EE internals (read at the interface level, not traced end to end) and on the orchestration mutation ledger's runtime behaviour (read from types, comments and the skill guide; the executor implementation was not traced line by line).
- **Query coverage: ~95%.** All seven numbered areas answered with citations.
- **Not verified, and worth naming:**
  - No test suite, build, or benchmark was executed. This is a read-only analysis; every performance number quoted is the reference's own measurement from `docs/reference/renderer-agent-status-performance.md`, not reproduced here.
  - The `ipcRenderer.invoke` channel count (767 raw grep hits) exceeds the `ipcMain.handle` count (~340 unique). The gap is most likely duplicate call sites for the same channel plus table-driven registration, but it was not reconciled; treat "~340 unique IPC channels" as the sounder figure and the 767 as call sites [inference].
  - `defineMethod` + `defineStreamingMethod` call sites total 571 against a 610-method registry. The 39-method gap is presumably factory- or loop-generated methods (the browser-text and repo-route modules are likely candidates); not traced [inference].
  - Whether a web-mode client can in fact use the 6 `plugins.*` RPC methods today was not tested; the finding is that the renderer's web facade does not expose them [fact], not that the host refuses them.
  - The relay's conformance path and the mobile E2EE v2 key schedule were not analysed; a separate mobile-focused review should own those.

## Open questions for a maintainer

1. Is the generated params catalog intended to become the web client's call surface, or is it mobile-only infrastructure that web deliberately skips?
2. Is there any measure of web-mode API coverage against `PreloadApi`, or is the fallback proxy the only answer?
3. Why are all 121 reliability gates still `experimental`, is promotion blocked on soak infrastructure, or on the 100-run/14-day bar being impractical?
4. Was the agent-status batching slice landed after the doc's `077f5a11cd4` baseline, and did the re-measured numbers match the bundled-prototype targets restated in §Results?
5. Are Electron IPC channels intended to converge onto the RPC registry, or is the two-contract split permanent?
