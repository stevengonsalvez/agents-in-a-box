Mode: focused-query

# Our Current Surface — agents-in-a-box @ ffb4e362 — 2026-09-11

**Repo** `/home/claude/.ref/agents-in-a-box-desktop-app-part2` (worktree, branch `desktop-app-part2`)
**Origin** https://github.com/stevengonsalvez/agents-in-a-box
**HEAD** `ffb4e362 2026-09-11 13:38:13 +0100 Steven Gonsalvez` [fact]
**Question** what exists today for (a) remote/multi-host, (b) web surface, (c) mobile/push, (d) agent status detection, (e) daemon client protocol, (f) tmux coupling depth — mapped for a redesign.

---

## 0. The one-paragraph answer

Everything is **single-box, single-uid, filesystem-rooted**. The daemon is the real control plane and it is good, but its only door is a `0600` unix socket at `~/.agents-in-a-box/hangar.sock` guarded by `SO_PEERCRED` same-uid plus a token file on the same disk [fact, `ainb-hangar-daemon/src/rpc/mod.rs:256-258`, `rpc/auth.rs:222`]. There is **no host/machine identity anywhere** in the wire protocol or the schema [fact, `ainb-hangar-proto/src/fleet.rs:339-400`, `ainb-hangar-store/migrations/0044_fleet_control_plane.sql:12`]. "Remote" in this codebase means a git remote or an `ssh` session type run inside a *local* tmux pane, never another box's fleet. The only network-reachable surface is `ainb-web`, an axum server that can bind off-loopback behind a bearer token, and it is a *separate renderer* that proxies the CLI. There is no mobile app; "push" is W3C web-push to a browser PWA. Status detection is a three-tier pipeline (lifecycle hooks → `events.jsonl` → attention table; plus tmux `capture-pane` regex as fallback). tmux is load-bearing for interactive sessions: ~126 non-test `Command::new("tmux")` sites, and the *answer delivery* path types digits into a pane and reads the screen back to confirm.

---

## 1. Topology today

```
                          ONE MACHINE, ONE UID
┌──────────────────────────────────────────────────────────────────────────┐
│                                                                          │
│  ┌────────────┐   ┌─────────────┐   ┌──────────────┐  ┌──────────────┐   │
│  │ ainb TUI   │   │ ainb CLI    │   │ ainb-web     │  │ AINBFleet    │   │
│  │ (ainb-core)│   │             │   │ (axum+HTTP)  │  │ .app (Swift) │   │
│  └──┬──────┬──┘   └──────┬──────┘   └──┬────────┬──┘  └──────┬───────┘   │
│     │      │             │             │        │            │           │
│     │  subprocess     direct        own dial  own dial    own dial       │
│     │  plugin (stdio) │             (daemon.rs)│          (Darwin        │
│     │      │          │             │        │            socket())      │
│     │  ┌───▼────────────────┐       │        │            │              │
│     │  │ ainb-plugin-hangar │       │        │            │              │
│     │  │ host unix_socket_  │       │        │            │              │
│     │  │ dial cap           │       │        │            │              │
│     │  └───┬────────────────┘       │        │            │              │
│     │      │                        │        │            │              │
│     │      ▼         ▼              ▼        │            ▼              │
│     │  ┌────────────────────────────────────────────────────────────┐     │
│     │  │  ~/.agents-in-a-box/hangar.sock   mode 0600                │     │
│     │  │  JSON-RPC 2.0 over LSP Content-Length framing              │     │
│     │  │  gate 1: SO_PEERCRED same-uid   gate 2: auth/hello token   │     │
│     │  └───────────────────────┬────────────────────────────────────┘     │
│     │                          │                                          │
│     │                 ┌────────▼──────────────┐                           │
│     │                 │ ainb-hangar-daemon    │ 142k LOC                  │
│     │                 │ 156 dispatch arms     │                           │
│     │                 │ EventBroker(broadcast)│                           │
│     │                 └──┬─────┬──────┬───────┘                           │
│     │                    │     │      │                                   │
│     │        ┌───────────▼┐ ┌──▼───┐ ┌▼─────────────┐                     │
│     │        │ hangar.db  │ │ ACP  │ │ tmux CLI     │                     │
│     │        │ SQLite 96  │ │ pool │ │ send-keys /  │                     │
│     │        │ migrations │ │(stdio│ │ capture-pane │                     │
│     │        └────────────┘ │ PTY- │ │ new-session  │                     │
│     │                       │ free)│ └──────┬───────┘                     │
│     │                       └──────┘        │                             │
│     │                                       │                             │
│     └──── tmux attach / capture-pane ───────┤                             │
│                                             ▼                             │
│                              ┌──────────────────────────┐                 │
│                              │ tmux server (user's own)  │                │
│                              │ agent CLIs live in panes  │                │
│                              └──────────┬────────────────┘                │
│                                         │ lifecycle hooks                 │
│                              ┌──────────▼────────────────┐                │
│                              │ ~/.agents-in-a-box/        │               │
│                              │  events.jsonl  (append)    │               │
│                              │  notify.sock  → notifyd    │               │
│                              │  sessions.json (TUI store) │               │
│                              └────────────────────────────┘                │
│                                                                            │
│  TCP surfaces, both narrow:                                                │
│   127.0.0.1:<port>  webhook ingress (hardcoded loopback)                   │
│   <bind>:<port>     ainb-web  ← ONLY off-box-capable listener              │
└────────────────────────────────────────────────────────────────────────────┘
```

Every client above dials the **same local path**. Nothing in the diagram crosses a machine boundary. [fact]

---

## 2. Crate inventory

Workspace root: `ainb-tui/Cargo.toml` (34 members + `xtask`) [fact, `ainb-tui/Cargo.toml:1-40`].
`default-members = ["crates/ainb-core", "crates/ainb-hangar-daemon"]` — a bare `cargo test` skips every other crate's tests [fact, `ainb-tui/Cargo.toml:38-40`, and `ainb-tui/CLAUDE.md` warns about exactly this].
Total Rust: **707,838 lines** across `ainb-tui` [fact, `wc -l`].

| Crate | LOC | Files | Purpose (one line) |
|---|---:|---:|---|
| `ainb-core` | 252,391 | 457 | The `ainb` TUI + CLI binary: tmux, docker, git, session store, fleet CLI, plugin host |
| `ainb-hangar-daemon` | 142,418 | 237 | The control plane: unix-socket RPC server, FSM claim loop, ACP pool, answer router, schedulers |
| `ainb-plugin-hangar` | 82,164 | 106 | Subprocess TUI plugin: 25 Hangar screens (control centre, fleet, boards, chat, usage) |
| `ainb-hangar-store` | 70,318 | 153 | SQLite persistence: 46 repos, 12 services, 96 migrations |
| `ainb-plugin-burndown` | 18,763 | 19 | Subprocess plugin: usage analytics |
| `ainb-hangar-proto` | 15,722 | 18 | **Pure wire crate**: JSON-RPC envelopes, 159 method consts, fleet/attention/snapshot types |
| `ainb-cli` | 14,679 | 33 | clap surface for the non-TUI commands |
| `ainb-plugin-notifyd` | 12,551 | 18 | Notification daemon: `notify.sock` listener, own rusqlite, OS notifications, hook installer |
| `ainb-plugin-learnings` | 11,272 | 30 | Subprocess plugin: reflect knowledge base browser |
| `ainb-hangar-core` | 9,911 | 43 | Pure IO-free domain types (ids, actors, channels, env policy, token, clock) |
| `ainb-fleet-core` | 9,848 | 27 | **Discovery + classification + tmux send**: pane classifier, jsonl tail, send routing |
| `ainb-plugin-runtime` | 8,977 | 24 | Host-side runtime for subprocess plugins (capability gating incl. `unix_socket_dial`) |
| `ainb-plugin-session-reader` | 8,803 | 16 | Subprocess plugin: canonical local provider usage reader |
| `ainb-skill-core` | 7,708 | 29 | Skill/unit manager business logic |
| `ainb-plugin-witr` | 6,657 | 26 | Subprocess plugin: wraps `witr` process-causality CLI |
| `ainb-plugin-cts-v2` | 4,634 | 28 | 14-axis plugin protocol conformance suite |
| **`ainb-web`** | **4,548** | **13** | **axum dashboard: SSE read surface, WS tmux terminal, VAPID web-push, PWA** |
| `ainb-acp` | 4,294 | 12 | Agent Client Protocol client lib (spawn adapter, reduce updates, write transcript) |
| `ainb-plugin-protocol` | 3,666 | 8 | v2 plugin JSON-RPC protocol + manifest/capability types |
| `ainb-adapters-tool` | 3,051 | 24 | Per-tool install adapters (claude, codex, copilot, cursor, cline, roo, gemini, q, antigravity) |
| `ainb-fleet-tools` | 2,727 | 8 | The fleet Pal's MCP tool server (separate process, own daemon credential) |
| `ainb-plugin-sdk-rust` | 2,033 | 7 | Rust SDK for plugin authors (incl. `host_client.unix_socket_dial`) |
| `ainb-plugin-abtop` | 1,970 | 15 | Subprocess plugin: wraps `abtop` |
| `ainb-adapters-source` | 1,385 | 12 | Skill/agent source adapters (marketplace, manifest, frontmatter, walk) |
| `ainb-hangar-sandbox` | 1,170 | 6 | OS filesystem confinement (Seatbelt profile) for spawned providers |
| `ainb-plugin-testkit` | 1,131 | 4 | In-process plugin test harness |
| **`ainb-hangar-client`** | **1,083** | **2** | **The canonical daemon client**: dial + `auth/hello` + framed JSON-RPC + fleet subscribe |
| `ainb-fetch` | 741 | 7 | Fetcher trait + cache layout |
| `ainb-plugin-types-sessions` | 720 | 1 | Wire schema for the `sessions.usage_data` event |
| `ainb-hangar-secrets` | 596 | 6 | OS keychain bridge |
| `ainb-usage` | 528 | 2 | Per-unit invocation tracking |
| `ainb-model-rates` | 410 | 1 | Model price table |
| `ainb-diff` | 234 | 1 | File diff + pager driver |

