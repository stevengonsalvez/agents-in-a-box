Mode: focused-query

# Remote Hosts and Relay Federation in the Reference Implementation

**Repo** cloned reference implementation at `/tmp/claude-1000/-home-claude--ref-agents-in-a-box-desktop-app-part2/7ec9b5c8-5403-4a49-b0ee-f6b71be9354a/scratchpad/ref`
**Commit** `9aa0f7e77d366c23a3cc8de2da32ae550d397dc0` (2026-09-11 12:36:35 +0000)
**Research query** How does one UI drive and stream from many remote instances, and what architecture would reproduce it?
**Analyzed** 2026-09-11
**Claim labels** every substantive claim is tagged `[fact]` (a file:line or command output backs it) or `[inference]`.

Naming: this document calls the analyzed system "the reference implementation". Code identifiers, environment variable names, and file paths are quoted verbatim because they are the citations.

---

## 0. Executive answer: there is no single "federation" mechanism. There are four.

The most important finding is structural. What reads from the outside as "one UI, many boxes" is actually four independent mechanisms with different trust models, different transports, and different sources of truth. Conflating them is the main design trap.

```
                       ┌──────────────────────────────────────────┐
                       │        ONE DESKTOP UI (renderer)         │
                       └───┬──────────┬───────────┬───────────────┘
                           │          │           │
        ┌──────────────────┘          │           └──────────────────┐
        │ (1) local                   │ (2) ssh:<target>             │ (3) runtime:<env>
        ▼                             ▼                              ▼
┌───────────────┐          ┌──────────────────────┐      ┌────────────────────────┐
│ local daemon  │          │ DUMB EXEC HOST       │      │ PEER (own control      │
│ daemon-v<N>   │          │ detached relay       │      │ plane, own DB, own     │
│ .sock         │          │ daemon over ssh      │      │ agent sessions)        │
└───────────────┘          │ client owns control  │      │ WebSocket + NaCl box   │
                           └──────────────────────┘      └───────────┬────────────┘
                                                                     │
                                       (4) cloud relay ──────────────┘
                                       splices phone <-> desktop
                                       when neither can dial the other
```

| # | Mechanism | Transport | Who owns control plane | Survives client offline | Primary purpose |
|---|-----------|-----------|------------------------|--------------------------|-----------------|
| 1 | `local` | Unix socket to a local daemon | client | yes (daemon detached) | local terminals |
| 2 | `ssh:<target>` | SSH exec channel -> bridge process -> remote Unix socket | **client** | PTYs yes, control plane **no** | drive a remote box you can dial |
| 3 | `runtime:<env>` | WebSocket + X25519/NaCl, direct or via tunnel | **the remote host** | yes, fully | true peer federation |
| 4 | cloud relay | two outbound WebSockets spliced at a cell | n/a (opaque pipe) | n/a | NAT traversal for the mobile companion |

`[fact]` The three host kinds are a closed union: `src/shared/execution-host.ts:8-9` defines `ExecutionHostKind = 'local' | 'ssh' | 'runtime'` and `ExecutionHostId = 'local' | \`ssh:${string}\` | \`runtime:${string}\``.

`[fact]` The reference implementation explicitly forbids registering one machine under two models. `docs/reference/ssh-execution-boundary.md:98-101`: "An SSH host and a paired runtime imply opposite boundaries: the first is a dumb execution host driven by your client, the second is a peer that owns its own control plane. Registering the same machine both ways splits its worktrees across two identities... Pick one per machine."

`[fact]` The cloud relay is **not** desktop-to-desktop federation. `cloud/README.md:3-7`: "The relay that connects the mobile app to a desktop host. Phones and desktops never talk to each other directly: each opens an outbound WebSocket to a relay cell, the relay pairs the two sessions, and it splices frames between them."

---

## 1. Host abstraction

### 1.1 The identity model

```
┌──────────────────────────────────────────────────────────────┐
│ ExecutionHostId  (src/shared/execution-host.ts:9)            │
│                                                              │
│   'local'                  ── this machine                   │
│   'ssh:' + encodeURIComponent(targetId)                      │
│   'runtime:' + encodeURIComponent(environmentId)             │
│                                                              │
│ ExecutionHostScope = ExecutionHostId | 'all'   (:11)         │
└──────────────────────────────────────────────────────────────┘
              │ parseExecutionHostId  (:71-112)
              ▼
    ┌──────────────────────────────────────┐
    │ {kind:'local', id}                   │
    │ {kind:'ssh', id, targetId}           │
    │ {kind:'runtime', id, environmentId}  │
    └──────────────────────────────────────┘
```

| Concern | Implementation | Cite |
|---------|----------------|------|
| Host id is an opaque string, URI-encoded | `toSshExecutionHostId`, `toRuntimeExecutionHostId` | `src/shared/execution-host.ts:53-59` |
| `\|` is banned inside a host id | it is the delimiter of `composeWorktreeHostIdentity`; an unencoded pipe would rebind an alias to a different host | `src/shared/execution-host.ts:84-88`, `:101-103` |
| Omitted scope means **this host**, never fan-out | `requestedExecutionHostScope` defaults to `LOCAL_EXECUTION_HOST_ID`, not `'all'` | `src/shared/execution-host.ts:126-130` |
| Legacy dual spelling on a repo row | a repo carries both a legacy `connectionId` and a unified `executionHostId`; resolve the host first, then read the connection off it | `src/shared/execution-host.ts:158-205` |
| An unknown id is **one unknown host**, not all hosts | `getExecutionHostLabel` returns `'Unknown host'`, deliberately not `'All hosts'`, because the everything-scope label would render an unroutable row as though it were on every host | `src/shared/execution-host.ts:229-249` |
| Runtime-owned SSH targets are hidden | ids prefixed `runtime-ssh-` are recognised by prefix because the renderer cannot read the target's `owner` field | `src/shared/execution-host.ts:61-69` |

`[fact]` A `runtime:` host can itself own nested SSH targets, addressable as the pair `(environmentId, targetId)`; that nesting appears only in `executionHostId` and is what stops a nested-SSH workspace reading as local (`src/shared/execution-host.ts:176-205`, the long explanatory comment).

### 1.2 Per-host capabilities and health

`[fact]` `src/shared/execution-host-registry.ts:29-44` defines the registry entry the UI renders:

```ts
ExecutionHostRegistryEntry = {
  id, kind, label, detail,
  health: 'local'|'available'|'connecting'|'blocked'|'disconnected'|'error',
  connectionStatus?, compatibility?,
  capabilities?: readonly string[],
  appVersion?, protocolVersion?, minCompatibleClientVersion?,
  platform?, remoteControlState?, source?
}
```

`[fact]` Capability negotiation is by opaque string. `src/shared/protocol-version.ts` declares roughly 80 named capability constants, each with a `.v1`-style suffix, e.g. `BROWSER_HEADLESS_RUNTIME_CAPABILITY = 'browser.headless.v1'` (`:81`), `ORCHESTRATION_FEDERATION_RUNTIME_CAPABILITY = 'orchestration.federation.v1'` (`:46`). Some are static (always advertised) and some conditional on runtime facts, and the file annotates which (`:72-81`).

`[fact]` Numeric protocol version and window: `RUNTIME_PROTOCOL_VERSION = 3`, `MIN_COMPATIBLE_RUNTIME_CLIENT_VERSION = 2`, `MIN_COMPATIBLE_RUNTIME_SERVER_VERSION = 2` (`src/shared/protocol-version.ts:34-36`). Bump rules are documented in-file at `:19-32`: bump on removing a method or required param, on changing the meaning of a field, or on changing encrypted/terminal framing or auth; do **not** bump for new methods, new optional fields, or new ignorable event types.

### 1.3 How the UI aggregates vs switches

This is the part most likely to be mis-copied. The reference implementation **aggregates** across `local` and `ssh:` hosts and **switches focus** between `runtime:` peers.

```
  scope='all'
      │
      ▼
┌────────────────────────────────────────────────────────────┐
│ answering runtime fans out to the hosts IT executes:       │
│   local  +  every ssh: target it holds a provider for      │
│                                                            │
│ returns: rows  +  hostScope {hostIds[], omittedHostIds[]}  │
└────────────────────────────────────────────────────────────┘
      │                            │
      │ row cap                    │ a runtime: peer is NEVER
      ▼                            ▼ covered here -- it answers
┌──────────────────────┐        for itself
│ per-host round robin │
│ so no host starves   │
└──────────────────────┘
```

| Behaviour | Implementation | Cite |
|-----------|----------------|------|
| Coverage census on every bounded listing | `RuntimeListingHostScope = { hostIds, omittedHostIds }` | `src/shared/runtime-listing-host-scope.ts:9-12` |
| An absent scope is never completeness | "A host too old to publish a scope cannot claim one" | `src/shared/runtime-listing-host-scope.ts:27-40` |
| A `runtime:` peer is disclosure, not a gap | "a paired runtime is a peer with its own control plane, reached with `--environment`, and this runtime has no paired-runtime PTY provider to have queried" | `src/shared/runtime-listing-host-scope.ts:22-25` |
| Census construction filters runtime hosts out of `covered` | `candidates.filter(id => queriedHostIds.has(id) && parseExecutionHostId(id)?.kind !== 'runtime')` | `src/main/runtime/ref-runtime-get-runtime-id.ts:164-172` |
| Row cap must not starve a remote host | 24 remote worktrees sat at indices 496-520 of 521 and a 200-row cap returned zero of them; fixed with a per-host round robin that preserves relative order | `src/shared/host-balanced-listing-page.ts:1-56` |
| N peers live at once in main | `Map<environmentId, connection>` for request connections, shared-control connections, and status owners | `src/main/ipc/runtime-environment-request-connections.ts:36-38` |
| But one peer is the *focused* host | `getSettingsFocusedExecutionHostId` reads a single `activeRuntimeEnvironmentId` | `src/shared/execution-host.ts:220-227` |

`[inference]` So the reference UI is not a symmetric fleet console. It keeps status live for every paired peer (one `RuntimeHostStatusOwner` each) but binds workspace, worktree, and terminal work to one focused peer at a time, while genuinely merging local and SSH hosts into one list. A "single pane over N boxes" product would have to extend the merge to `runtime:` peers, which the census code deliberately does not do today.

### 1.4 The execution boundary rule

`[fact]` `docs/reference/ssh-execution-boundary.md:5-18` states two non-negotiables:

1. **No silent substitution.** An operation on a remote `repoPath` must never fall back to running on the client. "A missing SSH provider is not permission to answer locally, a local run can answer for the *wrong repository*."
2. **No asserting what you cannot observe.** Verdict vocabulary is fixed at `live` / `unverifiable` / `exited`, and "a transport failure can only ever produce `unverifiable`".

`[fact]` Rule 1 is enforced by two chokepoints that route on the target's resolved `executionHostId`, not on a repo row's `connectionId`: `requireRuntimeGitProvider` in `src/main/runtime/runtime-git-command-target.ts` and `requireRuntimeFileProvider` in `src/main/runtime/runtime-file-command-target.ts`. They throw provider-unavailable when an SSH host has no registered provider, throw `ExecutionHostNotDispatchableError` for a `runtime:` host this process does not execute, and return `null` only for `local` (`docs/reference/ssh-execution-boundary.md:16`).

`[fact]` Rule 2's reference implementation is `src/main/runtime/unstopped-pty-verification.ts:12-16`, which keeps the three verdicts distinct and treats "we could not ask" as its own answer.

`[fact]` What runs where on an SSH host (`docs/reference/ssh-execution-boundary.md:20-30`):