**Crates the question asked about that DO NOT EXIST** [fact]:
- no `ainb-ssh`, no `ainb-remote`, no `ainb-gateway`, no `ainb-relay`, no tunnel crate. `grep -liE "tunnel|cloudflared|ngrok|reverse.proxy"` over all of `ainb-tui/crates` returns **nothing**.
- `tailscale` appears exactly once in the whole repo, in `docs/knowledge/reflect-memory/serve.md` (unrelated doc).
- notifyd exists but as a **plugin crate** (`ainb-plugin-notifyd`), not a standalone daemon crate.

### Three separate daemon clients exist

This is the single most duplicated thing in the tree [fact]:

| Client | Location | Notes |
|---|---|---|
| `ainb-hangar-client` | `ainb-hangar-client/src/lib.rs:1-45` | Canonical. Own crate *because* `ainb-fleet-tools` must dial the same socket and `ainb-core` depends on the daemon, so nothing below it can depend back. `ainb-core::fleet::bridge::daemon` re-exports it. |
| `ainb-web`'s own | `ainb-web/src/daemon.rs:1-40` | Re-implements dial + `auth/hello` + Content-Length framing. Header says this is deliberate: web depends only on the *pure* wire crates. Stateless, fresh connection per call. |
| Swift `FleetConnection` | `apps/ainb-fleet-macos/Sources/FleetRPC/FleetConnection.swift:575` | Raw `Darwin.socket(AF_UNIX, SOCK_STREAM, 0)`, own `ContentLengthCodec.swift` (81 lines), own `FleetWire.swift` (1,572 lines of hand-mirrored types). |

That is **three independent implementations of the same framing and the same handshake**, one of them in another language. [fact] Any protocol change is a three-way edit. [inference]

---

## 3. Daemon client protocol

```
client                                      daemon
  │                                           │
  │  connect(~/.agents-in-a-box/hangar.sock)  │  accept()
  │ ─────────────────────────────────────────▶│  gate 1: peer_cred().uid == own uid?
  │                                           │          else close
  │  Content-Length: N\r\n\r\n{auth/hello}    │  gate 2: sha256(token) vs stored digest
  │ ─────────────────────────────────────────▶│          (constant-time verify)
  │ ◀───────────────────────── {result:{}}    │  Caller::Operator | Caller::Pal{scope}
  │                                           │
  │  fleet/negotiate {read_versions,           │  stateless echo: version 2 +
  │                   write_versions}         │  25 capability ids
  │ ─────────────────────────────────────────▶│
  │  workspace/subscribe | fleet/subscribe    │  registers forwarder onto EventBroker
  │ ─────────────────────────────────────────▶│
  │ ◀═══════ hangar/event, fleet/event ═══════│  notifications (no id), same framing,
  │ ◀═══════ fleet/message_event, …  ═════════│  same socket, dedicated writer task
```

| Aspect | Today | Citation |
|---|---|---|
| Transport | `tokio::net::UnixListener` only. **No TCP, no TLS, no WebSocket** for the RPC. | `rpc/mod.rs:71,240,256` [fact] |
| Socket path | `{hangar_home}/hangar.sock`, home = `$AINB_HANGAR_HOME` or `~/.agents-in-a-box` | `rpc/mod.rs:240`; `ainb_hangar_core::hangar_home()` [fact] |
| Socket perms | `0o600` set immediately after bind; stale socket unlinked under the `daemon.lock` single-instance guard | `rpc/mod.rs:243-258` [fact] |
| Framing | LSP-style `Content-Length: N\r\n\r\n` + N bytes of JSON. `MAX_BODY_BYTES = 16 MiB` | `rpc/mod.rs:9-16,102` [fact] |
| Envelope | JSON-RPC 2.0, `{jsonrpc,id,method,params}` / `{jsonrpc,id,result|error}`. `RpcId` = number or string; notifications have no `id` | `ainb-hangar-proto/src/lib.rs:74-119` [fact] |
| Auth gate 1 | `UnixStream::peer_cred()` → `SO_PEERCRED` (Linux) / `LOCAL_PEERCRED`+`getpeereid` (macOS), uid must equal daemon's | `rpc/auth.rs:5-9,211-234` [fact] |
| Auth gate 2 | first frame MUST be `auth/hello {token}`. Token `mdt_…`, CSPRNG-minted at boot, only SHA-256 hex persisted (`daemon_socket_token`, migration 0011), plaintext written once to `{home}/hangar/daemon.token` `0600` | `proto/auth.rs:20-27`, `rpc/auth.rs:20-27` [fact] |
| Unauthorized code | `-32000` | `proto/auth.rs:39` [fact] |
| First-frame deadline | 10 s (`AUTH_FIRST_FRAME_TIMEOUT`) | `rpc/mod.rs:269` [fact] |
| Two caller identities | `Caller::Operator` (full) and `Caller::Pal{scope_key}` (own token, **7 methods only**: `ping`, `fleet/copilot_gate`, `fleet/snapshot`, `attention/list`, `attention/answer`, `fleet/transcript_list`, `fleet/message_send`) | `rpc/auth.rs:55-85` [fact] |
| Method catalogue | **159** `pub const` method names in proto; **156** dispatch arms in the daemon | `proto/methods.rs` (grep count), `rpc/mod.rs` (grep count) [fact] |
| Protocol version | `FLEET_PROTOCOL_VERSION = 2` — fleet-family only. There is **no version on the `hangar/*` or `attention/*` families at all**; `auth/hello` carries no version field | `proto/fleet.rs:14`; `proto/auth.rs:43-47` [fact] |
| Negotiation | `fleet/negotiate` is a **stateless echo**. Client declares read/write version *ranges*; daemon returns its version + 25 capability ids. Holds no connection state | `rpc/mod.rs:1492-1514`, `proto/fleet.rs:110-140` [fact] |
| Capabilities are advisory, NOT authz | Explicitly documented: "Anyone who can call `fleet/transcript_list` on a socket can call `fleet/transcript_prune` on it. The real boundary is the socket itself." | `proto/fleet.rs:50-61` [fact] |
| Subscriptions | 7 subscribe arms: `workspace/subscribe`, `fleet/subscribe`, `fleet/message_subscribe`, `fleet/transcript_subscribe`, `attention/subscribe`, `hangar/issue_subscribe`/`unsubscribe` | `rpc/mod.rs:1233,1406-1407,1420,1440,1442,1462` [fact] |
| Event delivery | `tokio::sync::broadcast`, capacity 256, **best-effort at-most-once**. A lagging consumer drops its oldest events; "the next snapshot pull reconciles authoritatively" | `daemon/src/events.rs:20-44` [fact] |
| Durable replay | A second, **lossless unbounded mpsc** feeds `event_outbox` for `seq`-cursor replay. `fleet/subscribe {after_revision}` returns `Complete` or `SnapshotReset{reason}` (bootstrap / cursor_ahead / replay_limit_exceeded) | `daemon/src/events.rs:64-76`, `proto/fleet.rs:416-445` [fact] |
| Idle bounds | 600 s plain, **86,400 s** when a subscription is live; `MAX_CONNECTIONS = 256` | `rpc/mod.rs:262-267,272` [fact] |
| Error codes | `-32601` method-not-found, `-32602` invalid-params, `-32603` internal, `-32000` permission-denied/unauthorized, `-32006` store-unavailable (SQLite lock contention, a code clients deliberately degrade on) | `rpc/mod.rs:75-97`, `proto/lib.rs:47` [fact] |

### The other TCP listener
`webhook_ingress.rs` is a hand-rolled HTTP/1.1 handler on a `tokio::TcpListener`, **hardcoded `127.0.0.1`** with a comment saying "the untrusted HTTP surface must never listen on a routable interface" [fact, `daemon/src/webhook_ingress.rs:256-268`]. Serves `POST /hangar/webhook/<autopilot-id>`.

---

## 4. Remote hosts / multi-host — **nothing exists**

I went looking for a remote story four ways. All four are negative.

**(a) No host identity in the data model.** `FleetSession` has `session_key`, `provider`, `provider_session_id`, `tmux_target`, `process_start_fingerprint`, `cwd`, `display_name`, lifecycle/attention/management/transport-health, capabilities, provenance, confidence, timestamps, `model`, `version`, `updated_revision`. **No host, hostname, machine, node, or box field** [fact, `proto/fleet.rs:339-400`]. Same for the table: `fleet_session` has no such column [fact, `store/migrations/0044_fleet_control_plane.sql:12-60`]. A grep for `hostname|host_id|machine` across all 96 migrations hits exactly one file, `0028_atc_standup.sql:48`, and that is the word "machine" in an English comment [fact].

**(b) `ssh` is a local session TYPE, not a transport.** `SshTarget {host, port, user, identity_file}` with `to_ssh_command()` that builds the string `ssh -p N -i key user@host` [fact, `ainb-core/src/models/session.rs:492-577`]. It is consumed by `new_ssh_session` and the new-session picker, i.e. the TUI runs that command **inside a local tmux pane** so you get a shell on another box [fact, `models/session.rs:925-944`, `app/state.rs:6195-6218`]. The remote box's own agents, sessions, attention rows and daemon are invisible. No ssh-exec of tmux commands against a remote host; no remote daemon dial.

**(c) `ssh_display_names.json` is dead nomenclature.** It is the *legacy filename* of the durable session-label store, superseded by `session-labels.json`, keyed on **tmux session name** [fact, `ainb-core/src/config/ssh_display_names.rs:38-56`]. Nothing to do with hosts.

**(d) `discover_from_peers` is same-box IPC.** Reads `~/.claude-peers.db` (rusqlite, read-only) and filters by `kill(pid, 0)` liveness — a **local PID check**, so by construction every peer is on this machine [fact, `ainb-fleet-core/src/fleet/discover/peers.rs:12-95`]. The claude-peers broker is described as "flaky" and demoted to fallback [fact, `fleet-core/src/fleet/send/route.rs:14-16`].

**"remote" in `ainb-fleet-core` means a git remote.** `repo_clone.rs` clones a remote-only favourite into `~/.agents-in-a-box/clones/<tail>-<blake3>` [fact, `fleet-core/src/fleet/repo_clone.rs:1-35`].

### What the TUI can show about another box: **nothing.** [fact]
There is no code path. The nearest thing is running `ssh` in a pane and looking at that box's terminal by hand.

### The only off-box-capable listener
`ainb-web` may bind a non-loopback address, gated by `check_bind_security()`:

```
token set?                       ──▶ allow
--insecure-bind AND --read-only  ──▶ allow (every write surface gated off)
--insecure-bind AND writable     ──▶ REFUSE  (WS terminal == shell on every session)
loopback ip?                     ──▶ allow
otherwise                        ──▶ REFUSE
```
[fact, `ainb-web/src/config.rs:64-95`]. So today "remote access" = expose the web dashboard with a bearer token, and reach the box's tmux through the WS terminal.

---

## 5. `ainb-web` — the web/browser surface

4,548 LOC, 13 files, axum. [fact]

```
browser (PWA)
   │  GET /                       embedded vanilla JS/CSS (rust-embed)
   │  GET /api/{snapshot,sessions,needs,cost}
   │  GET /api/events             SSE, pushed on fingerprint change
   │  POST /api/answer            ──┐ write surface
   │  WS  /ws/session/:id         ──┤ write surface
   │  POST /api/push/subscribe      │
   ▼                                │
┌──────────────────────────┐        │
│ axum router              │        │
│  bearer middleware on    │        │
│  /api/* (+ ?token= only  │        │
│  for SSE and WS)         │        │
│  read_only_gate on the   │◀───────┘
│  two write surfaces      │
└───┬──────────┬───────────┘
    │          │
    │          └──▶ DaemonClient (own dial) ──▶ hangar.sock
    │                 attention/list, attention/answer
    │
    └──▶ AinbCliSource ──▶ spawns `ainb --format json list` / `fleet cost`
                              (self via std::env::current_exe fallback)
```

| Question | Answer | Citation |
|---|---|---|
| What it serves | SPA shell + session list + fleet `needs` (ASK/ERR/IDLE/WAIT) + cost rollups + live terminal + PWA/push | `ainb-web/src/lib.rs:1-32` [fact] |
| Frontend | `frontend/` embedded via `rust-embed`, **no Node build step**: `index.html`, `app.js`, `styles.css`, `sw.js`, `manifest.webmanifest`, `xterm.js` + fit addon (vendored) | `ainb-web/src/assets.rs:1-14`, `ls ainb-web/frontend` [fact] |
| Routes | `/healthz`, `/api/snapshot`, `/api/sessions`, `/api/needs`, `/api/cost`, `/api/events` (SSE), `POST /api/answer`, `/ws/session/:id`, `/api/push/*`, `/`, `/static/*`, `/manifest.webmanifest`, `/sw.js` | `ainb-web/src/routes.rs:235-280` [fact] |
| Data source | **shells out to its own CLI**: `ainb --format json list`, `ainb --format json fleet cost`, kept as opaque `serde_json::Value` so new CLI fields flow through with no code change. Deliberate: "the browser view can never drift from the CLI/TUI" | `ainb-web/src/data.rs:1-6,38-56,200-213` [fact] |
| Needs source | NOT the CLI — `attention/list` RPC on the daemon. Answers route through `attention/answer` so "the web process never touches tmux" | `ainb-web/src/daemon.rs:1-19` [fact] |
| Daemon client | its **own** stateless dial, fresh connection per call, 5 s timeout. Depends only on `ainb-hangar-proto` + `ainb-hangar-core` | `ainb-web/src/daemon.rs:20-40`, `ainb-web/Cargo.toml` deps [fact] |
| Live update | background poller recomputes a content fingerprint; SSE fires only on change | `ainb-web/src/lib.rs:26-32`, `data.rs:57-80` [fact] |
| **WS PTY bridge** | `PtyBridge::attach(tmux_name)` runs a **real `tmux attach-session -t <name>`** inside a `portable_pty` PTY. Raw bytes → binary WS frames straight into xterm.js (no server-side VT parse). Reader / writer / reaper threads; bounded 256-chunk channel for back-pressure | `ainb-web/src/terminal.rs:36-40,286-355` [fact] |
| WS wire | S→C: text status/error frames + **binary** pane bytes. C→S: `{type:input|resize|ping}`. Resize clamps to `MIN_COLS=10`, `MIN_ROWS=3`, calls `master.resize()` which SIGWINCHes the attach client | `terminal.rs:26-34,56-58,398-407` [fact] |
| Session id → pane | resolved from the **cached poller snapshot** (`session_id` or `tmux_session_name`), never a fresh `ainb list` subprocess | `terminal.rs:81-101` [fact] |
| Auth | bearer token on all `/api/*`; `?token=` query fallback scoped to **only** SSE and WS (a browser cannot set a header on an upgrade) | `ainb-web/src/lib.rs:18-22`, `terminal.rs:11-17` [fact] |
| Read-only mode | `read_only_gate` middleware refuses the WS upgrade and `POST /api/answer` with `403 READ_ONLY` **before** the upgrade extractor — "no connect-then-reject half-state" | `terminal.rs:103-127` [fact] |
| Same screens as the TUI? | **No.** Entirely its own vanilla-JS renderer with 5 API shapes. The TUI's Hangar screens are 25 Rust/ratatui screens in `ainb-plugin-hangar/src/screen/` | `ls ainb-plugin-hangar/src/screen` vs `ainb-web/frontend/app.js` [fact] |
| Launched by | `ainb web` CLI verb → `ainb_web::serve(config, AinbCliSource)` | `ainb-core/src/cli/registry.rs:112,1760-1783` [fact] |
| Tests | Playwright e2e under `ainb-web/e2e/` (`ask-answer.spec.ts`) | [fact] |

**Architectural note [Cunningham].** Original intent, stated in the header: the dashboard "never re-implements data access". It holds that for sessions and cost (CLI proxy) and for needs/answer (daemon RPC). The drift is the **terminal**: it reaches past both and drives tmux itself, in-process, in the web binary [fact, `terminal.rs:288-307`]. That is the one place the "web never touches tmux" claim in `daemon.rs:1-9` is not true of the crate as a whole. [inference]

---

## 6. macOS app + mobile + push

### `apps/ainb-fleet-macos` — 17,763 Swift lines (9,650 in `Sources/`, rest tests) [fact]

```
AINBFleet.app (SwiftUI, menu bar + notch nav)
   │
   │  Darwin.socket(AF_UNIX, SOCK_STREAM)
   │  connect(~/.agents-in-a-box/hangar.sock)
   │  ContentLengthCodec  (81 lines, hand-written)
   │  FleetWire.swift     (1,572 lines, hand-mirrored proto types)
   ▼
hangar.sock ──▶ ainb-hangar-daemon    (SAME BOX, MANDATORY)
   │
   └── one connection carries: fleet/event, fleet/resync_required,
       fleet/message_event, fleet/confirm_event, fleet/activity_event,
       fleet/transcript_event  ("a client needs one connection, not four")
```