| Concern | Executes on |
|---------|-------------|
| PTYs, agent CLIs | remote, as children of the detached daemon, not of the ssh channel |
| git (status, diff, log, fetch, push, commit, branch, worktree) | remote, via `src/relay/git-handler.ts` |
| filesystem, watching, search | remote |
| repo setup hooks | remote |
| commit-message / PR-field AI generation | remote, using the remote agent CLI and its auth |
| GitHub / GitLab API calls | **client** (flagged in-doc as inconsistent with the rule; PRs carry the client's identity) |
| the CLI inside a remote terminal | client runtime, control plane only |

### 1.5 Remote model A: `ssh:` = deploy a dumb daemon, keep the control plane on the client

```
CLIENT                                          SSH HOST
┌─────────────────┐                       ┌──────────────────────────────┐
│ SshConnection   │  1. sftp upload       │ ~/.ref-relay/<bundle-hash>/ │
│ (ssh2)          │─────────────────────▶ │   relay.js, node             │
│                 │                       └──────────────────────────────┘
│                 │  2. exec: nohup node relay.js --detached
│                 │        --sock-path S --credential-file C ... &
│                 │──────────────────────────────────▶ ┌──────────────┐
│                 │                                    │ daemon, owns │
│                 │  3. exec: node relay.js --connect   │ PTYs as its  │
│  Multiplexer    │        --sock-path S               │ own children │
│  13-byte frames │◀═══ stdin/stdout bridge ═════════▶ │ ignores SIGHUP│
│  JSON-RPC       │        to unix socket S            └──────────────┘
└─────────────────┘
```

| Step | Detail | Cite |
|------|--------|------|
| Launch | `cd <dir> && nohup <node> relay.js --detached --grace-time G --sock-path S --credential-file C --log-file L > L 2>&1 </dev/null &` | `src/main/ssh/ssh-relay-deploy.ts:1796`, rationale at `:1786` |
| Attach | `cd <dir> && <node> relay.js --connect --sock-path S --credential-file C` over an SSH exec channel; the child bridges stdio to the Unix socket | `src/main/ssh/ssh-relay-deploy.ts:1750`, `:1846` |
| Liveness probe | inline `node -e` that connects to the socket and prints `READY` / `WAITING`, falling back to `test -S` | `src/main/ssh/ssh-relay-deploy.ts:1817` |
| Framing | 13-byte header, "matching VS Code's PersistentProtocol wire format" | `src/main/ssh/relay-protocol.ts:1-3`, `:28-41` |
| Multiplexing | one channel carries requests, notifications, keepalives, cancellation for every stream | `src/main/ssh/ssh-channel-multiplexer.ts:1-2` |
| Keepalive / timeout | send 5s, timeout 20s; request timeout 30s | `src/main/ssh/relay-protocol.ts:40-41`, `src/main/ssh/ssh-channel-multiplexer.ts:59` |
| Install dir | `.ref-remote` / `~/.ref-relay`, namespaced by a **content hash of the daemon bundle** | `src/main/ssh/relay-protocol.ts:31`; `computeRemoteRelayDir` in `src/main/ssh/ssh-relay-versioned-install.ts` |

**Not `ssh -L`.** `[fact]` The transport is an SSH exec channel plus a tiny bridge process reading a Unix socket, not a forwarded local port. Port forwarding exists separately (`src/main/ssh/ssh-port-forward.ts`, `ssh2-port-forward-provider.ts:29` uses `client.forwardOut`) and is used for other features, not for the control channel.

**Survival semantics.** `[fact]` `docs/reference/ssh-execution-boundary.md:34`: remote work survives the client going away, because the daemon is detached, its handler ignores `SIGHUP`, the PTY is its child rather than the ssh channel's, and quitting is a detach not a dispose (`src/main/ssh/ssh-relay-session.ts:901-915`). Sleep pushes `graceTimeSeconds: 0` to un-bound any grace window.

`[fact]` Two ways remote work actually stops (`:36-39`): a bounded grace period (shipped default `0` = keep alive until reset; if the keep-alive box is unchecked the range is 60s to 7d, form default 24h), and a host-acknowledged explicit user action. Ordinary disconnect and app quit do **not** stop it. No command reports which grace setting is in effect for a target, so at N hours the remote is `unverifiable`, not `exited`.

**The version-lock failure mode.** `[fact]` This is the single most instructive defect in the SSH model (`docs/reference/ssh-execution-boundary.md:43-49`). Because the install directory and therefore the socket path are namespaced by a **build content hash**, and the daemon refuses any client whose bundle hash differs (`handleDaemonHandshakeFrame` in `src/relay/relay-handshake.ts`, exit code 42 `EXIT_CODE_VERSION_MISMATCH`), the first reconnect after an app update deploys a new daemon at a path the incumbent was never listening on. Two builds with byte-identical protocols still refuse each other. Every PTY the incumbent owns becomes permanently `unverifiable`: running, unreachable, never `exited`. The old install directory stays pinned against GC because its socket really is live (`hasLiveRelaySocket` in `src/main/ssh/remote-install-gc.ts`).

`[fact]` The peer model does not have this failure, and the doc names that as the concrete reason behind "one host, one model" (`:49`): the local daemon's endpoint is namespaced by a **semantic protocol version** (`daemon-v<N>.sock`, `getDaemonSocketPath` in `src/main/daemon/daemon-spawner.ts`), every earlier protocol version stays attachable (`PROTOCOL_VERSION` in `src/main/daemon/daemon-protocol-version.ts`), and a daemon holding live sessions is preserved across a version change rather than replaced (`shouldPreserveDaemonWithLiveSessions` in `src/main/daemon/daemon-replacement-preflight.ts`).

**Client / daemon are version-locked by design.** `[fact]` `docs/reference/ssh-reconnect-source-recovery.md:48-59` records a correction worth copying: the client deploys and launches its own daemon build into a version-scoped directory (`ssh-relay-deploy.ts:231`, `:594`), and `validateGrant` rejects any grant whose `serverBuildId` differs (`ssh-pty-consumer-session.ts:58-65`, in-code rationale "client and relay ship in one build"). Mixed versions do not occur on this channel. The independent-update rule governs `runtime:` peers only.

**Host key verification.** `[fact]` `docs/reference/ssh-host-key-verification.md:6-15` documents the defect and its fix shape: `src/main/ssh/ssh-connection.ts:1184` installed a `hostVerifier` that recorded a fingerprint and then `return true`, so every ssh2 connection accepted every host key; there is exactly one ssh2 `Client` construction site so the fix has a single chokepoint. Scope is per-connection, not per-feature: one `SshConnection` per target serves exec, SFTP, port forwarding, the filesystem watcher, and daemon deploy (`:13-14`).

`[fact]` Corrected threat model (`:16-41`), all three corrections useful to inherit:
- Jump hosts are already safe: `shouldUseSystemSshTransport` (`ssh-transport-selection.ts:71-91`) and `resolveEffectiveProxy` (`ssh-proxy-command.ts:17-38`) branch on the same inputs, and `attemptConnect` returns unconditionally after the system probe (`ssh-connection.ts:670-673`), so ProxyJump/ProxyCommand go through OpenSSH and are verified.
- Agent forwarding risk applies only to users who set `ForwardAgent yes` (`ssh-connection-utils.ts:203-205`); agent *auth* signatures bind the session id and cannot be replayed onward.
- Credential theft was understated: `isAgentFallbackError` treats any auth error as agent fallback (`ssh-connection-utils.ts:59-61`), so a machine-in-the-middle that rejects publickey walks the user to the password prompt (`ssh-connection.ts:844`) and the private-key passphrase prompt (`:834`), and `cachedPassword` is replayed without prompting on every reconnect (`:709`). The real impact is the **return** direction: the attacker becomes the host the workspace trusts, driving protocol frames, landing SFTP content in local worktrees, and feeding agent-hook payloads in.

`[fact]` Decision D1 (`:45-60`): read the user's real `known_hosts` as a trust source, never write to it, because that file is shared with every other SSH tool on the machine.

**Reconnect and replay.** `[fact]` `docs/reference/ssh-execution-boundary.md:41`: reconnect re-attaches to the same live PTYs and replays a bounded buffer, `REPLAY_BUFFER_MAX`, a 102,400-code-unit tail. Output beyond that while away is lost to the client even though the process was never interrupted. "The transcript is truncated; the work stays `live`." Confirmed at `src/relay/pty-handler.ts:340`: `export const REPLAY_BUFFER_MAX = 100 * 1024`.

`[fact]` A subtle and important bug is recorded in full in `docs/reference/ssh-reconnect-source-recovery.md:61-105`: checkpointed source recovery has **never** run on an SSH reconnect, because a reconnect reuses the same `clientId` (`Dispatcher.setWrite`, `src/relay/dispatcher.ts:149-157`, reuses `this.primaryClient` including its id and replaces only the writer), so `activate()` short-circuits to `'existing'` at `relay-pty-source-publication.ts:99-108` and the `rotateDelivery` recovery path at `:118-142` is unreachable. The byte tail is not a fallback, it is the only path that has ever run. The recorded fix is to give a reconnected primary a transport generation bumped in `setWrite` and compare it alongside `clientId`.

### 1.6 Remote model B: `runtime:` = a peer with its own control plane

```
CLIENT (desktop / CLI / mobile)             PEER HOST (`serve` / daemon)
┌────────────────────────────┐             ┌──────────────────────────────┐
│ RemoteRuntimeSharedControl │  WebSocket  │ RuntimeRpcWebSocketDispatch  │
│ Connection                 │═══════════▶ │  validateToken(deviceToken)  │
│  - one socket per peer     │  NaCl box   │  scope: 'mobile'|'runtime'   │
│  - N logical requests      │  X25519     │  mobile -> allowlist only    │
│  - N logical subscriptions │             │  runtime -> full surface     │
│  - reconnect + replay      │             └──────────────────────────────┘
└────────────────────────────┘
```

| Concern | Implementation | Cite |
|---------|----------------|------|
| Endpoint record | `{id, kind:'websocket', label, endpoint, deviceToken, publicKeyB64}`, multiple per environment with a `preferredEndpointId` | `src/shared/runtime-environments.ts:4-36` |
| Environment record | `{id, name, pairingRevision, pairedDeviceId, runtimeId, source:'manual'\|'ephemeral-vm', connectionDependency?: 'ssh-tunnel', endpoints[], preferredEndpointId}` | `src/shared/runtime-environments.ts:23-36` |
| Secrets redacted for the renderer | `redactRuntimeEnvironment` strips `deviceToken` and `publicKeyB64` | `src/shared/runtime-environments.ts:44-53` |
| One socket per peer, many logical streams | `RemoteRuntimeSharedControlConnection` holds `pendingRequests`, `subscriptions`, a reconnect scheduler, retired request ids, socket generation | `src/shared/remote-runtime-shared-control-connection.ts:31-45` |
| N peers concurrently | three `Map`s keyed by environment id | `src/main/ipc/runtime-environment-request-connections.ts:36-38` |
| Per-peer status with one retry slot | `RuntimeHostStatusOwner`, backoff `[3s, 6s, 12s, 30s, 60s]`, 15s request timeout | `src/shared/runtime-host-status-owner.ts:9-42` |
| RPC envelope | `{id, ok:true, result, _meta:{runtimeId}}` / `{id, ok:false, error:{code,message,data}, _meta}` / `{_keepalive:true}`, all zod `.strip()` | `src/shared/runtime-rpc-envelope.ts:9-95` |
| Reach control | `RuntimePairingReach = 'this-computer' \| 'network'`; widening the listener to all interfaces is one-way for the process lifetime and must follow the reach the user picked, not the shape of the address typed | `src/shared/runtime-pairing-reach.ts:1-5`, `src/main/runtime/runtime-rpc/runtime-rpc-network-exposure.ts:7-36` |
| Tailnet is a first-class hint | recognises `*.ts.net`, `100.64.0.0/10`, `fd7a:115c:a1e0::/48` and appends targeted remediation to unreachable-runtime errors | `src/shared/remote-runtime-tailscale-hint.ts:10-80` |

`[fact]` The peer surface is large: the generated RPC params catalog holds roughly 610 method entries (`src/shared/rpc-contract/rpc-params-catalog.generated.ts`, 1167 lines, 610 quoted keys), and the mobile allowlist is a strict subset (`src/main/runtime/runtime-rpc/runtime-rpc-mobile-method-allowlist.ts`).

`[fact]` Authz at the transport boundary (`src/main/runtime/runtime-rpc/runtime-rpc-websocket-dispatch.ts:57-92`): a per-device token is required, a mismatch between the socket-bound token and a repeated request field is rejected, the token is validated against a device registry, and `device.scope === 'mobile'` outside the allowlist is `forbidden`. The E2EE channel identity is preferred over the repeated request field: "E2EE already authenticated the channel; authorize by that bound identity" (`:65-66`).

`[fact]` Per-device revocable tokens, not a shared secret: "Each paired device gets its own revocable token so compromising one device doesn't expose others. The registry is a simple JSON file with hardened permissions" (`src/main/runtime/device-registry.ts:1-4`).

### 1.7 Mixed-version discipline for peers

`[fact]` `docs/reference/remote-wire-compatibility.md:3-7`: "users update the two independently. **Mixed versions are the normal state**, not an edge case." Three rules:

| Rule | Statement | Why it bites |
|------|-----------|--------------|
| 1 | A new **optional JSON field** on an existing frame is safe | only while every reader treats it as optional; the moment a newer client requires it, that client is broken against every older host, which is the same defect as removing a field (`:12-29`) |
| 2 | A new **stream opcode** is NOT safe, negotiate it | `decodeTerminalStreamFrame` returns `null` for an unknown opcode and the caller drops the frame with no error, so the feature appears to hang and input is swallowed (`:31-59`) |
| 3 | Changing **what the host publishes** breaks old clients with no wire change | clients react to frame content: a field the host stops populating, a value whose units change, content the host stops synthesizing, a frame that starts or stops being sent (`:61-76`) |

`[fact]` The negotiation pattern for Rule 2 is worth copying verbatim (`:47-52`): the client advertises support in the `Subscribe` frame's `capabilities`; the host echoes `capabilities: { outputPause: 1 }` on the `subscribed` event; the client sends the new opcode only after that echo; the host only acts on it when it negotiated it.

`[fact]` Opcode numbers are permanent. The in-code comments explain why a shipped number cannot be reused: `Ack = 13` renumbered because `Metadata = 12` had already shipped to mobile clients in one release, and `ClaimViewport = 14` because 13 was taken (`src/shared/terminal-stream-protocol.ts:25-30`).

`[fact]` Enforcement is a real cross-version test harness: `tests/e2e/cross-version-wire/cross-version-terminal-wire.unit.test.ts` runs the real host RPC methods and the real renderer multiplexer from **two builds against each other**, current tree against the newest release tag, in both skew directions, over one scripted journey (subscribe, input, hide/reveal snapshot, drop, reconnect) (`docs/reference/remote-wire-compatibility.md:78-98`).

`[fact]` The harness has a named anti-pattern (`:100-130`): never write down what the old side lacks. Because the baseline is whichever release tag is newest, it moves on every cut, so a `not.toHaveProperty` or a hard-coded field list stops being true the first time a release ships that field, reddens unrelated pull requests, and trains people to ignore the job. Expectations must be **derived** from the checked-out baseline instead.

---

## 2. The cloud relay

### 2.1 What it is for

`[fact]` One purpose only: let a phone reach a desktop when neither can dial the other. `cloud/README.md:3-7`. It is not NAT traversal in the STUN/TURN sense (no UDP hole punching anywhere in the tree) and not a sharing or collaboration layer. `[inference]` It is a TURN-shaped relay specialised to one host and up to eight clients, with an application-level pairing and credential protocol on top.

`[fact]` The push gateway (`cloud/apps/push`) is deployed alongside but **is not on the relay data path** (`cloud/README.md:28-31`).

### 2.2 Topology: director and cells

```
                   ┌─────────────────────────────────────┐
                   │ DIRECTOR   (ref_RELAY_ROLE=director)│
                   │  POST /v1/assign  -> cellUrl, epoch  │
                   │  POST /v1/resolve -> cellUrl (resume)│
                   │  GET  /v1/regions -> allowlist       │
                   │  POST /v1/admin/* (40+ ops routes)   │
                   └───────┬─────────────────────┬────────┘
                           │ assignment          │
              ┌────────────▼──────┐   ┌──────────▼─────────┐
              │ CELL c1 us-central│   │ CELL c2 asia-east2 │
              │ /v1/host/control  │   │  (same image,      │
              │ /v1/connect/:hid  │   │   ROLE=cell)       │
              │ /v1/host/data/:cid│   │                    │
              └───────────────────┘   └────────────────────┘
                           │
                    shared Cloud SQL (us-central1 only)
```

`[fact]` "The same image runs as a director or a cell depending on `ref_RELAY_ROLE`. A director assigns hosts to cells and coordinates migrations; cells carry the user connections" (`cloud/README.md:17-19`, roles enumerated at `cloud/apps/relay/src/config.ts:42` as `'combined' | 'director' | 'cell'`).

`[fact]` Three WebSocket endpoints, all on the cell (`cloud/apps/relay/src/relay-server.ts`):

| Path | Who dials | First frame | Cite |
|------|-----------|-------------|------|
| `/v1/host/control` | the desktop, one per host, long-lived | `host-hello` | `:439` |
| `/v1/connect/:relayHostId` | a phone or second client | `relay-auth` with an invite or resume credential | `:294-295` |
| `/v1/host/data/:connId` | the desktop, one per accepted client | `host-data-auth` with the conn ticket | `:385-390` |

`[fact]` `relayHostId` must match `/^[A-Za-z0-9_-]{16}$/` and a non-matching path is rejected with 429, not 404 (`:295-297`). `[inference]` Answering 429 rather than 404 avoids confirming whether a host id exists.

### 2.3 The splice: message model

The relay is **not** pub/sub and **not** request/response. It is a pair of spliced byte streams with a state machine in front.

```
 PHONE                        CELL                          DESKTOP
   │                            │                              │
   │                            │◀──── /v1/host/control ───────│ (long-lived)
   │                            │  host-hello -> challenge ->  │
   │                            │  challenge-ack -> hello-ack  │
   │                            │                              │
   │── /v1/connect/:hostId ────▶│                              │
   │   {type:'relay-auth',      │                              │
   │    credential}             │                              │
   │                            │ reserveCredential()          │
   │                            │── conn-open{connId,          │
   │                            │   connTicket, kind,          │
   │                            │   relayDeviceId,             │
   │                            │   attachDeadlineMs:10000} ──▶│
   │                            │                              │
   │                            │◀─ /v1/host/data/:connId ─────│
   │                            │   {type:'host-data-auth',    │
   │                            │    connTicket, generation}   │
   │                            │                              │
   │                            │  wireSplice(client, host)    │
   │◀── relay-hello{ok:true} ───│                              │
   │                            │                              │
   │══════ opaque E2EE frames, relay forwards bytes ═══════════│
```

`[fact]` Splice state machine, strictly forward-only (`cloud/packages/relay-contract/src/splice-state-machine.ts:1-34`):

```
pre-auth-admitted -> credential-lease-reserved -> host-notified
  -> attach-pending -> host-attached -> client-acknowledged
  -> spliced -> e2ee-confirmable -> teardown
(every state can also go straight to teardown; teardown is absorbing)
```

`[fact]` A guard exists specifically to stop a fake splice: `mayAcknowledgeClient` returns true only in `host-attached` **and** with both forwarding handlers installed, because "success before both forwarding handlers exist can strand a client on a fake splice" (`:31-34`).

`[fact]` The forwarder itself (`cloud/apps/relay/src/splice-forwarder.ts:48-157`) is ~110 lines: two independent directions, each with a queue, a byte counter, and a wedge timer; `setNoDelay(true)` on both transports; `close` on either side or an error closes both. Oversize-frame kills are surfaced separately because "the ws receiver kills a connection whose frame exceeds maxPayload with this message; surfacing it separately is what makes catalog-growth kills visible" (`:42-46`).

### 2.4 Backpressure, precisely

```
frame arrives on source
   │
   ├── queue empty AND target.bufferedAmount <= HIGH (256 KiB)
   │        └─▶ send immediately
   │
   └── else reserve bytes
            ├── per-splice queued + bytes > HARD (8 MiB + 256 KiB)  ─▶ close 4429
            ├── process-wide budget exhausted (64 MiB)              ─▶ close 4429
            └── enqueue; source.pause(); flush loop:
                     drain while target.bufferedAmount <= LOW (64 KiB)
                     queue empty -> source.resume()
                     still queued after 10s -> close 4429 "wedged relay link"
                     else retry in 25ms
```

| Constant | Value | Cite |
|----------|-------|------|
| `spliceLowWaterBytes` | 64 KiB | `cloud/packages/relay-contract/src/admission-budgets.ts:9` |
| `spliceHighWaterBytes` | 256 KiB | `:10` |
| `spliceHardQueuedBytes` | 8 MiB + 256 KiB | `:13` |
| `maxProcessQueuedBytes` | 64 MiB | `:8` |
| `spliceWedgedTimeoutMs` | 10 s | `:14` |
| flush retry interval | 25 ms | `splice-forwarder.ts:117` |
| `maxFrameBytes` | 8 MiB | `cloud/packages/relay-contract/src/protocol-limits.ts:9` |
| `maxConnectionsPerHost` | 8 | `:10` |
| `idleTimeoutMs` | 10 min | `:11` |
| `firstFrameDeadlineMs` | 2 s | `:2` |
| `maxHttpBodyBytes` | 4 KiB | `:3` |
| control ping / silence | 15 s / 75 s | `:21-22` |

`[fact]` The hard queued bytes figure is deliberately above one max frame: "must admit at least one `maxFrameBytes` frame for a backpressured peer, or large catalog responses close the splice with `LIMIT_EXCEEDED`" (`admission-budgets.ts:11-13`).

`[fact]` The 8 MiB frame limit is itself a scar: the desktop's worktree catalog response already exceeds 1 MiB on large workspaces (~775 KiB at 415 worktrees, growing), and an oversized frame killed the session on every reconnect. "8MiB buys years of headroom; catalog pagination is the long-term fix on the desktop side" (`protocol-limits.ts:4-8`).

`[fact]` No compression anywhere in the data path. All four WebSocket servers and every client construct with `perMessageDeflate: false` (`relay-server.ts:105-110`, `:157-168`; `src/main/runtime/rpc/relay-transport.ts:68`; `src/main/browser/paired-runtime-browser-network-transport.ts:159`). A grep for `gzip|deflate|zstd|brotli` across the remote-runtime shared modules and the runtime RPC directory returns nothing. `[inference]` Correct, because the payload is already AEAD ciphertext and would not compress.

### 2.5 Admission control

`[fact]` Pre-auth admission (`cloud/packages/relay-contract/src/admission-budgets.ts:1-15`, applied at `relay-server.ts:188-215`): `maxPreAuthConnections: 45`, `maxPreAuthPerSource: 4`, `maxPreAuthAttemptsPerSourcePerMinute: 30`, with a 60-second sliding window per source.

`[fact]` The admission source is derived carefully: "Google Front End appends client and load-balancer addresses after any caller-supplied values, so only the penultimate hop is trustworthy" (`relay-server.ts:66-75`). `[inference]` A self-hosted deployment behind a different proxy would need this rewritten or it would rate-limit by an attacker-controlled header.

`[fact]` Per-cell connection ledger with reserved lanes: hard cap 600, of which `reservedHostControls: 100` are exclusive to same-host control rotation and rebind overlap, and `reservedHostDataSockets: 150` (`admission-budgets.ts:6-7`, `:17-36`). Ordinary sockets stop at the 500-unit ceiling and cannot borrow the control reserve (`cloud/docs/ref-relay-operations.md:132-139`).

`[fact]` The director stops placement earlier than the cell's own ceiling, by the cell's configured "unobserved arrival" allowance (`cellPlacementCeiling`, `admission-budgets.ts:54-60`). The admission inequality is stated in the runbook (`cloud/docs/ref-relay-operations.md:118-126`):

```
enforced connection units
  + durable pending-control leases
  + configured unobserved-arrival bound
  < 600 - 100 control-rebind reserve
```

`[fact]` The unobserved bound is defined operationally: the isolated test's maximum arrivals over one heartbeat plus reaction latency plus maximum pre-auth and in-flight units, with a 20% safety margin, and p99 is recorded separately as an SLO, not as the safety bound (`:127-131`).

`[fact]` Each accepted phone reserves **two** units: the phone socket and its future host-data socket; the second leg transfers the reservation instead of competing for capacity (`:132-135`).

`[fact]` At the ceiling the cell returns HTTP 503 for new work and leaves established sockets open; a 503 makes clients consult the director, but a healthy assignment normally resolves to the same cell. "It is backpressure, not an automatic migration" (`:142-146`).

### 2.6 Registration: the host control handshake

`[fact]` Control frames (`cloud/packages/relay-contract/src/control-messages.ts`):

| Frame | Fields | Cite |
|-------|--------|------|
| `host-hello` | `v:1, relayHostId, assignmentEpoch, hostPublicKeyB64, appVersion, previousGeneration?, controlResumeSecret?` | `:21-31` |
| `host-challenge` | `challengeId, relayEphemeralPublicKeyB64, nonceB64, ciphertextB64, expiresAt` | `:33-41` |
| `host-challenge-ack` | `challengeId, proofB64` | `:43-45` |
| `host-hello-ack` | `generation, controlResumeSecret, leaseExpiresAt, activeConnIds[<=8], pendingConns[<=8]` | `:78-87` |
| `conn-open` | `connId, connTicket, kind:'invite'\|'resume', relayDeviceId, attachDeadlineMs` | `:89-97` |
| `host-data-auth` | `v:1, connTicket, generation` | `:99-105` |
| `invite-create` / `invite-created` | `reqId, relayDeviceId` / `inviteToken, expiresAt, maxAttempts<=16` | `:107-118` |
| `device-revoke` | `reqId, relayDeviceId` | `:120-122` |
| `auth-refresh` | `relayJwt` | `:124` |
| `drain` | `graceMs<=1h, recovery:'resolve-director'` | `:126-131` |
| `heartbeat` | `t` | `:133` |

`[fact]` Every schema is zod `.strict()`. This has a real consequence the code calls out twice: a new hello key would be refused by every already-deployed cell, so host capabilities ride an **HTTP header on the control upgrade** instead: `x-ref-host-capabilities` (`control-messages.ts:47-52`, mirrored at `src/main/runtime/relay/relay-control-protocol.ts:31-36`). `[inference]` This is the concrete cost of `.strict()` on a long-lived wire and is worth deciding deliberately rather than inheriting.

`[fact]` Proof of host key possession is a bound transcript, not a bare signature. `buildHostProofTranscript` (`cloud/packages/relay-contract/src/host-proof-transcript.ts:60-85`) length-prefixes every field name and value, and covers: protocol domain, version, `relayOrigin`, relay ephemeral public key, challenge nonce, challenge id, `issuedAt`, `expiresAt`, `userId`, `profileId`, `organizationId`, `relayHostId`, host public key, `assignmentEpoch`, `previousGeneration`, `resumeRequested`.

`[fact]` The cell encrypts a random 32-byte secret to the host's Curve25519 key (`Curve25519-XSalsa20-Poly1305`, `:5`) over that transcript, and the host answers with `HMAC-SHA-256` over the transcript. In-code rationale: "the encrypted random secret makes the public transcript insufficient to forge the ack" (`:87-99`).

`[fact]` The relay JWT carries exactly `{sub, prof, org?, relayHostId, purpose:'host-control', exp}`, verified `ES256` against a remote JWKS with a fixed issuer and audience (`cloud/apps/relay/src/relay-token-verifier.ts:5-30`).

### 2.7 Control continuity: what happens when the control socket dies

`[fact]` `cloud/packages/relay-contract/src/control-continuity.ts:1-43`:

```
control socket lost
        │
        ▼
controlLossDisposition({matchingResumeSecret, competingGeneration})
        │
        ├── matchingResumeSecret ──────▶ 'rebind'       (same host returning)
        ├── competingGeneration ───────▶ 'fence-old'    (newer generation wins)
        └── neither ───────────────────▶ 'orphan-grace' (30s)
```

| Generation state | Meaning |
|------------------|---------|
| `active` | may start new work |
| `orphaned` | within the 30s grace |
| `drain-only` | existing splices finish, no new work |
| `fenced` | superseded by a newer generation |
| `closed` | terminal |

`[fact]` `mayStartNewRelayWork(state, authExpired)` requires `active` **and** not expired (`:25-27`). An auth refresh must preserve identity: `sub`, `prof`, `org`, `relayHostId` all unchanged (`preservesRelayAuthIdentity`, `:36-43`). Refresh window is 60 to 120 seconds before expiry (`:11-12`), and an expired auth still gets a 60-second grace for an **existing** splice (`:13`).

### 2.8 Discovery and reconnection by a second client

```
QR / paste  ──▶ ref://pair?code=<base64url(PairingOffer)>
                        │
                        ▼
   PairingOffer v2 {endpoint, deviceToken, publicKeyB64, pairedDeviceId?,
                    scope:'mobile'|'runtime',
                    relay?: {v:1, directorUrl, cellUrl, assignmentEpoch,
                             relayHostId, inviteToken, inviteExpiresAt,
                             e2eeFraming:2}}
                        │
       ┌────────────────┴─────────────────┐
       │ relay absent -> direct only      │
       │ relay present -> dial cellUrl    │
       └──────────────────────────────────┘
                        │
             wss://<cellUrl>/v1/connect/<relayHostId>
                        │
        first frame {type:'relay-auth', mode:'connect', credential}
                        │
            ┌───────────┴────────────┐
            │ relay-hello ok:true    │  invite (first time)  or
            │ credentialKind, lease  │  resume (thereafter)
            └────────────────────────┘
```

| Element | Detail | Cite |
|---------|--------|------|
| Deep link | `ref://pair?code=<base64url>`; query param chosen because "Android camera intents and Expo Router preserve query params more reliably than URL fragments" | `src/shared/pairing.ts:14-27` |
| Strict URL parsing | protocol must be `ref:` **and** hostname exactly `pair`; a prefix check had accepted `ref://pairing?...`, and "only the pairing deep-link host may carry runtime auth material" | `src/shared/pairing.ts:40-60` |
| Offer version | `PAIRING_OFFER_VERSION = 2` | `src/shared/mobile-relay-pairing-offer.ts:9` |
| Invite TTL | max 10 min, with 30s clock-skew leeway because "the cell stamps expiry from its own clock" | `:13-17`, `:56-66` |
| Canonical HTTPS origins only | `directorUrl` and `cellUrl` must equal `new URL(v).origin` with `https:` | `:19-32`, `:52-53` |
| Canonical 32-byte key required for relay offers | `relayHostId` is derived from the decoded key bytes, so permissive legacy base64 aliases are refused | `:34-44`, `:92-100` |
| Relay is mobile-only | a `relay` block with `scope:'runtime'` is a validation error: it "would imply routing and credential support that client does not have" | `:82-91` |
| Anywhere vs local-only | `MobilePairingConnectionMode = 'automatic' \| 'local-only'`; `automatic` (relay) cannot be minted without a signed-in desktop, and the UI gates minting rather than silently degrading | `src/shared/mobile-pairing-connection-mode.ts:1-41` |
| Host id derivation | `deriveRelayHostId(publicKey)` | `src/main/runtime/relay/relay-session-broker.ts:41` |

`[fact]` Reconnection after the assignment moves:

| Close code | Meaning | Recovery |
|-----------|---------|----------|
| 4401 `BAD_OUTER_CREDENTIAL` | bad credential | endpoint-scoped |
| 4404 `HOST_OFFLINE` | host genuinely absent (the only rejection allowed to name a cause) | endpoint-scoped |
| 4408 `PEER_DROPPED` | the other side went away | - |
| 4409 `WRONG_CELL` | assignment is elsewhere | consult the director |
| 4429 `LIMIT_EXCEEDED` | queue or connection limit | endpoint-scoped |
| 4503 `DRAINING` | cell draining | consult the director |

`[fact]` Codes at `cloud/packages/relay-contract/src/close-codes.ts:1-10`. Operator rule: "Treat `4401`, `4404`, and `4429` as endpoint-scoped. `4409`, `4503`, transport failure, `1006`, `503`, and `504` recover through the configured director" (`cloud/docs/ref-relay-operations.md:13`). The 4404 comment explains the distinction precisely: it names a cause only because "the host is genuinely absent. The attach-deadline 4404 below fires while control is still connected" (`cloud/apps/relay/src/host-session-registry.ts:279-281`).

`[fact]` Resume recovery is a bounded `POST /v1/resolve` carrying `{relayHostId, resumeToken}` and returning `{cellUrl, assignmentEpoch, leaseExpiresAt}`; an unexpired invite may use only the director compatibility WebSocket (`cloud/packages/relay-contract/src/director-messages.ts:33-48`; `cloud/docs/ref-relay-operations.md:14`).

`[fact]` A redirect is only honoured from the configured director and only forward in epoch: `isTrustedNewerMove` requires `sourceOrigin === configuredDirectorOrigin && move.assignmentEpoch > currentAssignmentEpoch`, because "cells and stale director responses must never redirect a credential-bearing client" (`director-messages.ts:58-69`).

`[fact]` Credential rotation is versioned with a grace window, and the install is a two-phase idempotent commit: `DeviceCredentialInstall` carries `newResumeTokenHash`, an optional `expectedCurrentHash` for compare-and-swap, and an authorization discriminated on `'relay-basis'` (proved by an existing splice) or `'authenticated-direct'`; the result is persisted so a lost response can be re-read via `DeviceCredentialInstallStatus` (`cloud/packages/relay-contract/src/credential-messages.ts:39-82`). `relay-hello` for a resume credential reports `acceptedCredentialVersion`, `acceptedAs: 'current'|'grace'`, `resumeExpiresAt`, and `graceExpiresAt?` (`:26-36`).

### 2.9 "Fence" means operational, not cryptographic

`[fact]` Two distinct uses of the word, and mixing them up would be an expensive mistake:

1. **Control-generation fencing** (data plane): a competing newer control generation fences the old one, per `controlLossDisposition` (`control-continuity.ts:16-23`) and `CONTROL_GENERATION_STATE.FENCED` (`:5`).
2. **Cell fencing** (infrastructure): forcing a relay cell offline by driving its managed instance group to desired size zero under Terraform, so a host's work can be moved with proof the old cell is gone.

`[fact]` The second is what `cloud/apps/relay-fence-broker` exists for. "A private, IAM-only service that owns the durable mutation lease, the Terraform checkout, and the narrow Compute mutation used when a registered target is superseded. The workflow that calls it holds read and invoke rights only, never those mutation permissions" (`cloud/README.md:20-23`).

`[fact]` The stated goal: "The workflow must never attest a cell as fenced until Terraform state, GCE, runtime identity, and retained routing all prove the same exact cell incarnation is offline" (`cloud/docs/relay-terraform-fencing-plan.md:24-27`).

`[fact]` Safety boundary worth copying as a pattern (`:30-45`): keep the cell route and backend while its group is fenced at size zero; treat any apply whose start cannot be disproved as recover-forward; never restore a fenced cell automatically after an apply may have started; never print, upload, or commit complete plan JSON; the broker accepts only its configured source, failed target, and replacement target, and its image commit must equal the reviewed fence commit.

`[fact]` The lost-response problem is handled explicitly rather than retried: "Any `/drain` attempt is rollback-unsafe because a lost response cannot prove the old source did not accept it... If the durable attempt is still exactly `prepared`, recover-forward may acquire the original send permit once and send its original trace and grace. A `send-may-have-started` attempt without an application receipt remains ambiguous and must freeze; it is never resent" (`cloud/docs/ref-relay-operations.md:153-159`). Routes for this state machine: `/v1/admin/drain-attempt-prepare`, `-send`, `-receipt`, `-recover-forward` (`cloud/apps/relay/src/app.ts:1056-1144`).

`[fact]` `cloud/apps/relay-ops` is the operations console and incident monitor, not a data-plane component (`cloud/README.md:24-26`), about 3,886 non-test lines.

### 2.10 Regional placement

`[fact]` Region selection happens **on the desktop**, before requesting an assignment (`docs/reference/relay-regional-placement.md:3-9`):

```
GET /v1/regions  (director publishes an allowlist of HTTPS cell
                  subdomains of that director only)
        │
        ▼
per origin: discard 1 warm-up /health probe   (cold request pays TCP+TLS
            take 3 bounded samples            setup that can exceed the RTT)
            compare regions by MINIMUM
        │
        ├── any region rejected or unmeasurable ─▶ send NO hint
        │        (cached 1 hour, not 24)
        └── stable winner ─▶ send preferredRegion (cached 24 hours,
                             changes only if the alternative is
                             materially faster)
```

`[fact]` Sending no hint is **not** neutral: the director assigns `preferredRegion ?? RELAY_DEFAULT_REGION`, and the default is `us-central1`. The trade is accepted because the relay database is `us-central1`-only, and bounded by the one-hour no-hint cache (`:11-18`).

`[fact]` The assignment request sends only `preferredRegion`. "It does not send latency, IP address, country, pairing data, or credentials" (`:26-27`), which matches `AssignmentRequestSchema` exactly: `{v, relayHostId, reconnect?, preferredRegion?}` (`cloud/packages/relay-contract/src/director-messages.ts:13-22`).

`[fact]` Self-heal after landing on a far cell is narrowly scoped: the cache is deleted only when it names a region other than the best measured one **and** the assigned cell is more than three times slower than that region, because "a far cell under a cache that still names the best region means the director declined the hint, and re-measuring would return the same answer" (`:20-25`).

`[fact]` The measurement is desktop-only: folder and SSH workspaces share the same local broker and do not run probes on remote hosts, and the phone connects to the exact cell URL in the pairing payload, so its location is never measured independently (`:32-35`).

`[fact]` A rolled-back director that rejects the new field is retried once without only that field, while preserving reconnect behaviour (`:29-30`).

---

## 3. Session authority: who wins when two clients disagree

### 3.1 The rule

`[fact]` The execution host is the source of truth, and clients make **idempotent** requests keyed by a client-minted operation id. The host commits first and replays the committed result to any retry.

```
CLIENT A                          EXECUTION HOST                    CLIENT B
   │                                     │                              │
   │ createAgentSession                  │                              │
   │   clientOperationId=<ts>-<32hex> ──▶│ commit, spawn agent          │
   │          X response lost            │                              │
   │ retry same clientOperationId ──────▶│ disposition:'replayed'       │
   │                                     │ (spawn count still 1)        │
   │                                     │                              │
   │ ensureAgentSession (resume) ───────▶│◀── ensureAgentSession (same) │
   │                                     │  one 'created', one 'adopted'│
   │                                     │  same terminal handle        │
```

`[fact]` The repro script asserts exactly this, end to end against a real paired server (`config/scripts/remote-agent-session-authority-repro.mjs`):

| Assertion | Cite (line within `sed -n 80,220p` offset from 80) |
|-----------|------|
| a dropped response still commits on the host, exactly one spawn marker | `:+22-39` (`'drop-response'`, then `countSpawnMarkers() !== 1`) |
| the retry returns `disposition === 'replayed'`, not a second spawn | `:+40-51` |
| two concurrent identical resumes return sorted dispositions `['adopted','created']` and the **same** terminal | `:+69-80` |
| a third retry returns `'adopted'` and does not change the spawn count | `:+82-94` |
| retiring one resumed terminal does not lose unrelated terminals | `:+97-126` |
| a stale write against a retired pane's ids is rejected | `:+137-141` |

`[fact]` The host also decides presentation: a client asking for `presentation: 'focused'` gets `surface: 'background'` back, and the test asserts that (`:+46-48`). `[inference]` Surface ownership is host-side so two clients cannot fight over focus.

### 3.2 The mechanism

`[fact]` `src/shared/agent-session-host-authority.ts` defines the contract:

| Element | Detail | Cite |
|---------|--------|------|
| Operation id format | `/^(\d{13})-[0-9a-f]{32}$/`, timestamp-prefixed | `:39-48` |
| Operation freshness | future skew 5 min, max new-operation age 24 h | `:36-37` |
| Execution claim | `{digestVersion:1, keyId, identityDigest (sha256 b64url, 43 chars), worktreeScopeDigest, agent}` | `:69-75`, validated `:148-165` |
| Owner binding | `{claim, generation, phase:'reserved'\|'live', ptyId, surface}` | `:77-83` |
| Surface binding | `{worktreeId, tabId, leafId, terminalHandle}` with `terminalHandle` required to start `term_` and be base64url | `:62-67`, `:167-182` |
| A claimed spawn result must be `live` | `isAgentSessionClaimedSpawnResult` requires `result.owner.phase === 'live'` | `:198-210` |
| Caller identity | `{clientId?, clientKind?: 'mobile'\|'runtime', signal?}` | `:142-146` |
| Protocol versions | execution-owner 2, create-operation 1 | `:33-34` |

`[fact]` The error vocabulary is a closed list of 14 codes (`:14-29`), and the conflict-relevant ones are worth copying verbatim: `agent_session_conflict`, `agent_session_claim_unavailable`, `agent_session_ownership_unknown`, `agent_session_checkpoint_stale`, `agent_session_operation_conflict`, `agent_session_operation_expired`, `execution_owner_reconciling`, `execution_owner_unavailable`.

`[inference]` Note the distinction between `agent_session_ownership_unknown` and a conflict. The former is the session-authority analogue of the `unverifiable` verdict: the host declines to assert who owns the session rather than guessing, which is the same discipline as the PTY verdict rule.

### 3.3 Cross-host authority

`[fact]` For orchestration across peers the authority is the **home** runtime's database, and the peer's outbox is pulled and acknowledged with sequence checkpoints:

```
HOME runtime                                      PEER runtime (environment)
   │                                                       │
   │ resolveOrchestrationWorkerServer(environment_id)       │
   │ verify currentServer.peerFingerprint                   │
   │   === federated.peer_fingerprint                       │
   │   else -> OrchestrationError('peer_changed')           │
   │                                                       │
   │ acquireFederationAckLease(dispatchId)                  │
   │ pull page (50 items, max 6 pages per sync) ───────────▶│
   │◀── {runtimeEpoch, items:[{dispatch_id, direction:      │
   │      'to_home', sequence, message_id, kind, payload}]} │
   │ import, then recordFederationAckCheckpoint ───────────▶│
```

| Element | Detail | Cite |
|---------|--------|------|
| Peer identity | `fingerprintOrchestrationPeer(publicKeyB64) = sha256(bytes).base64url` | `src/main/runtime/orchestration/environment-transport.ts:32-34` |
| Peer changed is a hard stop | "Saved environment ... now identifies a different [product] server." | `src/main/runtime/orchestration/federation-sync.ts:71-77` |
| Paging | `FEDERATION_PULL_PAGE_SIZE = 50`, `MAX_FEDERATION_PULL_PAGES_PER_SYNC = 6` | `:17-18` |
| Untrusted input | peer pages are zod-decoded so "a malformed page fails as an orchestration error instead of a TypeError deep inside the import loop" | `:20-38` |
| Ack lease | one ack lease per dispatch, plus `getFederationAckedThrough` / `recordFederationAckCheckpoint` | `:6-10`, `:78` |
| Epoch guard | each page carries `runtimeEpoch` | `:23-24` |
| Capability gate before mutating | any orchestration mutation first calls `status.get` and refuses unless the peer advertises `orchestration.contract.v1`, with "No effects were applied." in the error | `src/main/runtime/runtime-orchestration-federation.ts:80-104` |
| Pairing generation pinning | `expectedEnvironmentPairingRevision` threaded through every call | `environment-transport.ts:14-29` |
| Relevant capabilities | `orchestration.federation.v1`, `.federation-control-mail.v1` (proto 2), `.federation-lifecycle-settlement.v1` (proto 3), `.federation-fleet-snapshot.v1`, `.federation-structured-read.v1` | `src/shared/protocol-version.ts:46-62` |

`[inference]` This is the closest thing in the tree to true multi-box federation, and its shape is store-and-forward mailboxes with monotonic sequences and ack checkpoints, not shared state. That choice is what makes a peer usable while the home client is offline.

`[fact]` But the SSH model explicitly does not get this: orchestration state (Runs, Tasks, Dispatches, mailboxes) is client-resident, and when the client disconnects every CLI command on the SSH host fails with "No owning client is connected to the relay". "The PTY stays `live`; its control plane does not." The doc's advice is blunt: "Commit and push early" (`docs/reference/ssh-execution-boundary.md:51-57`).

---

## 4. Streaming: terminal bytes, agent status, files

### 4.1 Terminal stream framing

`[fact]` A fixed 16-byte binary header, little-endian, with a 64-bit sequence split across two `uint32`s (`src/shared/terminal-stream-protocol.ts:45-80`):

```
 byte 0    1       2        3      4..7        8..11      12..15   16..
┌──────┬───────┬────────┬──────┬──────────┬───────────┬─────────┬──────────┐
│ 0x74 │ ver=1 │ opcode │  0   │ streamId │ seq_high  │ seq_low │ payload  │
└──────┴───────┴────────┴──────┴──────────┴───────────┴─────────┴──────────┘
```

`[fact]` Opcodes (`:12-36`), with direction inferred from names and usage:

| Op | Name | Direction |
|----|------|-----------|
| 1 | `Output` | host -> client |
| 2/3/4 | `SnapshotStart` / `SnapshotChunk` / `SnapshotEnd` | host -> client |
| 5 | `Resized` | host -> client |
| 6 | `Error` | host -> client |
| 7 | `Input` | client -> host |
| 8 | `Resize` | client -> host |
| 9/10 | `Subscribe` / `Unsubscribe` | client -> host |
| 11 | `SnapshotRequest` | client -> host |
| 12 | `Metadata` | host -> client |
| 13 | `Ack` | client -> host (flow control) |
| 14 | `ClaimViewport` | client -> host |
| 15 | `OutputSpan` | host -> client |
| 16 | `SetOutputPaused` | client -> host, **negotiated** |
| 17 | `WriteUnavailable` | host -> client, **negotiated** |

`[fact]` JSON payloads are bounded twice: 8 MiB byte cap and a structural cap of 256 Ki structural tokens and nesting depth 32, enforced before `JSON.parse` (`:6-10`, `:86-97`). `[inference]` The structural limit is a JSON-bomb guard, cheap to copy and easy to forget.

`[fact]` An unknown opcode decodes to `null` and the frame is dropped silently, which is exactly why Rule 2 requires negotiation (`docs/reference/remote-wire-compatibility.md:33-45`).

### 4.2 Chunking, windows, and acks

`[fact]` `src/shared/terminal-multiplex-flow-control.ts:1-13`:

| Constant | Value |
|----------|-------|
| `TERMINAL_STREAM_CHUNK_BYTES` | 48 KiB |
| `TERMINAL_OUTPUT_BATCH_MAX_BYTES` | 64 KiB |
| per-stream ack window, initial / max | 512 KiB / 2 MiB |
| total ack window, initial / max | 2 MiB / 8 MiB |
| `TERMINAL_MULTIPLEX_PENDING_MAX_BYTES` | 256 KiB |
| `TERMINAL_MULTIPLEX_ACK_BATCH_BYTES` | 192 KiB |
| `TERMINAL_MULTIPLEX_ACK_FLUSH_MS` | 4 ms |
| max active streams per connection | 128 |
| max pending PTY waits per connection | 32 |
| limit error | `terminal_stream_limit_exceeded` |

```
 host                                             client
   │  Output frames, <=48 KiB each                   │
   │ ───────────────────────────────────────────────▶ │ xterm write
   │                                                  │
   │                                     pendingAckBytes += n
   │                                                  │
   │  Ack {bytes} or Ack {streamGeneration,           │
   │        ackedEndByte}                             │
   │ ◀─────────────────────────────────────────────── │ at 192 KiB
   │                                                  │ or after 4 ms
   │  credit restored, up to 2 MiB/stream, 8 MiB total│
   │                                                  │
   │  SetOutputPaused {paused:true}  (negotiated)     │
   │ ◀─────────────────────────────────────────────── │ hard stop
```

`[fact]` Two ack dialects coexist: a byte-count ack, and a generation-scoped cumulative `ackedEndByte` when `acknowledgeOutputSourceRanges` and a `streamGeneration` are present (`src/renderer/src/runtime/remote-runtime-terminal-flow-controller.ts:15-36`). `[inference]` The cumulative form is idempotent under retransmission; the byte-count form is not, which is presumably why it is being replaced.

`[fact]` Acks are batched with a size trigger and a timer, so the common case costs no extra round trip (`:76-89`).

`[fact]` A frame is only accepted for a stream still registered under that id: "sendFrame gates on readiness alone; a dropped handle would still report success" (`:38-41`). The same check guards pause (`:59-61`).

### 4.3 Path through the relay

```
remote PTY (peer host)
   │  pty data
   ▼
terminal stream frames (16-byte header)
   │
   ▼
runtime RPC subscription, binary frames on the shared-control socket
   │
   ▼
NaCl box encrypt (nonce || ciphertext)     src/shared/e2ee-crypto.ts:48-60
   │
   ▼
WebSocket frame, no compression
   │
   ▼ relay cell: wireSplice forwards bytes, understands nothing
   │
   ▼
client decrypts, decodes frame, writes to xterm, acks bytes
```

`[fact]` Cipher: `tweetnacl` `box` (X25519 + XSalsa20-Poly1305), with the wire bundle laid out as 24-byte nonce followed by ciphertext (`src/shared/e2ee-crypto.ts:48-60`, `:62-76`). Shared key precomputed with `nacl.box.before` (`:15-17`). Text plaintext cap 4 MiB (`:6`).

`[fact]` Handshake for the relay-transported channel is a bound transcript, not a bare key exchange. `MOBILE_E2EE_V2_PROTOCOL = 'ref-mobile-e2ee'`, framing `2`, payload kinds `['text','binary']`, and a context `{protocol, initiator:'mobile', responder:'desktop', transport:'direct'|'relay', relayHostId?}` (`src/shared/mobile-e2ee-v2-contract.ts:1-32`).

`[fact]` The transcript over which the session is bound length-prefixes 22 named fields including both nonces, both public keys, the negotiated framing and payload kinds, and **every context field including `transport` and `relayHostId`** (`:110-146`). `[inference]` Binding `transport` and `relayHostId` into the transcript is what prevents a relay-carried session from being replayed as a direct one or against a different host id. This is the single most important cryptographic detail to copy.

`[fact]` Validation is exact-record, not permissive: both hello and ready must have precisely the expected key sets, the contexts must be identical, `clientNonce` must be echoed, and all four keys and nonces must be canonical base64 of exactly 32 bytes with a round-trip check (`:45-108`, `:211-226`, `:274-281`). A `relay` transport additionally requires `relayHostId` to match `/^[A-Za-z0-9_-]{16}$/` (`:168-173`).

### 4.4 Replay and recovery on reconnect

| Layer | Mechanism | Cite |
|-------|-----------|------|
| Logical subscriptions | client re-sends every logical subscription; server re-emits each stream's current snapshot | `src/shared/remote-runtime-subscription-replay.ts:1-9`, `remote-runtime-shared-control-connection-actions.ts` (`replayRuntimeControlSubscriptions`) |
| Freshness gate defeat | a replayed snapshot can carry the same `publicationEpoch`/`snapshotVersion` the client already applied, so monotonic gates would drop it and leave mirrors frozen; the connection tags the first post-replay response as authoritative | `src/shared/runtime-subscription-replay.ts:1-24` |
| Tag is client-side only | "The tag is added client-side after wire parsing, nothing changes on the protocol, so old servers are unaffected" | `:6-9` |
| SSH PTY replay | non-destructive 100 KiB tail with no notion of what this client already consumed | `docs/reference/ssh-reconnect-source-recovery.md:8-14`, `src/relay/pty-handler.ts:340` |
| Two mechanisms, two jobs | "Recovery keeps main's model whole; the tail repaints a new terminal. Substituting one for the other is a category error." | `docs/reference/ssh-reconnect-source-recovery.md:45-47` |
| Peer status recovery | one verification and one retry slot shared by all readers of a connection | `src/shared/runtime-host-status-owner.ts:27-42` |

`[fact]` A residual, honestly recorded rather than papered over: after a reconnect the tab survives but the pane behind it does not reliably rebind, measured at three runs in four against the Docker SSH lane, and the team deliberately did **not** assert it in the spec because "a one-in-four flake in the lane that exists to catch this class costs more than it proves, the lane stops being trusted" (`docs/reference/ssh-reconnect-source-recovery.md:132-147`).

### 4.5 Agent status and file data

`[fact]` Agent status travels as ordinary published frame content on the same subscription surface, and the wire-compatibility doc uses it as the worked example of Rule 3: a host stopped synthesizing a finished agent status, and clients running older code saw different content in an identical frame (`docs/reference/remote-wire-compatibility.md:61-67`). There are dedicated references at `docs/reference/agent-status-store.md`, `renderer-agent-status-performance.md`, and `agent-pty-transcript-capture.md`.

`[fact]` File and git data are plain RPC, not a separate stream, with per-chunk streaming where payloads are large: `STREAM_CHUNK_SIZE = 256 * 1024`, mirroring the upstream editor's `bufferSize`, with the note "256KB raw -> ~340KB base64, well under MAX_MESSAGE_SIZE" (`src/main/ssh/relay-protocol.ts:61-64`). Git responses can stream as `git.responseChunk` frames behind a sentinel result, absent from old daemons so a new client falls back to the plain result (`:66-70`).

`[inference]` Base64 for binary file chunks inside JSON-RPC on the SSH channel is a 33% overhead the peer path avoids by having native binary frames. A new design should use binary frames everywhere.

---

## 5. Self-hosting the relay

### 5.1 What it takes

`[fact]` A single-process, single-node deployment is genuinely supported:

| Requirement | Detail | Cite |
|-------------|--------|------|
| Role | `ref_RELAY_ROLE=combined` is the default; the same image is director, cell, or both | `cloud/apps/relay/src/config.ts:42` |
| Only mandatory secret | `ref_RELAY_ASSIGNMENT_SIGNING_KEY`, at least 32 bytes. "the only required value; everything else has a local default" | `cloud/README.md:126-129`, `config.ts:41` |
| Database | PostgreSQL in production, SQLite for tests and local dev, via Node's built-in `node:sqlite` | `cloud/README.md:51-52`, `cloud/apps/relay/src/database.ts:4`, `:673` |
| Data dir | `ref_RELAY_DATA_DIR`, default `./data/relay` | `config.ts:112` |
| Container | 2-stage Alpine build, `USER node`, `EXPOSE 8080`, `CMD node apps/relay/dist/index.js` | `cloud/apps/relay/Dockerfile:1-29` |
| Test Postgres | one `docker run postgres:16-alpine` with a URL in `ref_RELAY_TEST_POSTGRES_URL` | `cloud/README.md:118-124` |
| No Redis | no Redis, Kafka, or message broker anywhere in the relay | grep of `cloud/apps/relay/src` and the Terraform root returns none `[inference]` |

`[fact]` The unavoidable external dependency is an **OIDC identity provider**. The relay requires `ref_RELAY_AUTH_ISSUER`, `ref_RELAY_JWKS_URL`, audience `ref-relay`, and verifies `ES256` tokens minted elsewhere (`config.ts:38-40`, `relay-token-verifier.ts:16-30`). `[fact]` The auth service itself is not in this repo: "the API and auth services live in the private ... repository" (`cloud/README.md:98-103`).

`[fact]` `/ready` is dependency-backed: it checks the database and the JWKS endpoint (`createRelayReadiness(observedDatabase, config.jwksUrl, ...)`, `relay-server.ts:118-120`).

`[fact]` The `ref_RELAY_ADMIN_JWKS_URL` default is Google's OAuth2 certs endpoint and admin routes authenticate with Google identity tokens bound to an exact audience (`config.ts:79`, `cloud/docs/ref-relay-operations.md:20-31`). `[inference]` Self-hosting outside that cloud means replacing the admin auth path, which is roughly 40 admin routes.

### 5.2 What the shipped production topology costs

`[fact]` Terraform resource census (`cloud/infra/terraform/*.tf`, `grep -c 'resource "google_..."'`):

| Resource type | Count |
|---------------|-------|
| `google_project_iam_member` | 35 |
| `google_service_account_iam_member` | 20 |
| `google_service_account` | 13 |
| `google_monitoring_alert_policy` | 13 |
| `google_secret_manager_secret_iam_member` | 11 |
| `google_storage_bucket_iam_member` | 10 |
| `google_iam_workload_identity_pool_provider` | 9 |
| `google_project_iam_custom_role` | 7 |
| `google_secret_manager_secret` (+ 4 versions) | 5 |
| `google_artifact_registry_repository_iam_member` | 5 |
| `google_cloud_run_domain_mapping` | 3 |
| `google_sql_database_instance` (+ 2 dbs, 2 users) | 1 |
| `google_compute_*` network, subnets, router, NAT, firewall, health checks, URL map, HTTPS proxy | ~14 |

`[fact]` Shape of the deployment: a Cloud Run director, GCE managed instance groups as cells behind a Google load balancer with an HTTPS proxy and per-cell DNS, one shared Cloud SQL instance in `us-central1`, and a dedicated database for the push gateway (`cloud/README.md:63-68`, `relay-gce-cells.tf`, `relay-database.tf`, `relay-dns.tf`, `push-dedicated-database.tf`).

`[fact]` Cells are fixed-one `RECREATE` managed instance groups with private-only networking and durable incarnation ids; a candidate must be a **new** cell id and "Never replace the backend behind an existing cell origin" (`cloud/docs/ref-relay-operations.md:84-90`).

`[fact]` Operational surface: 25 `cloud-*.yml` workflows plus a Cloud SQL rollout lease action that serialises rollouts against the shared instance (`cloud/README.md:78-86`). All of them are gated inert on a repository variable (`:88-92`).

`[inference]` Cost read: a self-hosted single-node relay is a Docker container plus SQLite plus an OIDC issuer, which is a weekend. The shipped topology is a multi-cell, multi-region, migration-capable system whose operational machinery (fencing, evacuation, rehoming, capacity proofs, incident monitor) is far larger than the data plane it protects. The data plane that actually moves bytes is roughly 110 lines (`splice-forwarder.ts`).

### 5.3 Component sizes

`[fact]` Non-test line counts (`find ... ! -name '*.test.ts' | xargs wc -l`):

| Component | Path | Non-test LOC |
|-----------|------|--------------|
| SSH host support (client side) | `src/main/ssh` (depth 1) | 32,143 |
| Remote daemon for SSH hosts | `src/relay` | 28,856 |
| Runtime RPC server | `src/main/runtime/rpc` + `runtime-rpc` | 36,182 |
| Renderer runtime and terminal multiplexer | `src/renderer/src/runtime` | 30,235 |
| Cloud relay server | `cloud/apps/relay/src` | 18,514 |
| Remote runtime client (shared) | `src/shared/remote-runtime-*.ts` | 4,971 |
| Desktop relay client | `src/main/runtime/relay` | 4,065 |
| Relay ops console | `cloud/apps/relay-ops/src` | 3,886 |
| Push gateway | `cloud/apps/push/src` | 2,736 |
| Relay wire contract | `cloud/packages/relay-contract/src` | 990 |
| Fence broker | `cloud/apps/relay-fence-broker/src` | 645 |
| Mobile client | `mobile` | 32,996 |

`[inference]` The wire contract is under 1,000 lines while the server that implements it is 18,500. Almost all of that 18x is admission control, assignment durability, migration, and regional operations, not message handling. A design that does not need multi-cell migration can skip most of it.

---

## 6. Security model

### 6.1 Trust boundaries

```
┌────────────────────────────────────────────────────────────────────┐
│ CLIENT                                                             │
│  holds: deviceToken per peer, X25519 keypair, ssh credentials       │
│  trusts: nothing it cannot verify; never substitutes local for      │
│          remote execution                                           │
└──────────┬──────────────────────┬──────────────────────────────────┘
           │                      │
           │ ssh (verified        │ WebSocket + NaCl box
           │ host key, D1)        │
           ▼                      ▼
┌────────────────────┐   ┌──────────────────────────────────────────┐
│ SSH HOST           │   │ PEER HOST                                │
│ owns execution,    │   │ owns execution AND control plane;         │
│ tools, creds,      │   │ authorizes per-device token + scope       │
│ identity, env      │   │ mobile scope -> method allowlist          │
└────────────────────┘   └──────────────────────────────────────────┘
                                     ▲
                                     │ spliced bytes (opaque)
                         ┌───────────┴────────────┐
                         │ RELAY CELL             │
                         │ SEMI-TRUSTED           │
                         │ sees metadata only     │
                         └────────────────────────┘
```

### 6.2 Token and credential lifetimes

`[fact]` All from `cloud/packages/relay-contract/src/protocol-limits.ts` and `control-continuity.ts` unless noted:

| Credential | Lifetime | Cite |
|------------|----------|------|
| Relay host-control JWT | 5 min (`relayTokenTtlMs`) | `protocol-limits.ts:19` |
| Auth refresh window | 60 to 120 s before expiry | `control-continuity.ts:11-12` |
| Expired auth, existing splice grace | 60 s | `protocol-limits.ts:20`, `control-continuity.ts:13` |
| Pairing invite | 10 min, max 5 attempts, 2 s cooldown per attempt, 15 s reservation lease | `protocol-limits.ts:12-15` |
| Invite in the offer schema | max 10 min + 30 s skew leeway | `src/shared/mobile-relay-pairing-offer.ts:13-17` |
| Resume credential | 30 days, versioned, with an optional grace version | `protocol-limits.ts:18`, `credential-messages.ts:26-36` |
| Resume confirmation deadline | 30 s | `protocol-limits.ts:17` |
| Host attach deadline | 10 s | `protocol-limits.ts:16`, mirrored `src/main/runtime/relay/relay-control-protocol.ts:40` |
| Control orphan grace | 30 s | `control-continuity.ts:10` |
| Idle socket timeout | 10 min | `protocol-limits.ts:11` |
| First frame deadline | 2 s | `protocol-limits.ts:2` |
| Per-device runtime token | no expiry in the record; revocable per device | `src/main/runtime/device-registry.ts:25-40` |

`[inference]` The per-device runtime token having no expiry is the weakest link in the peer model. Revocation is the only control, and the registry is a JSON file on the host.

### 6.3 What a compromised relay can and cannot see

`[fact]` The relay's own database stores identities, routing, and **hashes**, never payload. `cloud/apps/relay/src/database.ts:70-200`:

| Table | Sensitive columns |
|-------|-------------------|
| `relay_invites` | `user_id`, `relay_host_id`, `relay_device_id`, `token_hash` (unique), state, attempt counts, expiries |
| `relay_devices` | `current_hash`, `current_version`, `current_expires_at`, `grace_hash`, `grace_version`, `revoked_at` |
| `relay_connection_bases` | `basis_conn_id`, ids, `credential_kind`, `invite_token_hash`, deadlines |
| `relay_assignments` | `cell_id`, `assignment_epoch`, lease expiry, last activity, reservation counters |
| `relay_assignment_region_preferences` | `preferred_region`, `observed_at` |

`[fact]` Credentials are stored as `sha256(token).base64url` (`cloud/apps/relay/src/credential-store.ts:47`).

**Can see** `[fact]` unless noted:
- `user_id`, `profileId`, `organizationId` (they are in the verified JWT claims and the host-proof transcript: `relay-token-verifier.ts:5-12`, `host-proof-transcript.ts:72-76`).
- `relayHostId`, `relayDeviceId`, `hostPublicKeyB64` (sent in `host-hello`, `control-messages.ts:21-31`).
- The outer invite or resume credential **in cleartext** on the first frame, because it is the relay's own credential (`credential-messages.ts:4-6`). Stored hashed.
- Source IP of both peers, with the caveat about which forwarded hop is trustworthy (`relay-server.ts:66-75`).
- Exact byte counts and timing per splice (`onForwardedBytes`, `splice-forwarder.ts:130`, `:142`), and message boundaries and direction.
- `appVersion` (`control-messages.ts:27`).
- `preferredRegion` (`director-messages.ts:20`).

**Cannot see** `[inference, from the cipher and the splice being byte-opaque]`:
- Any frame payload: terminal bytes, file contents, diffs, prompts, agent output, RPC method names and params. The splice forwards `RawData` without parsing (`splice-forwarder.ts:121-146`), and payloads are `nacl.box` AEAD under a key derived from the two endpoint X25519 keys (`e2ee-crypto.ts:15-17`, `:48-60`).

**Cannot do** `[inference]`:
- Inject or modify frames undetected. Poly1305 authenticates, and the handshake transcript binds both nonces, both public keys, the negotiated framing, `transport`, and `relayHostId` (`mobile-e2ee-v2-contract.ts:110-146`).
- Impersonate the host to a client. The desktop's public key is pinned in the pairing offer and `relayHostId` is verified from its decoded bytes (`src/shared/mobile-relay-pairing-offer.ts:74-77`, `:92-100`).
- Redirect a credential-bearing client to an attacker cell. Only the configured director origin, and only forward in epoch (`director-messages.ts:58-69`).

**Can do** `[inference]`:
- Deny service: refuse, drop, delay, or close any session; 4404 and 4409 are indistinguishable from the truth to a client.
- Correlate: build a complete graph of which user, which host, which device, when, how much, for how long, from which IPs.

`[fact]` The operational rules acknowledge the metadata sensitivity: "Never put relay JWTs, access tokens, invite/resume credentials, or signing keys in URLs, shell history, logs, or reports" (`cloud/docs/ref-relay-operations.md:9`), "A drain response contains only `recovery: resolve-director`. Never provide a recovery URL from a cell" (`:12`), operator logs contain "only resource names, epochs, aggregate counts, and response codes" (`:43`), and for the push gateway "Logging is aggregate counters only. Tokens, notification titles, notification bodies, and full host fingerprints never reach a log line" (`cloud/README.md:59-60`).

`[fact]` Host identifiers are digested in relay logs (`relayHostLogDigest`, used at `host-session-registry.ts:449-452`).

### 6.4 Other hardening worth copying

| Control | Detail | Cite |
|---------|--------|------|
| Malformed percent-escape is a client error | a `URIError` thrown out of the `upgrade` listener is uncaught and kills the process | `relay-server.ts:36-44` |
| Query strings rejected on upgrade | any `url.search` -> 400 | `relay-server.ts:290-293` |
| First frame must be text | binary first frame -> 4401 | `relay-server.ts:261-265` |
| ws receiver errors guarded | "a ws receiver error (oversize or malformed frame) with no 'error' listener throws process-wide" | `relay-server.ts:57-64` |
| Control socket payload cap is smaller | 1 MiB on `/v1/host/control` vs 8 MiB on data sockets | `relay-server.ts:105-110` vs `:157-168` |
| Phone hangup detected mid-accept | `abandonedByClient` checks between each serialized Postgres call, because finishing work for a phone that hung up held an activity lease for the full 10 s attach deadline | `host-session-registry.ts:215-227` |
| Host data ticket is fully bound | conn ticket **and** control generation **and** session state all checked | `host-session-registry.ts:381-391` |
| Secure-file pattern for local secrets | `writeSecureJsonFile` / `hardenExistingSecureFile` | `src/main/runtime/device-registry.ts:8-12` |

`[fact]` One defence-evasion lesson is recorded in `docs/reference/windows-daemon-host-relocation.md:35-45`: an earlier revision copied the daemon host executable under a different name so a kill-by-image-name could not match it, and "cost a textbook defence-evasion signature: a process copies its own image into a user-writable directory under a different name so a kill-by-image-name cannot match it, then runs detached and survives the installer." Endpoint protection flagged it. The fix was to keep the app's own file name and rely on the path being out of the installer's path-scoped sweep, because "Survival is a property of the path."

`[inference]` Directly relevant to a Rust daemon on Windows: relocating a daemon binary to survive updates is fine, renaming it is not.

### 6.5 Known gaps in the reference implementation

| Gap | Status | Cite |
|-----|--------|------|
| SSH host key verification | documented defect with a designed fix; every ssh2 connection accepted every host key | `docs/reference/ssh-host-key-verification.md:6-15` |
| GitHub/GitLab API calls run on the client | in-doc "inconsistent with the rule; PRs carry the client's identity" | `docs/reference/ssh-execution-boundary.md:29` |
| Checkpointed source recovery dead on SSH reconnect | verified, five-link proof, fix designed but with named unverified preconditions | `docs/reference/ssh-reconnect-source-recovery.md:61-105` |
| Per-device runtime token has no expiry | revocation only | `src/main/runtime/device-registry.ts:25-40` `[inference]` |
| No standalone relay threat-model document | a grep for "threat model" across `cloud/docs` and `cloud/README.md` returns one unrelated hit; the threat reasoning lives in the SSH host-key doc and the operations runbook instead | `[fact]` from grep output |
| No orphan-PTY sweep safety in a different PID namespace | "a process the host's own `ps` cannot enumerate ... is unobservable while `killpg` still reaches it" | `docs/reference/ssh-execution-boundary.md:86` |

`[fact]` The orphan-sweep analysis is the best single example of the reference implementation's evidence discipline and is worth reading in full (`docs/reference/ssh-execution-boundary.md:75-88`). Its general rule: "**evidence must be measured in the unit the destructive action operates on.** Evidence in a different unit is `unverifiable` no matter how precise it looks." Concretely, `shellOwnsEveryTtyProcessGroup` requires two measurements, not one, because `set +m` hides a background job in the shell's own process group and `ioctl(TIOCNOTTY)` without `setsid` drops the controlling terminal while keeping the pgid, so neither a tty-shaped predicate nor a `ppid` walk can see the victim that `killpg` will still reach.

---

## 7. Adopt / Adapt / Avoid

### Adopt as-is

| Item | Why | Cite |
|------|-----|------|
| Three-kind host id union with a parse function returning a tagged enum | tiny, total, and it forces every routing decision through one place | `src/shared/execution-host.ts:8-16`, `:71-112` |
| Omitted scope means "this host", never fan-out | a default fan-out is a foot-gun that turns a cheap call into an N-host call | `:126-130` |
| Coverage census on every bounded listing, and "absent scope is never completeness" | the only honest way to report a partial answer across hosts | `src/shared/runtime-listing-host-scope.ts:9-40` |
| Per-host round-robin under a row cap | without it a remote host silently vanishes from the UI | `src/shared/host-balanced-listing-page.ts:11-56` |
| `live` / `unverifiable` / `exited` with no synonyms and no collapsing | the central discipline of the whole system | `docs/reference/ssh-execution-boundary.md:14` |
| Client-minted operation ids with host-side commit and replay | solves lost-response duplication and two-client races with one mechanism | `src/shared/agent-session-host-authority.ts:39-48`, repro at `config/scripts/remote-agent-session-authority-repro.mjs` |
| Host-proof transcript with length-prefixed fields, binding origin, epoch, identity, and transport | prevents cross-context replay | `cloud/packages/relay-contract/src/host-proof-transcript.ts:60-103` |
| E2EE handshake transcript binding `transport` and `relayHostId` | stops a relay-carried session being replayed as direct or against another host | `src/shared/mobile-e2ee-v2-contract.ts:110-146` |
| Ack-window flow control with batched acks (size trigger + short timer) | bounded memory, no per-frame round trip | `src/shared/terminal-multiplex-flow-control.ts:1-13` |
| Cumulative `ackedEndByte` over byte-count acks | idempotent under retransmission | `src/renderer/src/runtime/remote-runtime-terminal-flow-controller.ts:15-36` |
| JSON structural limits (token count + nesting depth) before parse | one-line JSON-bomb guard | `src/shared/terminal-stream-protocol.ts:7-10`, `:86-97` |
| Per-device revocable tokens with a scope, plus a method allowlist for the weaker scope | compromise of one client does not widen | `src/main/runtime/device-registry.ts:1-4`, `runtime-rpc-websocket-dispatch.ts:76-87` |
| Semantic protocol version in the socket path, not a build hash | see Avoid below | `docs/reference/ssh-execution-boundary.md:49` |
| Derive cross-version test expectations from the checked-out baseline, never "the old side lacks X" | stops a rolling baseline from reddening unrelated work | `docs/reference/remote-wire-compatibility.md:100-130` |

### Adapt

| Item | Adaptation |
|------|-----------|
| Splice forwarder (~110 lines) | the algorithm transfers directly to Rust (`tokio` + `tungstenite`), but replace the `_socket.pause()/resume()` reach-through with real `Sink`/`Stream` backpressure |
| `.strict()` on every wire schema | keep it for control frames, but add a **capability field from day one** rather than discovering you need an HTTP header to carry it (`control-messages.ts:47-52`) |
| Admission source derivation | `relay-server.ts:66-75` is specific to one cloud's forwarded-header semantics; rewrite for whatever proxy sits in front |
| Regional placement | the probe-and-cache algorithm is sound but only pays off with multiple cells; skip until there are |
| 8 MiB max frame | the real fix named in-code is paginating the catalog, not raising the cap (`protocol-limits.ts:4-8`) |
| Base64 file chunks in JSON-RPC | use binary frames; the peer path already does |
| Status owner with one shared retry slot | good pattern, but the reference version polls every paired peer, which is the wrong default for a large fleet |

### Avoid

| Anti-pattern | Consequence | Cite |
|--------------|-------------|------|
| Namespacing a remote daemon's socket path by a **build content hash** | every app update permanently orphans live remote work: running, unreachable, never `exited`, and the old install directory stays pinned against GC | `docs/reference/ssh-execution-boundary.md:43-49` |
| Reusing a client identity across a reconnect | the reference implementation's whole checkpoint-recovery mechanism is inert on the SSH path because a reconnected client presents the same `clientId` | `docs/reference/ssh-reconnect-source-recovery.md:61-80` |
| A new stream opcode without negotiation | unknown opcodes are dropped silently; the feature appears to hang and input is swallowed | `docs/reference/remote-wire-compatibility.md:31-45` |
| Client-resident orchestration state for a host that must work offline | every CLI command on the remote box fails the moment the client disconnects | `docs/reference/ssh-execution-boundary.md:51-57` |
| Registering one machine as both a dumb SSH host and a peer | splits its worktrees across two identities and makes listings depend on which flag you passed | `docs/reference/ssh-execution-boundary.md:98-101` |
| Falling back to local execution when a remote provider is missing | "a local run can answer for the *wrong repository*" | `docs/reference/ssh-execution-boundary.md:11` |
| Treating loss of contact as process death | orphans live work and can cold-start a duplicate over the same worktree | `docs/reference/ssh-execution-boundary.md:73` |
| Renaming a daemon binary to dodge a kill-by-image-name | textbook defence-evasion signature; flagged by endpoint protection | `docs/reference/windows-daemon-host-relocation.md:35-45` |
| Building multi-cell migration machinery before you have multiple cells | the operational tooling (fencing, evacuation, rehoming, capacity proofs) dwarfs the 110-line data plane it protects | `[inference]` from the LOC table in §5.3 |

---

## 8. Decision inputs for us

Context assumed: a Rust daemon plus a Tauri desktop, today planning only `ssh -L` socket forwarding per host.

### 8.1 Verdict up front

**Ship the peer model over a plain WebSocket, keep `ssh -L` as one transport option, and do not build a relay.** For laptop-to-tailnet-box, a relay buys nothing that a tailnet does not already give you. Build the relay only if and when you must support a client that cannot dial the box at all, which in practice means a phone on a cellular network, and even then the mobile companion is a separate product decision.

### 8.2 Is a relay needed for laptop-to-tailnet-box?

**No.** `[fact]` The reference implementation agrees, twice over:

- The relay's stated purpose is phone-to-desktop only (`cloud/README.md:3-7`), and a `relay` block on a `runtime`-scoped pairing offer is a hard validation error because it "would imply routing and credential support that client does not have" (`src/shared/mobile-relay-pairing-offer.ts:82-91`). Desktop-to-peer federation never uses the relay.
- Tailnet is the first-class answer for peer reach. There is a dedicated module that recognises tailnet addresses and, on a connection failure, tells the user to "connect both devices to Tailscale and pair using its Tailscale address" (`src/shared/remote-runtime-tailscale-hint.ts:60-80`).

`[fact]` A peer environment can even record `connectionDependency: 'ssh-tunnel'` (`src/shared/runtime-environments.ts:33`), i.e. the reference implementation already treats an SSH tunnel as a legitimate carrier for the peer WebSocket. That is precisely the shape of a `ssh -L` plan, and it composes with the peer model rather than competing with it.

### 8.3 What a relay would buy beyond tailnet or SSH

| Capability | Relay | Tailnet | `ssh -L` |
|-----------|-------|---------|----------|
| Reach a box behind symmetric NAT with no VPN | yes | no | no |
| A phone client with no VPN profile | yes | needs the app | no |
| Works when the user cannot install software on the network path | yes | no | no |
| Zero client-side network config | yes | no | no |
| Region-aware latency optimisation | yes | tailnet DERP does this | no |
| Survives the box's IP changing | yes (director reassigns) | yes (tailnet identity) | no |
| Cost to operate | a service, a database, an OIDC issuer, ~40 admin routes | zero for us | zero |
| Cost to build | see §8.5 | zero | small |
| Metadata exposure | a correlatable graph of user/host/device/time/bytes | tailnet operator sees similar | none |

`[inference]` The only column where the relay wins uniquely is "no VPN, no config, arbitrary network", and that is a consumer-mobile requirement, not a laptop-to-box requirement. Everything else tailnet already provides at zero build and zero operational cost.

### 8.4 Minimum viable federation

Target: one desktop UI, N remote boxes, live terminal streaming, agent sessions that survive the client going away.

```
┌────────────────────────────────────────────────────────────────┐
│ TAURI DESKTOP                                                  │
│  HostRegistry: Vec<Host>  (Local | Remote{endpoint, token, pk})│
│  one PeerConnection per remote, all live                       │
│  listings: fan out, merge, return {covered[], omitted[]}       │
└──────────────┬─────────────────────────────────────────────────┘
               │ WebSocket (direct, tailnet, or through ssh -L)
               │ frames: NaCl box or Noise
               ▼
┌────────────────────────────────────────────────────────────────┐
│ RUST DAEMON on the box (the SAME binary)                       │
│  owns: PTYs, git, fs, agent sessions, its own sqlite           │
│  authz: per-device token + scope                               │
│  idempotency: op_id -> committed result (replay on retry)      │
│  detached from any client; survives disconnect                 │
└────────────────────────────────────────────────────────────────┘
```

Seven components, in dependency order:

| # | Component | What it must do | Reference to copy | Est. Rust LOC |
|---|-----------|-----------------|-------------------|---------------|
| 1 | **Host identity** | closed enum `Local \| Remote(id)`, total parse, `Scope::All` distinct from `Scope::One`, omitted scope means this host | `src/shared/execution-host.ts:8-130` | 200 |
| 2 | **Wire contract crate** | one crate, versioned; RPC envelope; terminal stream framing with a 16-byte header; capability strings; close codes; explicit "unknown opcode" handling | `src/shared/runtime-rpc-envelope.ts`, `terminal-stream-protocol.ts`, `cloud/packages/relay-contract` (990 LOC total) | 600 |
| 3 | **Peer transport** | one WebSocket per peer; N logical requests and subscriptions over it; reconnect with backoff; subscription replay; replayed-snapshot tagging; keepalive | `RemoteRuntimeSharedControlConnection` (`src/shared/remote-runtime-shared-control-connection.ts`) + the `remote-runtime-*` family (4,971 LOC of TS) | 1,500 |
| 4 | **Pairing and E2EE** | X25519 + AEAD (use `snow`/Noise `IK` rather than reimplementing box); handshake transcript binding both keys, both nonces, framing, transport, host id; pairing offer as a versioned struct in a deep link; per-device revocable token with a scope | `mobile-e2ee-v2-contract.ts:110-146`, `mobile-relay-pairing-offer.ts`, `device-registry.ts` | 800 |
| 5 | **Terminal streaming with flow control** | 48 KiB chunks; per-stream 512 KiB initial / 2 MiB max window; 8 MiB total; batched cumulative acks at 192 KiB or 4 ms; negotiated pause; bounded replay tail on reconnect | `terminal-multiplex-flow-control.ts:1-13`, `remote-runtime-terminal-flow-controller.ts:15-89` | 900 |
| 6 | **Session authority** | timestamped op ids with freshness bounds; commit-then-reply; `replayed` / `adopted` / `created` dispositions; a closed error vocabulary including an explicit "ownership unknown"; host owns presentation | `agent-session-host-authority.ts:14-48`, repro script | 700 |
| 7 | **UI aggregation** | fan-out with a coverage census; per-host round robin under a row cap; per-host health and capability badges; per-peer status with one shared retry slot | `runtime-listing-host-scope.ts`, `host-balanced-listing-page.ts`, `runtime-host-status-owner.ts` | 600 |

`[inference]` Total roughly **5,300 Rust LOC** for the federation layer, on top of a daemon that already does PTYs, git, and filesystem locally. Effort: **6 to 9 engineer-weeks** for one strong engineer, of which items 4, 5, and 6 are the hard three and item 6 is the one most likely to be underestimated. The reference implementation spends 4,971 TS lines on item 3 alone; Rust will be tighter but the state machines are the same.

Excluded deliberately, with the trigger for revisiting:

| Excluded | Add when |
|----------|----------|
| Cloud relay (director, cells, splice, assignment, credential rotation, fencing) | a client genuinely cannot dial the box. See §8.5 |
| Push gateway | you ship a mobile app that needs background banners |
| Regional placement | you have more than one relay cell |
| Cross-host orchestration federation (mailbox relay, ack checkpoints, peer fingerprints) | a peer must run dispatches while the home client is offline |
| Cell fencing, evacuation, rehoming | you operate relay cells in production |
| Mixed-version cross-build test harness | clients and daemons update independently, which for a self-updating pair they will, so plan for this by month 3 |

### 8.5 If you later need the relay

`[inference]` Scope for a minimum useful self-hosted relay, based on what the reference implementation's 18,514 lines actually spend themselves on:

| Piece | Needed for MVP | Est. Rust LOC |
|-------|----------------|---------------|
| Three WebSocket endpoints (host control, client connect, host data) | yes | 400 |
| Splice forwarder with high/low/hard watermarks and a wedge timer | yes | 250 |
| Splice state machine, forward-only, with the "both handlers installed" guard | yes | 150 |
| Host-proof challenge/ack over a bound transcript | yes | 300 |
| Invite and resume credential store, hashed, versioned, with a grace window | yes | 600 |
| Pre-auth admission limits per source and in total | yes | 200 |
| Close-code taxonomy and client recovery routing | yes | 150 |
| Assignment director, epochs, leases, `/v1/resolve` | only with more than one cell | 1,200 |
| Regional placement, migration, evacuation, rehoming, fencing | no | 10,000+ |

`[inference]` A single-cell combined-role relay is about **2,000 Rust LOC plus SQLite**, roughly 2 to 3 weeks, and it still needs an OIDC issuer for the host-control token or a simpler substitute. Everything past the first cell is where the cost explodes, so treat "one cell, no migration" as the design point and accept that scaling means a second deployment rather than a migration protocol.

### 8.6 Specific corrections to the current `ssh -L` plan

| Current plan | Problem | Change |
|--------------|---------|--------|
| `ssh -L` socket forwarding per host as the architecture | forwarding is a **transport**, not a host model. It answers "how do bytes get there" and leaves "who owns the control plane" undecided, which is the decision that actually matters | keep `ssh -L` as one of several carriers for the peer WebSocket. The reference implementation already models exactly this as `connectionDependency: 'ssh-tunnel'` (`src/shared/runtime-environments.ts:33`) `[fact]` |
| Client drives the remote box | if the client owns the control plane, nothing on the box works while the laptop is closed, which defeats the point of a remote agent box | put the control plane on the box. One binary, two roles. This is the reference implementation's own conclusion: "For work that must continue while you are offline, use the peer/headless-runtime model on the remote host instead of the direct-SSH model" (`docs/reference/ssh-execution-boundary.md:102`) `[fact]` |
| Implicit: socket path from the build | orphans live work on every update | version the daemon socket by **semantic protocol version** (`daemon-v<N>.sock`), keep earlier versions attachable, and preserve a daemon that holds live sessions across a version change `[fact]` `docs/reference/ssh-execution-boundary.md:49` |
| Implicit: host key trust from ssh2-style defaults | a client library that returns `true` from its host verifier accepts every key | consult the user's real `known_hosts` as a trust source, never write to it, and keep a separate store for hosts you learned yourself `[fact]` `docs/reference/ssh-host-key-verification.md:45-50` |
| Implicit: one host at a time | the whole value proposition is N boxes | build the coverage census and the host-balanced page cap **before** the UI has more than one remote host, because retrofitting them means auditing every listing `[inference]` |

### 8.7 Claim ledger for the decision inputs

| Claim | Label |
|-------|-------|
| The relay is mobile-only and never carries desktop-to-peer federation | `[fact]` `cloud/README.md:3-7`, `mobile-relay-pairing-offer.ts:82-91` |
| Tailnet is the reference implementation's own recommended peer reach | `[fact]` `remote-runtime-tailscale-hint.ts:60-80` |
| An SSH tunnel is already a modelled carrier for the peer WebSocket | `[fact]` `runtime-environments.ts:33` |
| The peer model, not the SSH model, is what survives the client going offline | `[fact]` `docs/reference/ssh-execution-boundary.md:51-57`, `:102` |
| Build-hash socket namespacing permanently orphans live work | `[fact]` `docs/reference/ssh-execution-boundary.md:43-49` |
| The relay data plane is ~110 lines and the other 18,400 are admission, durability, and migration | `[fact]` LOC counts in §5.3 + `splice-forwarder.ts:48-157`; the apportionment between categories is `[inference]` |
| ~5,300 Rust LOC and 6 to 9 engineer-weeks for minimum viable federation | `[inference]` scaled from the TS component sizes in §5.3 |
| ~2,000 Rust LOC and 2 to 3 weeks for a single-cell self-hosted relay | `[inference]` scaled from `cloud/packages/relay-contract` (990) plus the non-migration portion of `cloud/apps/relay/src` |
| A compromised relay sees a full correlation graph but no payload | `[fact]` for the schema and cipher (`database.ts:70-200`, `e2ee-crypto.ts:48-60`); `[inference]` for the completeness of the "cannot see" list |
| Per-device runtime tokens have no expiry | `[fact]` `device-registry.ts:25-40`; that this is the weakest link is `[inference]` |
| The reference UI switches focus between peers rather than merging them | `[fact]` `execution-host.ts:220-227`, `runtime-listing-host-scope.ts:22-25`, `ref-runtime-get-runtime-id.ts:164-172`; the product implication is `[inference]` |

---

## 9. Open questions I could not answer from the tree

1. **Does any client merge listings across `runtime:` peers?** The host-side census deliberately excludes them (`ref-runtime-get-runtime-id.ts:164-172`) and settings hold a single `activeRuntimeEnvironmentId`, but I did not trace the renderer store deeply enough to rule out a client-side merge for some surfaces. `[inference]` Probably not, but unverified.
2. **What is the actual relay cost per host per month?** No pricing, quota, or billing data in the tree. The topology is legible; the bill is not.
3. **How is the host-control JWT minted?** The auth service is in a private repository (`cloud/README.md:98-103`). The claim set and verification are fully visible; the issuance path is not.
4. **Does the peer path have an equivalent of the SSH grace-period kill?** I found the SSH grace window (60s to 7d, default 0) but did not locate a peer-side analogue. `docs/reference/refd-operations.md` is referenced for process-scoped and cgroup-wide stops and would be the place to look.
5. **Is `ClaimViewport` (opcode 14) live or vestigial?** It is in the enum and the comment says older runtimes ignore it and receive a compatibility `Resize` behind it, but I did not trace a live sender.
6. **What is the observed p99 relay latency?** `cloud/docs/ref-relay-capacity-testing.md` exists and would answer this; I did not read it.
7. **Are the 610 catalogued RPC methods all reachable from a `runtime`-scope device?** The mobile allowlist is explicit; the runtime-scope surface appears to be "everything not otherwise gated", but I did not enumerate exceptions.

## 10. Confidence notes

| Area | Confidence | Basis |
|------|-----------|-------|
| Host abstraction, the three kinds, id encoding | **High** | read the whole module and its tests' neighbours |
| SSH model: deploy, launch, attach, framing, survival | **High** | read the launch command, the attach command, the protocol constants, and a dedicated 100-line reference doc |
| Relay endpoints, splice, state machine, backpressure, admission | **High** | read the upgrade router, the forwarder in full, the whole contract package, and the operations runbook |
| Relay auth, proof transcript, credential lifecycle | **High** | read every schema and the transcript builder |
| E2EE framing and what the relay cannot see | **High** on the cipher and the transcript; **Medium** on completeness of the "cannot see" list | cipher and transcript read in full; the negative claim is reasoned from the splice being byte-opaque rather than from an audit |
| Session authority | **High** | contract module plus a 300-line executable repro whose assertions I read |
| Streaming constants and ack protocol | **High** | exact constants from one 13-line module and the flow controller |
| Self-hosting feasibility | **Medium-High** | config schema, Dockerfile, and SQLite support are explicit; I did not actually run it |
| Infrastructure cost shape | **Medium** | resource census is a mechanical count, the interpretation is inference |
| UI aggregation vs focus switching | **Medium-High** | host-side census is confirmed in code; the renderer side is inferred from a single-valued setting |
| LOC and effort estimates | **Low-Medium** | line counts are exact; the Rust translation ratio and the week estimates are judgement |