| Aspect | Detail | Citation |
|---|---|---|
| Transport | raw BSD `AF_UNIX` socket + own Content-Length codec. Reads the token from `{home}/hangar/daemon.token` | `FleetConnection.swift:575-616`, `HangarLocation.swift:17-34` [fact] |
| Home resolution | `$AINB_HANGAR_HOME` else `~/.agents-in-a-box` — mirrors the Rust resolver exactly | `HangarLocation.swift:10-15` [fact] |
| **Locality** | **Hard-wired same-box.** A unix socket path cannot cross a machine. No ssh, no TCP, no file-sync fallback in the app | `HangarLocation.swift:18` [fact] |
| Negotiation | full client-side `fleet/negotiate` with typed refusals: `protocolReadIncompatible`, `protocolWriteIncompatible`, `missingNegotiatedCapability(String)` | `FleetConnection.swift:4-18` [fact] |
| Forward-compat discipline | unknown notification methods and undecodable chat frames become `.unknownNotification(String)` — "named and dropped, never fatal". A confirm card is carried **raw** so a row this build cannot read still shows as unanswerable | `FleetConnection.swift:38-53` [fact] |
| Screens | Sessions, Needs you, Chat, Usage, Settings; menu-bar view, roster, quick switcher, session detail, interview deck, chat pane | `FleetDesktopController.swift:25-38`, `ls Sources/` [fact] |
| Attach to a pane | shells out: `Process` → `ainb fleet open-terminal <tmuxTarget>` | `FleetDesktopController.swift:808-817`; `ainb-core/src/cli/fleet/open_terminal.rs:1` (macOS-only, bails elsewhere) [fact] |
| Notifications | **`UNUserNotificationCenter` LOCAL only**. Quiet-hours policy, sound toggle, `threadIdentifier` grouping, `userInfo` deep link | `FleetNotificationCenter.swift:1-52` [fact] |
| **APNs / remote push** | **absent.** grep for `APNs|apns|remoteNotification` across `Sources/` → only `UNUserNotificationCenter` hits | [fact] |
| Updater | `FleetUpdater.swift` (Sparkle-style appcast, per recent commit `fix/fleet-adhoc-appcast`) | [fact] |
| Tests | 12 unit suites (`Tests/FleetRPCTests/`: codec, wire forward-compat, contract, reducer, presentations) + 4 UITest journeys driving a `FleetFixtureDaemon` | `ls Tests/ UITests/` [fact] |
| Validation rule | `ainb-tui/CLAUDE.md` mandates end-to-end GUI validation: a pass is the answer landing in the target session's JSONL `tool_result`, or the app showing the daemon's refusal `detail`. "Never record an unfinished GUI run as a success." | [fact] |

### Mobile: **does not exist** [fact]
- `SDKROOT = macosx`, `SUPPORTED_PLATFORMS = macosx`, `MACOSX_DEPLOYMENT_TARGET = 14.0` in every build config [fact, `AINBFleet.xcodeproj/project.pbxproj:496-549`].
- No iOS target, no UIKit, no `iphoneos` anywhere under `apps/` [fact].

### Push: exists, but only as browser web-push
| Aspect | Detail | Citation |
|---|---|---|
| Standard | W3C Push API, VAPID (P-256) | `ainb-web/src/push.rs:1-24` [fact] |
| Keypair | per-install, generated once, persisted `0600` at `$AINB_HOME/.agents-in-a-box/web/vapid.json`; private key never leaves the server | `push.rs:5-9,21-23` [fact] |
| Subscriptions | browser `PushSubscription`s persisted atomically beside the keypair. `MAX_SUBSCRIPTIONS = 64`, FIFO eviction. 16 KiB body limit | `push.rs:46-56` [fact] |
| Presence | per-endpoint focus tracking — "so we don't buzz a phone for a tab you're already looking at" | `push.rs:10-13` [fact] |
| Trigger | background task re-reads the cached snapshot every **3 s**; fires on transition *into* an attention state (ASK/ERR/WAIT, the same `AlertKind` classification notifyd uses). Dead endpoints (404/410) pruned | `push.rs:14-18,41-44` [fact] |
| Init failure | non-fatal — push disabled with a warn, read surface and terminal still work | `ainb-web/src/lib.rs:82-94` [fact] |
| Transport | `web-push` crate 0.11 with `hyper-client` (avoids libcurl/isahc) | `ainb-web/Cargo.toml` [fact] |

So: a phone can receive an ASK notification **today**, as a PWA push from `ainb-web`, provided the box's web surface is reachable from the phone. [fact, composition of the above] There is no native mobile client and no APNs/FCM path. [fact]

---

## 7. Agent status detection

Three tiers feed one `attention` table. The `source` field on `NeedsRow` names which tier produced a card: `Some("hook")`, `Some("tmux")`, or `None` (legacy live classify) [fact, `fleet-core/src/fleet/read/needs.rs:80-87`].

```
TIER 1  AUTHORITATIVE — lifecycle hooks
 agent CLI ──hook──▶ notify.sh ──┬──▶ notify.sock ──▶ notifyd (own rusqlite, OS notifs)
                                 └──▶ (fallback) notify.fallback.jsonl, replayed at startup
 ainb fleet atc hook ──────────────▶ ~/.agents-in-a-box/events.jsonl  (append-only)
                                              │
                       ┌──────────────────────┴──────────────────────┐
                       ▼                                             ▼
              notifyd ingest (rusqlite)              daemon attention_ingest (sqlx)
              → OS notifications                     → attention table + AttentionRaised
                                                      byte-offset cursor, own & independent

TIER 2  INFERRED — tmux pane scan
 tmux capture-pane -e -J ──▶ classify()  ASK > ERR > IDLE > WAIT

TIER 3  INFERRED — transcript tail
 ~/.claude/**/*.jsonl ──▶ jsonl_tail (1,314 LOC): last AskUserQuestion,
                          last api_error, stop_reason, last assistant msg
```

### Tier 1: hooks
| Aspect | Detail | Citation |
|---|---|---|
| Install mechanism | idempotent read-preserve-modify-write **merge** into `~/.claude/settings.json`. Every injected entry tagged `"_ainb_atc_managed": true` so re-install replaces exactly ours and uninstall strips exactly ours. Other tools' hooks on the same event are preserved verbatim — "we append to the event's array, we never replace the array" | `ainb-core/src/fleet/plumbing/hooks.rs:1-28` [fact] |
| Claude events hooked | **30**: SessionStart, Setup, InstructionsLoaded, UserPromptSubmit, UserPromptExpansion, MessageDisplay, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, PostToolBatch, PermissionDenied, Notification, SubagentStart, SubagentStop, TaskCreated, TaskCompleted, Stop, StopFailure, TeammateIdle, ConfigChange, CwdChanged, FileChanged, WorktreeCreate, WorktreeRemove, PreCompact, PostCompact, SessionEnd, Elicitation, ElicitationResult | `hooks.rs:36-69` [fact] |
| Other providers | `plugins/ainb-hooks/{codex,copilot,antigravity}/hooks.json`, merged by `ainb-notifyd install --codex` etc. Codex `SessionEnd` capped at 3 s because "Codex hard-clamps that hook" | `plugins/ainb-hooks/codex/hooks.json:1-2` [fact] |
| Hook script | one bash script, `plugins/ainb-hooks/hooks/notify.sh`. Handles Claude/Copilot (JSON on **stdin**) and Codex (JSON in **argv[1]**). Normalizes to one envelope, delivers over `notify.sock`, falls back to a JSONL file on any failure, **always exits 0** so delivery failure never blocks the agent | `notify.sh:1-40` [fact] |
| Events that can raise a card | `Notification`, `Stop`, `SubagentStop` | `daemon/src/attention_ingest.rs:15-18` [fact] |
| Idempotency | attention id = `att:<session>:<event_id>`; legacy lines fall back to byte offset. The cursor is "a pure efficiency optimisation, not a correctness dependency" — a full re-read from 0 raises each request exactly once | `attention_ingest.rs:28-44` [fact] |
| The ASK re-fire problem, solved | Claude re-fires `Notification` while a session stays blocked; "one live question was measured raising three cards". ASK rows therefore also carry `request_key` (migration 0081) so every firing observing the same still-open question collapses onto the first row; once closed the key frees again | `attention_ingest.rs:46-54` [fact] |
| The withheld-tool_use problem, solved | Claude withholds the `AskUserQuestion` `tool_use` row from the transcript until the tool **resolves**, so a transcript read while the question is open finds nothing. ASK context is therefore read from the **hook payload** instead | `attention_ingest.rs:20-25` [fact] |
| Convergence sweep | `sweep_once` closes open rows no live Fleet session claims. Measured drift without it: **732 open rows against 7 waiting sessions** | `attention_ingest.rs:56-62` [fact] |
| Ingest bound | 4 MiB per pass, remainder next tick | `attention_ingest.rs:87-90` [fact] |
| Two independent consumers of one file | notifyd (rusqlite, OS notifications) and the daemon (sqlx, attention inbox). Deliberate: "the daemon is sqlx-only; it never touches another crate's DB" | `attention_ingest.rs:5-12` [fact] |

### Tier 2: pane classifier (`ainb-fleet-core`)
| Aspect | Detail | Citation |
|---|---|---|
| Priority | **ASK > ERR > IDLE > WAIT**, first match wins, one card per session | `fleet-core/src/fleet/read/needs.rs:1-5` [fact] |
| Capture | `tmux capture-pane -e` (ANSI attributes **kept**) | `fleet-core/src/fleet/read/tmux_pane.rs:1-58` [fact] |
| WAIT markers | exactly two: `"WAITING:"` and `"needs input:"` | `needs.rs:22` [fact] |
| ERR fallback | if the pane finds no error, scan the newest **40** transcript rows | `needs.rs:24-26` [fact] |
| IDLE threshold | 5 min default, `AINB_FLEET_IDLE_MIN` / `--idle-min`; non-numeric and non-positive ignored "so a typo cannot make every session idle" | `needs.rs:28-29,116-130` [fact] |
| tmux-discovery idle | separate 120 s threshold for "between turns vs working", via `AINB_FLEET_TMUX_IDLE_AFTER_SECS` (config bridged through env because fleet-core cannot read `config.toml`) | `fleet-core/src/fleet/discover/tmux.rs:20-42` [fact] |
| Classifier purity | `ClassifyInput` reads everything up front so `classify()` is a **pure function, no IO** | `needs.rs:107-114` [fact] — a real seam [Feathers] |
| Discovery format | `tmux list-panes` with `#{session_name} #{window_index} #{pane_index} #{pane_id} #{pane_pid} #{pane_current_path} #{pane_current_command} #{pane_start_command} #{session_created} #{pane_dead} #{window_activity}` | `discover/tmux.rs:14-18` [fact] |
| Enrichment | `enrich_key` = blake3 of the serialized context, so a cached LLM-drafted suggestion self-invalidates when the session advances | `needs.rs:66-79` [fact] |

**OSC escape sequences: not used for status.** grep for `OSC` / `\x1b]` in `ainb-fleet-core` and `ainb-plugin-notifyd` returns nothing [fact]. ANSI handling in `fleet-core/src/fleet/send/tmux.rs:891,955,1034` is CSI attribute parsing of `capture-pane -e` output (DIM tracking per row, un-wrapping tmux's baked-in row wrapping) — reading the screen, not a terminal-app status protocol [fact].

### Recognised agent CLIs — two registries
| Registry | Values | Citation |
|---|---|---|
| `SessionAgentRegistry` (TUI new-session picker, 8 built-ins) | `claude`, `shell`, `ssh`, `codex`, `gemini`, `copilot`, `antigravity`, `kiro` (kiro `is_available() == false`) | `ainb-core/src/agents/mod.rs:255-320` [fact] |
| `FleetProvider` (wire) | `Claude`, `Codex`, `Antigravity`, `Copilot`, `Acp`, `Unknown` (default) | `proto/fleet.rs:196-211` [fact] |
| Provider adapters in the daemon | `claude.rs`, `codex.rs`, `codex_manager.rs` — **two** providers have authoritative adapters | `ls daemon/src/fleet_provider/` [fact] |
| Tool-install adapters (separate concern) | amazonq, antigravity, claude, claude_desktop, cline, codex, copilot, cursor, gemini, roo | `ls ainb-adapters-tool/src/` [fact] |

The two registries overlap but are not the same set, and they are deliberately separate: `SessionAgent` covers non-AI session shapes (`shell`, `ssh`) [fact, `agents/mod.rs:10-21`]. For a redesign this is **two lists to keep in sync**, and `FleetProvider` is the one on the wire. [inference]

### The attention / ASK answer pipeline
This is the most behaviourally intricate code in the tree. [inference]

```
attention/answer {attention_id, answer}
        │
        ▼
 AttentionRepo::get ──▶ state != open?  ──▶ AlreadyAnswered{by}
        │
        ▼  ACP permission row?  ──▶ AcpPool::answer_permission   ← NO TMUX AT ALL
        │
        ▼  resolve target BEFORE the flip
 exact session id → else cwd correlation
   │  >1 session in cwd, or merged session with 2+ sources ──▶ REFUSE (row stays open)
   │  original session exited AND a different agent now owns the cwd
   │  (its transcript is newer)                            ──▶ REFUSE
   ▼
 mark_answered_if_open   ← conditional UPDATE, first-answer-wins at the DB
        │
        ▼
 deliver(): capture-pane FIRST (always, picker or not)
        │
        ├─ pane unreadable AND row says a picker is up ──▶ Failed, row reopens
        ├─ Route::Text    ──▶ send()  (tmux send-keys, submit-verified)
        ├─ Route::Refuse  ──▶ Failed  ("free text is not typed into a picker")
        └─ Route::Picker(position)
                 │
                 ▼  deliver_picker: press the DIGIT, then WATCH THE SCREEN
                    up to 60 × 100 ms capture-pane:
                      Confirmed          ──▶ Delivered
                      Recorded(other)    ──▶ Failed "agent recorded option N instead"
                      Commit             ──▶ send Enter ONCE (older builds)
                      Gone ≥ PICKER_SETTLE ──▶ Delivered
```

| Guarantee | Detail | Citation |
|---|---|---|
| Exactly-once | conditional UPDATE `mark_answered_if_open`; two surfaces serialise at SQLite, one delivers, the other is told "already answered by X" | `daemon/src/answer.rs:6-13` [fact] |
| C1 misroute refusal | refuses on cwd ambiguity rather than guessing; additionally binds the cwd fallback to the transcript captured at raise time so a *later* agent in the same cwd is never answered | `answer.rs:14-25` [fact] |
| Ordering | target resolution runs **before** the flip, so an ambiguous or dead target leaves the row open and answerable later | `answer.rs:27-31` [fact] |
| Picker routing | digit is the commit key on Claude Code 2.1.258 (probed live 2026-09-02: `2` alone closed a three-option picker and echoed `→ Green` within 1.5 s, before any Enter); older builds only moved the highlight and needed Enter. `picker_step` tells them apart from the capture | `answer.rs:549-571` [fact] |
| Picker position matching | label verbatim → label case-insensitive → 1-based digit. **Prefixes deliberately excluded**: "coercing a prefix into a pick turned a deliberate free-text answer into a selection" | `answer.rs:519-535` [fact] |
| Multi-select | no routed path at all. With its picker on screen the free-text guard refuses, so the operator must answer at the pane — because typing into it "used to report Delivered while the picker kept whatever was highlighted" | `answer.rs:493-499` [fact] |
| Mirrored interviews | when the interview is showing in Claude's own picker, remote answering is refused with a message naming the fix (`ainb fleet interview surface fleet`) | `rpc/mod.rs:104-109` [fact] |
| `answer.rs` size | 1,940 lines for this one router | [fact] |

**This is the deepest tmux entanglement in the system, and it is not incidental.** The correctness of a remote answer currently depends on reading the target's *screen* and watching it settle. [inference]

---

## 8. tmux coupling depth

| Metric | Value | Citation |
|---|---|---|
| `Command::new("tmux")` sites, all | **678** | grep [fact] |
| `Command::new("tmux")` sites, non-test | **126** | grep excluding `/tests/` and `*_tests.rs` [fact] |
| By crate (all sites) | `ainb-core` 642, `ainb-hangar-daemon` 25, `ainb-fleet-core` 10, `ainb-plugin-hangar` 1 | grep [fact] |
| `capture-pane` string occurrences | 654 | grep [fact] |

### Distinct tmux subcommands used (occurrence counts, all files)
| Subcommand | Count | What depends on it |
|---|---:|---|
| `send-keys` | 194 | **answer delivery**, prompt send, picker keys, broker fallback |
| `new-session` | 134 | session creation (TUI `ainb run`, daemon interactive mode) |
| `kill-session` | 120 | session teardown, soft-stop, timeout kill |
| `capture-pane` | 90 | **status classification**, picker settle-watch, previews, snapshots |
| `has-session` | 36 | liveness before every send; interactive-run completion polling |
| `set-option` | 11 | session config at create |
| `list-sessions` | 7 | discovery |
| `list-panes` | 7 | **fleet discovery** (the 11-field format string) |
| `display-message` | 7 | target/identity probes |
| `respawn-pane` | 6 | session recovery |
| `attach-session` | 6 | TUI embedded client, `ainb-web` PTY bridge, `ainb fleet open-terminal`, `ainb run` |
| `set-window-option` | 4 | pane config |
| `source-file` | 2 | tmux.conf load |
| `select-window`, `select-pane` | 2, 2 | navigation |
| `show-options`, `run-shell`, `rename-session`, `list-clients`, `display-popup`, `copy-mode`, `bind-key` | 1 each | misc; `list-clients -F "#{client_readonly}"` at `ainb-core/src/tmux/embed_client.rs:551` |
[fact, grep over all `.rs`]

### The tmux modules
`ainb-core/src/tmux/` — 2,882 LOC across `capture.rs`, `embed_client.rs`, `embed_input.rs`, `mod.rs`, `process_detection.rs`, `pty_wrapper.rs`, `session.rs` [fact].
`ainb-core/src/tmux/embed_client.rs` drives a live `tmux attach-session` in a PTY for the TUI preview pane [fact, `embed_client.rs:1,156`].

### What breaks if tmux is replaced by a daemon-owned PTY

| Feature | Depends on | Breaks? | Note |
|---|---|---|---|
| Session persistence across TUI/daemon restart | tmux server outliving both | **YES, hard** | tmux is currently the *only* thing keeping an interactive agent alive when every ainb process exits |
| Attach from anywhere (`tmux attach`, another terminal, iTerm) | tmux multi-client | **YES** | `list-clients`, `attach-session` × 6 sites; daemon PTY would need its own multiplexer |
| `ainb-web` live terminal | `PtyBridge` spawning `tmux attach-session` | **Adapts cleanly** | already a PTY bridge; it would attach to the daemon PTY instead of a tmux client. Smallest change of the set |
| `ainb fleet open-terminal` / macOS "open session" | `tmux attach -t` | **YES** | needs a new attach verb |
| Status classification (tier 2) | `capture-pane -e -J` | **Adapts** | a daemon-owned PTY already has the byte stream; today's capture is a *poll* of someone else's screen. Would become cheaper and more accurate |
| Fleet discovery of *unmanaged* sessions | `list-panes` over the user's own tmux server | **YES, by design** | today ainb sees agents the user started by hand in their own tmux. A daemon-owned PTY only sees what the daemon spawned. **This is a capability loss, not a refactor** |
| Answer delivery (free text) | `send-keys` + submit verification | **Adapts** | write to the PTY master instead |
| Answer delivery (picker) | `send-keys` digit **+ `capture-pane` settle-watch loop** | **Adapts, but the logic must move wholesale** | 1,940 lines of `answer.rs` encode screen-reading semantics against specific Claude Code builds. A PTY gives the same bytes, so the state machine survives; the transport swaps |
| Resize | `master.resize()` → SIGWINCH → tmux re-arbitrates | **Simplifies** | one client, no arbitration |
| Interactive board cards | `tmux new-session -d` + exit-code sidecar file + `has-session` polling | **YES** | `daemon/src/interactive.rs:1-27` exists *specifically* to make a board card "a live, attachable terminal that shows up in `tmux ls` like any other session" |
| Headless runs | nothing | **NO** | `runner.rs` (2,629 LOC) is pipe-and-capture already |
| ACP sessions | nothing | **NO** | `acp_pool.rs` (3,893 LOC) is stdio JSON-RPC, explicitly tmux-free |

**The good news [Feathers].** Two tmux-free execution paths already ship and are substantial: the headless runner and the ACP pool. `ainb-acp` is documented as a pure library that "could be promoted to a standalone process without a redesign" [fact, `ainb-acp/src/lib.rs:17-24`]. A daemon-owned-PTY mode is a **third sibling** of an existing pattern, not a new architecture. [inference]

**The pinch point [Feathers].** `ainb-fleet-core::send::route::send` is the one funnel for all outbound text, and `tmux_delivery_preferred()` is already an env-selected switch (`AINB_FLEET_TRANSPORT` ∈ unset|`tmux`|`tmux-first`|`tmux-only`|`peers`|`broker`|`peers-first`) [fact, `fleet-core/src/fleet/send/route.rs:9-56`]. A `Transport::DaemonPty` variant is the designed extension point, and the comment says so: "a transport added later is NOT assumed to drive keys into a pane until it says so here" [fact, `route.rs:50-51`]. Adding a transport is a small diff at a real seam. [inference]

**The blast radius, honestly.** 126 non-test tmux call sites, 642 of the 678 total in `ainb-core` alone. `ainb-core` is 252k LOC with no characterization harness over its tmux layer beyond the tripwire tests. [fact] Ripping tmux out is not a refactor; adding a parallel PTY transport behind the existing seam is. [inference]

---

## 9. Session durability today

Three independent records of "what sessions exist", each with a different lifetime.

```
┌──────────────────────────────────────────────────────────────────────┐
│ 1. tmux server           ── the actual processes                     │
│    survives: TUI restart, daemon restart, ainb upgrade               │
│    dies on:  machine reboot, tmux kill-server, OOM                   │
├──────────────────────────────────────────────────────────────────────┤
│ 2. ~/.agents-in-a-box/sessions.json   ── TUI metadata (SessionStore) │
│    cross-process flock guard; survives everything except disk loss   │
│    can go STALE: entry present, tmux dead → "orphan"                 │
├──────────────────────────────────────────────────────────────────────┤
│ 3. ~/.agents-in-a-box/hangar.db       ── daemon truth (96 migrations)│
│    fleet_session, attention, tasks, boards, receipts, event_outbox   │
│    survives everything; reconciles to reality at boot                │
└──────────────────────────────────────────────────────────────────────┘
        +  periodic snapshots (30 min) for disaster recovery
        +  events.jsonl / notify.fallback.jsonl  (append-only, replayed)
```

| Artefact | Path | Contents | Citation |
|---|---|---|---|
| TUI session store | `~/.agents-in-a-box/sessions.json` | tmux name → `SessionMetadata` (session_id, worktree_path, workspace_name, agent_type). Cross-process **advisory flock**, RAII `SessionStoreGuard`; `mutate()` is the preferred upsert/remove seam. Never holds the lock across async IO | `ainb-core/src/interactive/session_manager.rs:1141-1271`, `state.rs:13339` [fact] |
| Session labels | `~/.agents-in-a-box/session-labels.json` (legacy `ssh_display_names.json` still read) | keyed on tmux session name; labels normalized (no control chars, ≤64 chars) | `config/ssh_display_names.rs:9-56` [fact] |
| Snapshots | every **30 min** | per session: tmux name, session_id, worktree_path, workspace_name, agent_type, git_branch, git_dirty_files, **pane_content**, `tmux_alive` | `ainb-core/src/app/snapshot.rs:1-60` [fact] |
| Hangar DB | `~/.agents-in-a-box/hangar.db` | 96 migrations, 46 repos | `ls store/migrations` [fact] |
| Durable event outbox | `event_outbox` table | gapless log for `seq`-cursor replay; fed by an **unbounded** mpsc so a burst never back-pressures a mutation path and never drops | `daemon/src/events.rs:64-76` [fact] |
| Hook event log | `~/.agents-in-a-box/events.jsonl` | append-only, PIPE_BUF-atomic line cap so concurrent hook appends don't interleave | `ainb-core/src/cli/fleet/atc.rs:2466-2471,2671-2677` [fact] |
| Hook fallback | `~/.agents-in-a-box/notify.fallback.jsonl` | written when `notify.sock` is down; **replayed and cleared** at notifyd startup | `notifyd/src/lib.rs:12-15` [fact] |
| Parent inboxes | `~/.agents-in-a-box/inbox/<parent_id>.{jsonl,consumed,budget}` + `dead-letter.jsonl` | exactly-once child-completion drain with a consumed fingerprint and a Stop-drain block budget | `ainb-core/src/fleet/plumbing/paths.rs:1-11` [fact] |
| Daemon single-instance | `{home}/hangar/daemon.lock` | held for the whole of `boot`; the kernel releases on crash. **The only thing making the stale-socket unlink in `rpc::bind` safe** | `rpc/mod.rs:243-255`, `daemon/src/single_instance.rs` [fact] |
| Daemon token | `{home}/hangar/daemon.token` | `0600`, plaintext written once; reused when the stored digest and the on-disk plaintext agree, else re-minted | `rpc/auth.rs:20-27` [fact] |

### What survives what

| Event | tmux panes | sessions.json | hangar.db | in-flight task |
|---|---|---|---|---|
| TUI restart | survive | survives | survives | survives |
| Daemon restart | survive | survives | survives | **reclaimed, not resumed** — see below |
| Daemon crash (SIGKILL/OOM) | survive | survives | survives | reclaimed at next boot; orphaned Codex app-servers reaped |
| Machine reboot | **LOST** | survives (stale) | survives | lost; rows reconciled |
| `tmux kill-server` | **LOST** | survives (stale) | survives | lost |

**Daemon restart reclaim [fact].** Migration 0092 added a runtime *instance id*. At boot `resolve_runtime_boot` stamps this process's instance id onto the runtime row and reports whether it displaced a different one — "which is what tells the run loop that the rows still `dispatched`/`running` for this runtime are a dead process's orphans rather than our own live work" [`daemon/src/lib.rs:1294-1301`]. Separately, `reap_orphaned_codex_servers` kills app-server processes orphaned by a SIGKILLed prior daemon [`lib.rs:1076-1083`]. A Fleet twin goes `EXITED` under the stale reaper [`lib.rs:929`].

**Recovery UX [fact].** `ainb recover {list,resume,cleanup}` (897 LOC) detects three orphan classes: `StaleMetadata` (in store, tmux dead), `AgentFile` (JSON in `~/.claude/agents/` untracked), `BrokenSymlink` (`~/.agents-in-a-box/worktrees/by-session/`). `resume` re-registers an orphan **if tmux is alive** [`ainb-core/src/cli/recover.rs:1-50`]. There is also a `session_recovery.rs` TUI component.

**The gap.** Nothing resumes an interactive agent whose tmux pane is gone. The snapshot captures `pane_content` for forensics, not for restart [fact, `snapshot.rs:26-28`]. A reboot loses every interactive session's process; only the *record* survives. [inference]

---

## 10. Seams and change points [Feathers]

| Change point | Seam? | Blast radius | Safe to touch? |
|---|---|---|---|
| `ainb-hangar-proto` (pure data, no host deps) | **Yes, excellent** | 3 hand-written clients (Rust ×2, Swift ×1) + the plugin | Safe for **additive** change. A shape change is a three-way edit incl. 1,572 lines of Swift |
| `Transport` enum in `fleet-core/send/route.rs:25-56` | **Yes, designed for this** | one `match`, one env var | **Safe.** Explicitly documented as the place a new transport declares itself |
| `classify()` in `read/needs.rs:107-114` | **Yes, pure function** | 3 producers (hook ingest, tmux fold, legacy live) | **Safe.** No IO, table-testable |
| `EventSink` / `EventBroker` | **Yes** | every mutation path holds a cloned sink | Safe to add a channel; changing delivery semantics is not |
| `DataSource` trait in `ainb-web/src/data.rs:15-18` | **Yes**, boxed-future for object safety | web routes only | **Safe.** Built for test injection |
| `settle()` in `answer.rs` | **Yes, pure** | the picker loop | **Safe.** "pure so it can be table-tested" |
| RPC transport (`UnixListener` in `rpc/mod.rs:240-258`) | **No seam** | `bind()` returns a concrete `UnixListener`; `serve()` takes one; `handle_conn` takes `UnixStream` and calls `peer_cred()` on it | **Needs a harness first.** Adding TCP/TLS means abstracting the stream *and* re-answering what replaces `SO_PEERCRED` |
| `rpc/mod.rs` dispatcher | **No** | 17,298 lines, 156 arms in one function tree | **Do not restructure without characterization tests.** Add arms; don't reshape |
| `ainb-core` tmux layer | **No seam** | 642 call sites, `Command::new` inline throughout | **Do not touch without a harness.** No trait, no injection point at the tmux boundary |
| `ainb-core/src/app/state.rs` | **No** | single file, 13,000+ lines, holds the TUI state machine | Highest-risk file in the tree |

### Pinch points worth one characterization test each
1. `fleet-core::send::route::send` — every outbound byte to an agent. [inference]
2. `fleet-core::read::needs::classify` — every status card. Already pure. [fact]
3. `daemon::answer::answer` — every remote answer, with the exactly-once and misroute guarantees. [fact]
4. `daemon::rpc::dispatch` — the whole client-visible surface. [fact]
5. `ainb-hangar-proto` shapes — the three-client contract. Already partly held by `wire_surface_gate` and the Swift `CanonicalFixtureTests`. [fact]

### Existing behavioural harnesses (what is already locked down)
| Harness | Scope | Citation |
|---|---|---|
| `argv_golden_matrix` | frozen per-provider CLI argv | `ainb-tui/CLAUDE.md`, `daemon/tests/` [fact] |
| `migration_upgrade_full_chain` | migration replay over an adversarially-seeded db | `store/tests/` [fact] |
| `axes`, `real_plugin_axes`, `wire_surface_gate` | 14-axis plugin conformance + wire-surface semver gate | `ainb-plugin-cts-v2/tests/` [fact] |
| `tripwire_*` | ~20+ TUI screen tripwires | `ls ainb-core/tests/` [fact] |
| Swift `FleetRPCTests` | codec, wire forward-compat, daemon contract, reducer, presentations | `ls apps/.../Tests/` [fact] |
| Swift `UITests/ShellJourneys` | offline/reconnect, protocol compat, menu-bar roster, chat pane — against a `FleetFixtureDaemon` | [fact] |
| `ainb-web/e2e` | Playwright `ask-answer` | [fact] |
| `live_probe_tests.rs` (1,146 lines) | live tmux send probes | `fleet-core/src/fleet/send/` [fact] |
| CI trap | these are **contract** tests: "if one goes red, never rerun the job to green it" | `ainb-tui/CLAUDE.md` [fact] |

**Caveat on coverage.** `default-members` is only `ainb-core` + `ainb-hangar-daemon`, so a plain `cargo nextest run` silently skips every other crate's tests **and still reports a clean summary** [fact, `ainb-tui/CLAUDE.md`]. Whether the gates above actually run is a per-job question. I did not verify the CI workflow. [unknown]

---

## 11. Technical debt, intent → drift [Cunningham]

**1. Three daemon clients.** *Intent:* one canonical client (`ainb-hangar-client`), split into its own crate for a real dependency reason (`ainb-fleet-tools` must dial the socket; `ainb-core` depends on the daemon, so nothing below can depend back) [fact, `hangar-client/src/lib.rs:17-21`]. *Drift:* `ainb-web` re-implemented dial+framing to avoid host deps [fact, `web/src/daemon.rs:12-19`], and Swift re-implemented it again out of necessity [fact, `ContentLengthCodec.swift`]. *Impact:* the wire has three implementations and 1,572 lines of hand-mirrored Swift types. Every protocol change is a three-way edit. A **generated** Swift client, or a thin C-ABI shim over `ainb-hangar-client`, removes one of the three. [inference]

**2. Capabilities read like authorization and are not.** *Intent:* `fleet/negotiate` advertises what the build serves. *Drift:* the names (`fleet.transcript.prune`, `fleet.confirm.answer`) read like permissions, and the code comment has to say out loud that they are not, with a dated review note [fact, `proto/fleet.rs:50-61`, review 2026-08-07]. Real enforcement needs per-connection granted capabilities, which the comment calls "a v3 surface change". *Impact:* the `Caller::Pal` allowlist exists precisely because capabilities could not carry that weight [fact, `rpc/auth.rs:39-85`]. A redesign with more than one remote client **needs** the v3 per-connection model. [inference]

**3. `capture-pane` as the status oracle.** *Intent:* see agents the operator started by hand, in their own tmux, with no cooperation required. Genuinely valuable. *Drift:* the *answer* path now also depends on screen-reading — 60 × 100 ms captures watching a picker settle, with build-specific behaviour probed live on a dated Claude Code version [fact, `answer.rs:549-571`, probed 2026-09-02]. *Impact:* remote answer correctness is coupled to a specific agent CLI's *rendering*. This is the single largest obstacle to a clean remote/multi-host answer path. [inference]

**4. `ainb-core` is a monolith with the TUI, the CLI, tmux, docker, git and the session store in it.** 252k LOC / 457 files; `app/state.rs` alone exceeds 13,000 lines [fact]. *Impact:* anything that needs session management links the TUI. `ainb-web` avoided this by shelling out to the binary — a subprocess boundary standing in for a missing library boundary [fact, `web/src/data.rs:200-213`]. [inference]

**5. Two agent registries.** `SessionAgentRegistry` (8 ids, includes non-AI `shell`/`ssh`) and `FleetProvider` (6 variants, on the wire) [fact]. The split is justified in the header [fact, `agents/mod.rs:10-21`], but adding a provider means touching both, and only one is wire-visible. [inference]

**6. Three session-existence records.** tmux, `sessions.json`, `fleet_session`. Reconciliation is real and works (the 732-vs-7 drift was found and fixed with a sweep [fact, `attention_ingest.rs:56-62`]), but it is reconciliation between three writers, not one source of truth. [inference]

**7. The idle threshold appears twice.** 5 min in `needs.rs` (`AINB_FLEET_IDLE_MIN`) and 120 s in `discover/tmux.rs` (`AINB_FLEET_TMUX_IDLE_AFTER_SECS`), for genuinely different questions [fact]. `needs.rs:118-122` says every tier must age idle by the same rule or "a session flickers in and out of `needs`" — that warning is about the tiers within `needs`, and the two knobs are separate by design. Worth confirming with a maintainer that this is intended and not a latent flicker source. [inference]

---

## 12. Security leads (for security-agent, not an audit)

| Lead | Location | Severity | Note |
|---|---|---|---|
| Daemon token is a **plaintext file on disk**, `0600`, same-uid | `rpc/auth.rs:20-27` | Medium | Adequate for single-user local. For any remote client it is not a credential model: no expiry, no rotation, no per-client identity, no revocation |
| `SO_PEERCRED` same-uid is the real boundary and it **does not exist off-box** | `rpc/auth.rs:5-9` | High **for a redesign** | A TCP/TLS transport must replace this gate, not merely add to it. This is the load-bearing check |
| Capabilities are advisory, self-documented as non-authz | `proto/fleet.rs:50-61` | Medium | With >1 remote client this becomes exploitable-by-design |
| `ainb-web` WS terminal is **interactive shell on every fleet session** | `web/src/terminal.rs:1-20` | High | Correctly gated twice (bearer + read-only), and `--insecure-bind` + writable is refused outright. The gating is good; the exposure is inherent |
| `?token=` in a URL query | `web/src/lib.rs:18-22`, `terminal.rs:11-17` | Low-Medium | Necessary (browsers cannot set headers on an upgrade), scoped to exactly two routes. Tokens in URLs land in logs and history |
| `answer.rs` types keystrokes into a live agent pane | `answer.rs:558` | Medium | Any surface that can call `attention/answer` can drive keys into a session. The Pal allowlist includes `attention/answer`, with an explicit note that binding it to the gate verdict needs a per-call capability that does not exist yet | 
| VAPID private key on disk | `web/src/push.rs:5-9` | Low | `0600`, never leaves the server, documented |
| Hook script always exits 0 | `plugins/ainb-hooks/hooks/notify.sh:9` | Low | Deliberate (must never block the agent). Means silent delivery failure; the fallback JSONL covers it |
| `env` allowlist applies to ambient vars, keys layered **after** the policy | `daemon/src/dispatch.rs:11-18` | Informational | Deliberate and correct: `LD_PRELOAD` can never reach the child even if allowlisted |
| Webhook ingress is hand-rolled HTTP/1.1 | `daemon/src/webhook_ingress.rs:3` | Low-Medium | Loopback-hardcoded, but a hand-rolled parser on an "untrusted HTTP surface" is worth a fuzz pass |

---

## 13. Confidence notes

**Confirmed** (read the code path or ran the command):
- Crate inventory, LOC, workspace membership, `default-members`.
- Daemon transport, framing, both auth gates, `Caller` split, method/dispatch counts, subscription arms, event-broker semantics, idle bounds, error codes.
- `FLEET_PROTOCOL_VERSION = 2`, negotiate statelessness, capability advisory-ness.
- Absence of any host/machine field in `FleetSession` and `fleet_session`.
- `SshTarget` is a local command builder; `ssh_display_names` is a legacy label file; `discover_from_peers` is same-box PID-checked.
- `ainb-web`: routes, embedded frontend, CLI-proxy data source, own daemon dial, `PtyBridge` running `tmux attach-session`, bind-security policy, web-push design.
- macOS app: raw `AF_UNIX` dial, same home resolution, local-only `UNUserNotificationCenter`, `macosx`-only build config, `ainb fleet open-terminal` shell-out.
- Hook event lists (30 Claude, per-provider JSON), `notify.sh` dual-input handling, `events.jsonl` dual-consumer design, attention idempotency and `request_key`.
- Pane classifier priority, markers, thresholds, purity; `capture-pane -e`; the 11-field `list-panes` format.
- `answer.rs` guarantees and the picker settle machine.
- tmux subcommand inventory and call-site counts.
- Durability artefacts, paths, locking, and the runtime-instance-id reclaim.

**Inferred** (reasoned from confirmed evidence, flagged inline):
- That a daemon-owned PTY mode is a sibling of the existing headless/ACP paths rather than a new architecture.
- That losing `list-panes` discovery is a capability loss and not just a refactor.
- Which features "adapt cleanly" vs "break hard" in the tmux-replacement table.
- Every debt item's impact statement.
- That three wire implementations make protocol change a three-way edit.

**Not verified / open questions:**
1. **Does CI actually run the gates?** `default-members` is narrow and a plain `cargo nextest run` skips the rest while reporting clean. I did not read `.github/workflows/ci.yml`. Which of the 5 contract tests and 20+ tripwires actually execute?
2. **Is `ainb-web` shipped or a prototype?** It is a workspace member wired to `ainb web`, but I found no packaging, service file, or docs telling a user to expose it. If it is real, it is already the remote surface and the redesign should build on it rather than beside it.
3. **What is `~/.claude-peers.db` and is the broker still alive?** `route.rs:14-16` calls it "proven flaky" and demotes it. Is `peers` transport dead code, dormant code, or still in use by someone?
4. **What replaces `SO_PEERCRED` off-box?** This is *the* design question. mTLS, an SSH-tunnelled unix socket, or a bearer token over TLS all imply different threat models and different work.
5. **Is the picker settle-watch acceptable to keep?** It works and it is well-tested, but it pins remote answering to a specific agent CLI's rendering. Is there a provider-native answer path (ACP already has one — `AcpPool::answer_permission`, no tmux at all) that could become the default rather than the exception?
6. **Two idle thresholds** (5 min / 120 s): intended, or latent flicker?
7. **Is `ainb-fleet-tools` (the Pal MCP server) shipped?** It has its own daemon credential and a 7-method allowlist, which is a notable amount of security machinery for something I did not see wired into a user-facing flow.
8. **Kiro** is registered but `is_available() == false`. Dead or dormant?

---

## Redesign inputs

| Thing | Verdict | Why |
|---|---|---|
| `ainb-hangar-proto` (envelopes, 159 methods, fleet/attention/snapshot types) | **Reusable as-is** | Pure data, no host deps, already the contract for 3 clients + a semver gate. Extend additively |
| JSON-RPC 2.0 + Content-Length framing | **Reusable as-is** | Transport-agnostic. Works unchanged over TCP/TLS/WS |
| `ainb-hangar-store` (96 migrations, 46 repos, `event_outbox`, revisions) | **Reusable as-is** | Revision-ordered, durable, replay-capable. The right substrate for multi-client sync |
| `EventBroker` + `event_outbox` + `fleet/subscribe {after_revision}` with `SnapshotReset` | **Reusable as-is** | Cursor-replay sync already solved, incl. the three reset reasons. This is the hardest part of remote state sync and it is done |
| `ainb-hangar-client` (canonical dial + framing + fleet subscribe) | **Reusable with change** | Correct shape; hardcodes `UnixStream`. Needs a transport abstraction |
| `classify()` pane/transcript classifier | **Reusable as-is** | Pure function, 3 producers, table-tested |
| `jsonl_tail` (1,314 LOC transcript reader) | **Reusable as-is** | Pure file reading; works on any box that has the transcript |
| Hook pipeline (30 events, `notify.sh`, `events.jsonl`, byte-cursor ingest, `request_key`) | **Reusable as-is** | Authoritative status tier, provider-agnostic, crash-safe, idempotent. Already the best signal source |
| `ainb-acp` + `acp_pool` (stdio, PTY-free, multiplexed, at-most-once) | **Reusable as-is** | Already tmux-free and remote-friendly. The template for a new execution mode |
| Headless `runner.rs` (pipe-and-capture) | **Reusable as-is** | Already tmux-free |
| `Transport` enum in `send/route.rs` | **Reusable as-is** | The designed extension point. Add a variant |
| `ainb-web` read surface (SSE, fingerprint poller, `DataSource` trait, PWA, VAPID push) | **Reusable with change** | The only existing off-box surface, and the push story. Swap `AinbCliSource` (CLI subprocess) for a direct daemon client |
| `ainb-web` `PtyBridge` (WS ⇄ PTY, xterm.js, back-pressured) | **Reusable with change** | Right shape; point it at a daemon PTY instead of `tmux attach-session` |
| `ainb-web` `check_bind_security` | **Reusable as-is** | Sane exposure policy incl. refusing unauthenticated writable binds |
| Swift `FleetConnection` + `FleetProjectionReducer` + forward-compat discipline | **Reusable with change** | Excellent client patterns (unknown frames named-and-dropped, raw confirm cards). Retarget the transport; ideally generate `FleetWire.swift` instead of hand-mirroring 1,572 lines |
| `answer.rs` exactly-once + C1 misroute guards | **Reusable as-is** | The hard-won correctness. Keep verbatim |
| `answer.rs` picker settle-watch (digit + capture loop) | **Reusable with change** | Logic survives a PTY swap; the `capture-pane` transport does not survive going off-box |
| `AcpPool::answer_permission` (answer with no tmux) | **Reusable as-is** | The model for what every provider's answer path should look like |
| `sessions.json` + `SessionStore` flock | **Reusable with change** | Single-machine file. Needs to be per-host or subsumed by `fleet_session` |
| `SnapshotManager` (30-min snapshots incl. `pane_content`) | **Reusable with change** | Forensic only today. Would need real state to support resume |
| `ainb recover` (3 orphan classes, resume-if-tmux-alive) | **Reusable with change** | Assumes local tmux and local filesystem |
| **Unix socket as the only RPC door** | **BLOCKER** | Cannot cross a machine. Every one of the 3 clients hardcodes the path |
| **`SO_PEERCRED` same-uid as the trust boundary** | **BLOCKER** | Has no off-box equivalent. Must be replaced, not extended. The token alone is not a remote credential model |
| **No host/machine identity in wire types or schema** | **BLOCKER** | `FleetSession` and `fleet_session` have no host column. Multi-host needs a new identity dimension threaded through snapshot, subscribe, action, receipt, attention, and all 3 clients |
| **`session_key` uniqueness is implicitly per-box** | **BLOCKER** | Derived from tmux target / process fingerprint / cwd, all machine-local. Two boxes will collide |
| **Filesystem-rooted contract (`~/.agents-in-a-box` for db, socket, token, events.jsonl, sessions.json, inbox)** | **BLOCKER** | Every component resolves the same local home. A remote client has no access to any of it |
| **`capture-pane` status + `send-keys`/digit answer delivery** | **BLOCKER for remote answering** | Requires local tmux. The ACP path shows the way out, but only 2 of 6 providers have authoritative adapters today |
| **tmux as the session-persistence owner** | **BLOCKER for daemon-owned processes** | tmux is the only thing keeping an interactive agent alive across restarts, and `list-panes` discovery of hand-started agents is a real capability that a daemon-owned PTY loses |
| **`ainb-core` monolith (252k LOC, TUI+CLI+tmux+docker+git+store)** | **BLOCKER for reuse** | No library boundary. `ainb-web` already works around it by spawning the binary |
| **Capabilities are advisory, not per-connection authz** | **BLOCKER for multi-client** | Self-documented as needing a v3 surface change. Fine for one trusted local operator, not for N remote clients |
| **No mobile client; no APNs/FCM** | **BLOCKER for mobile** | macOS-only Xcode target, local notifications only. Web-push PWA is the only phone path today |
| **`ainb-web` terminal reaches past its own abstraction into tmux** | **Reusable with change** | The crate's stated "never touches tmux" rule already has this exception. Make it explicit in the redesign rather than inherited by accident |
