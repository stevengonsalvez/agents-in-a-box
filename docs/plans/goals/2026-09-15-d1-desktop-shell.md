# /goal D1 is on v2: a Tauri v2 desktop shell that embeds the same `ainb-app` state machine the TUI embeds, supervises a bundled hangar daemon sidecar, mirrors the Sessions section as named frames into a SolidJS store, draws the sessions sidebar, a WS terminal tab and the command palette, and proves it with a wdio sessions journey green on ubuntu and macos

─ CONTEXT ─

· Project: agents-in-a-box (ainb) desktop programme, slice 3 node D1.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:132`. Gate to start: P2. Gate to finish: "wdio sessions journey".
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: the D1 row (`:115`), the architecture diagram (`:37-73`), the renderer contract (`:75-101`), the interface and screen inventory (`:153-225`), the behaviour and edge cases (`:227-263`), the hosts and daemon state machine (`:265-285`), errors (`:330-341`), testing (`:343-355`), packaging (`:356-368`).
  - Multi-surface spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md`: D10 to D18 at `:49-61`.
  - Surface-safety plan `docs/plans/2026-09-05-desktop-p0-surface-safety.md`: the chord grammar that reserves `cmd+` for this renderer (`:58`), the draw path sealed on `&AppState` plus a renderer-local `UiState` (`:48`), and its own scope line "no desktop crate yet" (`:38`).
  - Sibling goals whose format and voice this one matches: `docs/plans/goals/2026-09-14-w0-mirror.md` and `docs/plans/goals/2026-09-14-p5-hosts-close-slice-2.md`.

· Stack:
  - Rust workspace under `ainb-tui/`; members are listed in `ainb-tui/Cargo.toml`, and `default-members` is `ainb-core` plus `ainb-hangar-daemon` so a plain build keeps the sidecar fresh rather than stale.
  - Frontend: Tauri v2, SolidJS, xterm.js, vite, Node 22 with a committed `package-lock.json`, the pattern CI already uses at `.github/workflows/ci.yml:289-298`.
  - This node adds one excluded workspace at `ainb-tui/crates/ainb-desktop/` with its own `Cargo.toml` workspace and lockfile, listed in `exclude` and never in `members`, so the 29-crate `--all-features` Test job and its 90G disk assert (`.github/workflows/ci.yml:366`) are untouched and no existing job needs webkit2gtk. Its gates run in one new job per runner that installs the Tauri system packages. Nothing else in the workspace gains a dependency.

· What is on v2 that D1 builds on, seam by seam. Every path below exists on `origin/v2` at `b081c09cf`.
  - `ainb-tui/crates/ainb-app/src/lib.rs:43` re-exports the whole renderer contract: `AppState`, `Btn`, `Chord`, `CommandId`, `Effect`, `Intent`, `Key`, `Keymap`, `Mods`, `Pos`, `SectionId`, `Versioned`, `dispatch`.
  - `ainb-tui/crates/ainb-app/src/app/intent.rs:56`: `dispatch(&mut AppState, &Keymap, &mut dyn RendererHost, Intent) -> Vec<Effect>`, marked `#[must_use]` with "the effects are host work the reducer did not perform; run them or they are lost".
  - `ainb-tui/crates/ainb-app/src/app/events.rs:30`: `trait RendererHost { queue(HostAction); pointer(&AppState, Pos, Btn) -> Option<Intent> }`, with `NoRenderer` at `:47` as the no-layout implementation. The desktop implements this over its own layout.
  - `ainb-tui/crates/ainb-app/src/app/effect.rs:22`: the `Effect` enum. Every variant already documents a "Desktop host:" half beside the "Terminal host:" half, for `AttachTerminal`, `Detach`, `OpenEditor`, `PasteClipboard`, `RunDaemonAction`, `RunPluginAction`, `ForwardToPlugin` (`:94`, landed as #1083) and `Persist` (`:100`). D1 implements the documented desktop half, it does not invent one.
  - `ainb-tui/crates/ainb-app/src/wire/frame.rs`: `Frame` (`:76`) carrying section name, version, `epoch`, `host_id`, an optional `daemon_read` and a private redacted `body`; `FrameBatch` (`:150`) with its `oversize` list; `MAX_FRAME_BYTES` at 4 MiB (`:136`); `Subscription` (`:200`), wire-encoded as section names so an unknown name is skipped rather than fatal; `Mirror` (`:260`) with `new`, `resubscribe` and `batch`.
  - Nothing under `ainb-tui/crates/ainb-core/src` constructs a `Mirror` today. D1 is the first production frame producer in the tree; the only existing consumer is a test renderer.
  - `ainb-tui/crates/ainb-app/src/wire/mod.rs`: `section_json` and `serialize_section` are the only path from state to JSON, and the redaction lives below them. `daemon_read` names which sections carry a daemon revision and clock.
  - `ainb-tui/crates/ainb-app/src/wire/store.rs`: `MirrorStore` (`:78`) with `apply_drain`, `on_commit`, `section_by_host`, `evict_host` and `resubscribe`; `Scalar` (`:50`) with no list or object variant; `RootSelector` (`:59`). This is the Rust reference implementation of the D15 invariants and the desktop store's specification.
  - `ainb-tui/crates/ainb-app/src/wire/web.rs`: `WebSessionRow` and `session_rows`, the worked example of projecting a redacted Sessions frame into a renderer's row shape without reintroducing a withheld field.
  - `ainb-tui/crates/ainb-app/src/wire/shape.rs`: the sample state and the key-path tracing behind the committed fixture.
  - `ainb-tui/crates/ainb-app/bindings/AppState.ts`: 4,922 generated lines behind the `typescript-bindings` feature, regenerated and diffed by the CI job "TypeScript bindings freshness" and type-checked against sample frames by `ainb-tui/crates/ainb-app/bindings/package.json`. `sample.check.ts` is generated, not committed (`bindings/.gitignore`).
  - `ainb-tui/crates/ainb-app/src/wire/store.rs` is the desktop store's specification: keys of `(host_id, section)`, per-host epochs, the channel peer and never the frame choosing the key (`:182`), `MAX_HOSTS`, `evict_host`, effects after commit. `ainb-tui/bench/mirror-fanout/bench.mjs` contributes the drain shape only, one `batch()` per 16 ms drain with `reconcile` per section (`:220`) and removed keys cleared (`:234`); its `applyUnit` keys by section alone (`:213`) and its header excludes epoch and peer handling, so it is a performance reference, not a semantic one. SolidJS is pinned at 1.9.15 in `ainb-tui/bench/mirror-fanout/package.json`. The TypeScript store carries host id and epoch from day one.
  - `ainb-tui/crates/ainb-hangar-proto/src/connections.rs`: `SurfaceKind::Desktop` (`:17`), `SurfaceKind::Plugin` (`:25`), `SurfaceInfo` (`:56`), `SurfaceHost` (`:81`), `ConnectionRow` with `attributed_kind` (`:114`), `ConnectionsListResult` (`:121`).
  - `ainb-tui/crates/ainb-hangar-client/src/presence.rs`: `PresenceLease::spawn` (`:107`), `mark_process_as_surface` (`:63`), `PresenceState` (`:74`). Its own header names the desktop as the next consumer: a surface that only polls never lists, so a held lease is how the daemon knows the desktop is live.
  - `ainb-tui/crates/ainb-hangar-client/src/lib.rs:29` re-exports `Dialer`, `PresenceLease`, `PresenceState` and `mark_process_as_surface`.
  - `ainb-tui/crates/ainb-hangar-daemon/src/single_instance.rs`: the flock on `<home>/hangar/daemon.lock`, taken as the first statement of boot, loser exits 0. The sidecar supervisor rides this and adds no second guard.
  - `ainb-tui/crates/ainb-web/src/terminal.rs` with its route at `routes.rs:235`: the WS terminal bridge `GET /ws/session/:id`, bearer token accepted via `?token=` on this route only, raw PTY bytes as binary frames, JSON control frames for input, resize and ping.
  - `ainb-tui/crates/ainb-core/src/effect_host.rs:31` and `ainb-tui/crates/ainb-core/src/terminal_clients.rs`: the terminal host's stateless executor and its client ownership, the shape the desktop executor copies.
  - `ainb-tui/crates/ainb-app/src/config/persist.rs`: the one writer every host uses for a store write, so the desktop writes the same files the same way.
  - `ainb-tui/scripts/proof/run.sh` with `lib.sh` and `scenarios/*.sh`: the proof harness, one `result.json` per node with expected versus observed, exit 0 only when every node passed.

· Locked decisions, as the specs state them. A node PR does not reopen one; a change needs a spec amendment PR first ("Rules of the road" in the programme doc).
  - D10 tmux: hybrid. tmux stays PTY owner, multi-attach substrate and truth; the daemon adds a headless VT emulator as a cache for snapshot, status and per-viewer flow control; the feed is tmux control mode.
  - D11 host model: peer daemon on the box, `HostId` on every wire type and schema row, `SessionRef { host_id, session_key }` on every cross-host surface, `ssh -L` one carrier among LAN and tailnet. D4's host switcher and transport moved into R1.
  - D12 relay: none now. The pairing offer carries an optional `relay` field from day one.
  - D13 off-box transport and auth: WebSocket carrying today's JSON-RPC envelope, Noise IK, host static key pinned in the pairing offer, single-use invite redeemed inside the Noise session, per-device revocable tokens, scope allowlist at dispatch and at subscribe.
  - D14 agent status: six tiers, hook push highest and pane text lowest. Only tiers 0 and 1 open a turn or assert needs-input. Silence is `unverifiable` or `idle`, never `done`. The store is the `fleet_session` and `fleet_event` family with one writer.
  - D15 renderer contract: Plan B with four invariants. Frames name changed sections. One store transaction per channel drain, effects after commit. Root selectors return scalars, never lists or objects. Renderers may subscribe to a section subset. Specta sits behind the one workspace feature `typescript-bindings` with a CI freshness diff.
  - D16 mobile: Expo and React Native, xterm in a webview, thin RPC client, interactive terminal in v1 behind a separate `mobile+type` pairing scope.
  - D17 wire versioning: one integer `PROTOCOL_VERSION` in `auth/hello` as `{min, max}`, capability strings negotiated both ways, handshake-negotiated opcodes with permanent numbers, a two-direction skew harness including the local leg.
  - D18 mutations: a client-minted opaque 128-bit op id plus a mutation-specific fence on every mutation; the daemon commits then replies; receipts written in the same SQLite transaction as the state flip.

· What D1 must NOT do:
  - No second state machine. The desktop holds no reducer logic, no derived session model, no shadow copy of a section. Flow state belongs to the section, navigation state to the renderer, per the split table at base spec `:97-101`.
  - No raw daemon reads for anything a section already carries. Sections arrive as frames from `ainb-app`. The desktop's only direct daemon traffic is the presence lease, the sidecar handshake and the terminal WS.
  - No PTY in the renderer. The webview receives bytes over the WS bridge and nothing else. `portable-pty` left `REACHABLE_TODAY` in P5a and the fence at `ainb-tui/crates/ainb-app/tests/host_side_effects.rs:53` never grows.
  - No new wire field without the key-path fixture and the bindings regenerated in the same PR: `ainb-tui/crates/ainb-app/tests/fixtures/section_key_paths.txt`, the fixture test at `tests/state_serde.rs:99`, and `bindings/AppState.ts`.
  - No deltas on the wire. Whole section bodies frame and the key-path diff happens in the renderer, the granularity decision recorded in `docs/plans/goals/2026-09-14-p5-hosts-close-slice-2.md`.
  - No `Serialize` on a section or on `AppState`, and no serialisation call site outside `wire/`: `tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
  - No `cmd+` chord through `Chord::parse`. It rejects `cmd`, `command`, `super` and `meta` outright with "cmd/super chords belong to the desktop renderer, not the TUI" (`ainb-tui/crates/ainb-app/src/app/keymap.rs:149`). Desktop accelerators resolve in the shell and enter the reducer as `Intent::Command(CommandId, Args)`.
  - No host-only state on a frame. `HostOnlyState` never derives `Serialize` and a probe test proves it (`tests/host_side_effects.rs:500`).
  - No edits to `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule.
  - No command that bypasses the gate. `Intent::Command` resolves through the overlay precedence P5d landed (`tests/command_gate.rs`); a desktop click on a pane under a dialog changes nothing, exactly as in the TUI.

· What D1 draws, from the screen inventory at base spec `:206-225` and the interface table at `:192-205`. Everything else in that inventory is D2 or D3.
  - Sidebar nav, which is the home menu merged into the shell.
  - Sessions sidebar plus terminal tabs, where the TUI's tmux preview becomes a live tab.
  - The palette, which merges the help overlay and the shortcuts page.
  - The header strip: host label, attention counts, settings entry.
  - Not in D1: the board, the answer box, the ACP chat card, the review tab, the inbox, the stats tab, the plugin fallback cell, the settings page, the daemons panel, the new session form, the modals. Those are D2 and D3 and their rows stay planned.

· D1's own shell data model, from base spec `:123-152`. None of it is a section and none of it goes on a frame.
  - `HostApp`: `host_id`, label, transport, connection state. One per host, holding one `AppState` and N terminal tabs. In this node there is exactly one, local; the host set is R1's under D11.
  - `TerminalTab`: `session_id`, `host_id`, `ws_url`, `attached_at`. Capped at 8 per host.
  - `DesktopShell`: hosts, active host, sidebar width, window, open tabs, persisted to `~/.agents-in-a-box/desktop.json`.
  - Desktop accelerators live in a `desktop_accelerators` table in the shell and resolve to `Intent::Command(CommandId, Args)`, because `Chord::parse` rejects `cmd`, `command`, `super` and `meta` outright (`ainb-tui/crates/ainb-app/src/app/keymap.rs:149`) and surface-safety `:58` reserves that prefix for this renderer. `~/.agents-in-a-box/keymap.toml` still merges for every non-`cmd` binding and the desktop adds no second override format; whether it grows a `[desktop]` table is filed, not decided here. The "rejected at load" rule below applies to the desktop table, not to that file, which cannot express a `cmd` chord at all.

· Edge cases D1 must handle, quoted from base spec `:250-263`. Each one needs a test or a named deferral on its PR.
  - A 9th terminal tab detaches the oldest idle tab, a toast names it, and the tab stays listed and re-attaches on click.
  - The daemon socket vanishing: reconnect with 1 s, 4 s, 16 s backoff, a "reconnecting" banner, sections frozen with a stale badge, resync on hello.
  - The daemon down after three spawn retries: a degraded banner with "show log" and "retry", and the sidebar still listing tmux sessions from fleet discovery.
  - An invalid `keymap.toml`: the file is ignored, a toast names the line, defaults apply.
  - A keymap conflict with the terminal: only `cmd`-prefixed chords are allowed while the terminal has focus, and anything else is rejected at load.
  - Both TUI and desktop drawing the same flow: last input wins, which is accepted and is what two TUI clients already do.
  - Linux with no `cmd` key: the layer maps `cmd` to Super, falling back to Alt when the window manager captures Super.
  - An ACP session with no tmux: no terminal tab, and the chat card is the session detail. In D1 that means no tab and no error, since the card itself is D2.
  - Channel backpressure: the send coalesces to the latest per section and never queues a stale version. `Mirror` already gives this, because a batch carries the current version or nothing.

· Error surfaces D1 owns, from base spec `:330-341`: a failed hello showing the token path and a copy-fix command, a dropped WS terminal keeping its buffer behind a "reconnecting" overlay with three auto-redials then a "reattach" button, a vanished tmux session closing the tab with a toast and marking the row exited, and a missing sidecar binary showing the degraded banner with install instructions.

· Working dir: the Orca worktree this session was launched in, `/home/claude/.orca/agents-in-a-box-desktop-app-part2` on claude-hetzner.
  - Branch `stevengonsalvez/d1-desktop-shell` from `origin/v2` at `b081c09cf`, after P5d (#1065), #1083 and #1094.
  - This box shares RAM with other sessions: build with `-j 4` and `CARGO_INCREMENTAL=0`, one test invocation at a time, push after every commit.

· Audience: D2 (board, attention, answer, ACP card) and D3 (review, inbox, settings), which extend this shell; lane K, whose frames this node is the first production consumer of; R1, which replaces the single local host with a real host set; the security reviewer who checks that no frame body and no terminal byte reaches the webview outside the two named seams; and Stevie.

─ THE SIDECAR DECISION ─

The spec is explicit. Option (a): the desktop embeds `ainb-app` in Tauri's Rust side as a host exactly as `ainb-core` does. "Sidecar" in this programme means the bundled `ainb-hangar-daemon` binary, not a state-machine host process.

```
┌──────────────────────────────┐        ┌──────────────────────────────┐
│ ainb (TUI binary)            │        │ ainb-desktop (Tauri v2)      │
│ ratatui draw · UiState       │        │ SolidJS · xterm.js           │
│ effect_host · TerminalClients│        │ DesktopHost · Mirror pump    │
└──────────────┬───────────────┘        └──────────────┬───────────────┘
               │ Intent in · Vec<Effect> out           │ + Channel<FrameBatch>
               ▼                                       ▼
┌───────────────────────────────────────────────────────────────────────┐
│ ainb-app: AppState (20 Versioned sections) · dispatch · wire::frame   │
└──────────────┬────────────────────────────────────────────────────────┘
               │ ainb-hangar-client: hello · subscribe · PresenceLease
               ▼
┌───────────────────────────┐   spawn if no socket, flock decides
│ ainb-hangar-daemon        │◀── the sidecar. Bundled, never killed on exit
└───────────────────────────┘
```

Citations, all in `docs/plans/2026-09-04-desktop-shared-core-spec.md`:
· `:14` "`ainb-desktop`, a Tauri v2 app equal to the TUI, both rendering the same `ainb-app` state machine".
· `:37-73` the architecture diagram: the TUI box and the `ainb-desktop` box both sit above one `ainb-app` box, joined by "Intent in · AppState sections out · Vec<Effect>".
· `:69` the component table: `ainb-desktop` owns "shell state (hosts, tabs, layout), Solid components, WS terminal client, sidecar supervisor, in-tree plugin components". The state machine is not in that list, and the supervisor is a separate responsibility in it.
· `:88` the renderer contract table: the consumers of `AppState` are "ratatui draw, Tauri `Channel<T>`", so frames cross an in-process channel, not the hangar wire.
· `:278` the local transport row: "attach if hello answers, else flock + spawn `ainb-hangar-daemon` sidecar (bundled, target-triple named); never kill on exit".
· `:282` "The desktop sidecar supervisor reuses it: spawn, and if the child exits 0 within the grace window, attach to the winner."
· `:367` packaging: "Sidecar: `bundle.externalBin` = `ainb-hangar-daemon`, copied to `src-tauri/binaries/<name>-<triple>` by a workspace `xtask` step".

So W0-wire and W0-mirror do different jobs in this node. The frames ride a Tauri channel inside one process. The hangar protocol is how that one process talks to the daemon, through `ainb-hangar-client`, exactly as the TUI does.

Settled, not a lane question: the local terminal has no listener. The Rust side owns the `tmux attach-session` PTY (the `PtyBridge` shape at `ainb-tui/crates/ainb-web/src/terminal.rs:274`) and streams bytes to xterm.js on a second Tauri `Channel<Vec<u8>>`, with input and resize as `invoke` commands. No loopback TCP, no `?token=` in a URL, nothing a visited web page can reach. `ainb-web` is not linked: its handler takes `crate::routes::AppState` (`terminal.rs:48`), so there is no module to lift, and its auth has no `Origin` check ("no token, all requests allowed", `auth.rs:122`). If a later lane ever binds a local WS, it binds `127.0.0.1` on port 0, requires the token, and rejects any `Origin` outside the app's own. The frontend hides both legs behind one `TerminalTransport` with `send`, `resize` and `onBytes`; R1 implements it over the box's `ainb-web` WS. Spec `:195` names a WS URL for the local leg, so D1a opens a one-paragraph spec amendment PR first, per the programme's rules of the road.

─ SCOPE, STAGED AS PRs ─

Four PRs. Each is mergeable alone, targets `v2`, and carries its own proof.

**D1a, the crate, the embedded host and the sidecar.**
· Creates: `ainb-tui/crates/ainb-desktop/` as an excluded workspace with its own `Cargo.toml` and lockfile, added to `exclude` in `ainb-tui/Cargo.toml` and never to `members`. The Tauri v2 Rust side, `tauri.conf.json`, and the frontend tree under `ainb-tui/crates/ainb-desktop/ui/` with `frontendDist` pointing at its build output.
· Seams consumed: `ainb_app::dispatch`, `RendererHost`, `Keymap`, `Effect`, and `wire::frame::{Mirror, Subscription, FrameBatch}`.
· Builds: a `DesktopHost` implementing `RendererHost` over the shell's own layout; a desktop effect executor covering the effects this node can reach (`Detach`, `OpenEditor`, `PasteClipboard`, `Persist`, `RunDaemonAction`) and returning the documented failure report for any it cannot; a frame pump that calls `Mirror::batch` after every dispatch and every tick and sends a non-empty `FrameBatch` on a Tauri channel.
· Takes the config as an argument. `AppState::default` loads `AppConfig` from disk (`ainb-tui/crates/ainb-app/src/app/state.rs:3330`), which a second host must not do implicitly. D1a either adds a constructor that accepts the config, or documents in one line the divergence it inherits and files the fix.
· Proof: `ainb-tui/crates/ainb-desktop/tests/host_contract.rs`, headless, no window. The first batch frames every subscribed section and nothing else; a section that did not move frames nothing; effects come back from `dispatch` and run after the state write; constructing the host with an injected config touches no file under `HOME`.
· Gate: `cargo test -p ainb-desktop`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`.

· Still D1a, the sidecar supervisor, the connection state machine and presence. Touches: `ainb-tui/crates/ainb-desktop/`, plus one step in `ainb-tui/xtask/` that stages the built `ainb-hangar-daemon` at `binaries/ainb-hangar-daemon-<triple>` for `bundle.externalBin`.
· Seams consumed: `ainb_hangar_client::{DaemonClient, PresenceLease, mark_process_as_surface}`, `SurfaceKind::Desktop`, and the daemon's existing flock in `single_instance.rs`.
· Implements the spec's state machine at `:265-283`: probe the socket, hello, connected; no socket means try the flock, spawn the bundled sidecar on a win, wait for hello and attach on a loss; a child that exits 0 inside the grace window means attach to the winner; three crash retries then degraded with "show log" and "retry"; never kill the daemon on app exit; reconnect and resync stay inside `ainb-hangar-client`.
· Proof: `ainb-tui/crates/ainb-desktop/tests/sidecar.rs` against the real daemon binary in a private hangar home. A cold start spawns it. A second host attaches to the winner instead of spawning a second. `hangar/connections_list` shows exactly one `desktop` row while the app runs and none after it closes. The daemon survives the app closing. A `SIGKILL` on the daemon leaves the app degraded and then reconnected.
· Gate: `cargo test` in the excluded workspace, `cargo clippy -- -D warnings` and `cargo fmt --check` there, plus the sidecar test on ubuntu and macos, in the new desktop CI job.

**D1b, the SolidJS renderer and the sessions sidebar.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/`.
· Seams consumed: `ainb-tui/crates/ainb-app/bindings/AppState.ts` for types, `ainb-tui/bench/mirror-fanout/bench.mjs` for the store apply, `ainb-tui/crates/ainb-app/src/wire/web.rs` as the worked projection example.
· Builds: the frame receiver and the Solid store with the four D15 invariants keyed by `(host_id, section)` with per-host epochs, one `batch()` per drain with effects after the commit and scalar-only root selectors, and a test that two hosts' Sessions sections do not merge and that a larger epoch drops that host's held sections; the sessions sidebar grouped by workspace with the attention ring per row; the shell chrome (header counts, sidebar, tab strip) as the interface block draws it at base spec `:155-170`.
· Proof: the wdio journey's sidebar leg, plus a store check on the TypeScript side that an unsubscribed section applies nothing and that every root selector returns a scalar, mirroring `ainb-tui/crates/ainb-core/tests/mirror_renderer.rs:228`.
· Gate: `tsc --noEmit --strict` over the app's own sources with the generated bindings, in CI.

**D1c, the terminal tab.**
· Touches: `ainb-tui/crates/ainb-desktop/` for the PTY owner and the byte channel, and `ui/` for xterm.js behind `TerminalTransport`.
· Seams consumed: the `PtyBridge` shape in `ainb-tui/crates/ainb-web/src/terminal.rs` as the reference for the Rust-side PTY owner (not linked), and `Effect::AttachTerminal` and `Effect::Detach` as the reducer's way of asking for a tab.
· Implements: one tab per session, capped at 8 per host with the 9th detaching the oldest idle tab behind a toast that names it; the "reconnecting" overlay with three redials; the focus rules at base spec `:239-246`, where terminal focus keeps only the shell accelerators and sends everything else to the pane.
· Proof: the wdio journey's terminal leg against a real tmux session. Seeded pane output paints in the tab and a typed line reaches the pane.
· Throughput: the spec's gate is 50 MB in under 2 s with zero dropped bytes (`:350`). It runs on the ubuntu leg. If webkit2gtk plus the xterm WebGL addon cannot reach it, the measured number is recorded and the gap is filed for D4' rather than dropped quietly.
· Gate: as D1b, plus the journey leg.

**D1d, the palette, the proof scenario and the programme row.**
· Touches: `ainb-tui/crates/ainb-desktop/`, `ui/`, `ainb-tui/scripts/proof/scenarios/d1-shell.sh`, `.github/workflows/ci.yml`, `docs/plans/2026-09-12-desktop-programme.md`.
· Builds: the palette on the shell accelerator, fuzzy over the merged keymap table and the `CommandId` registry plus the live sessions, dispatching `Intent::Command`. Hosts are not in the palette in this node; the host set arrives with R1 under D11.
· Adds the proof scenario `d1-shell` in the shape of `ainb-tui/scripts/proof/scenarios/w0-wire.sh`: an `EXPECT` line and a `scenario` function that launches the real app headless in a private world and checks that the shell reaches the sessions sidebar, that the daemon it found is the one a separate CLI call finds, that `hangar/connections_list` carries its one `desktop` row, and that a session created by the CLI appears in the sidebar without a restart.
· Adds the CI job that runs the wdio journey on `ubuntu-latest` under `xvfb-run` and on `macos-latest` (or the recorded substitute), and the job that builds the app headless on both.
· Flips the D1 row in the programme doc with the run ids, in this PR.
· Gate: the three success criteria below, all green.

─ WHICH EXISTING TESTS MUST STAY GREEN ─

Unchanged on every PR of this node, or changed only in their own commit with the reason in the message:
· `ainb-tui/crates/ainb-app/tests/host_side_effects.rs`: `REACHABLE_TODAY` (no new host-effect crate reachable from `ainb-app`), `CALL_SITES`, `REDUCER_DISK_WRITES`, `HOST_STATE_READS`, the host tmux lookup fence, and the `HostOnlyState` not-`Serialize` probe.
· `ainb-tui/crates/ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
· `ainb-tui/crates/ainb-app/tests/state_serde.rs` with `tests/fixtures/section_key_paths.txt`. A new wire field regenerates the fixture with `UPDATE_SECTION_KEY_PATHS=1` in the same PR, after triage against the deny-lists.
· `ainb-tui/crates/ainb-app/tests/bindings.rs` and the CI job "TypeScript bindings freshness". `AppState.ts` regenerates with `UPDATE_APP_STATE_TS=1` and is committed in the same PR.
· `ainb-tui/crates/ainb-core/tests/mirror_renderer.rs`. The Rust reference renderer keeps passing; the desktop store is a second implementation of the same invariants, not a replacement.
· `ainb-tui/crates/ainb-app/tests/command_gate.rs`, `parity.rs`, `renderer_free.rs`, `terminal_host_contract.rs`, `plugin_actions.rs`, `persistence.rs`, `effects.rs`, `intent_dispatch.rs`, `key_only_commands.rs`.
· CI jobs "Mirror fan-out bench", "TypeScript bindings freshness", "Contracts", "Test (ubuntu-latest)", "Test (macos-latest)", "ainb-core tripwires (ubuntu-latest)", and the excluded-set tripwire workflow.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. The wdio sessions journey is green in CI on `ubuntu-latest` under `xvfb-run` and on `macos-latest`, using `@wdio/tauri-service` at its default `driverProvider: 'embedded'`, against a real daemon and real tmux sessions. That provider requires `tauri-plugin-wdio-webdriver` in the app: it sits behind a `wdio` cargo feature, off by default, and a test asserts the release bundle binds no driver port and exports no WebDriver symbol. Linux installs `webkit2gtk-driver`; both runners install tmux as the existing Test job does. In one run: the app launches, connects or spawns the sidecar and then connects, the sidebar lists the seeded sessions grouped by workspace, selecting a row opens a terminal tab whose pane output paints and which accepts a typed line, the palette opens on the shell accelerator and runs a named command, and a session created by a separate CLI process appears in the sidebar without a restart. If the macOS leg cannot reach a session in one day of lane time, it degrades to a substitute recorded on the PR: macOS runs the Rust host-contract and sidecar tests plus a `tauri build` bundle smoke, and the DOM journey is Linux-only. The 50 MB throughput number (spec `:350`) is measured and recorded, never a pass condition, because spec `:368` still calls it an open spike. The job name and run id are recorded on the PR.

2. The proof harness scenario `d1-shell` passes on a `v2` build. `bash ainb-tui/scripts/proof/run.sh --only d1-shell` writes a `result.json` with `pass: true`, and its `observed` lines carry the daemon pid the desktop found, the one `desktop` row in `hangar/connections_list` while it runs and zero after it closes, and the section names in the first `FrameBatch` the renderer applied. A full harness run still reports every other node passing.

3. Bindings and the wire shape are provably unchanged or regenerated in place. The CI jobs "TypeScript bindings freshness" and the key-path fixture test pass with no manual step; `ainb-tui/crates/ainb-app/bindings/AppState.ts` and `tests/fixtures/section_key_paths.txt` are either byte-identical to their state at `origin/v2` or regenerated in the same commit with the triage reason in the message; `tests/serialize_guard.rs` and the `host_side_effects.rs` fences pass unchanged; and `tsc --noEmit --strict` over the desktop sources plus the generated bindings is clean.

4. Final deliverable runs without errors

5. You can show proof (screenshot · test output · URL)

─ CONSTRAINTS ─

· PRs target `v2`, staged as the four above, one file per commit, GPG-signed with `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`.
· No attribution trailers, no `Co-Authored-By`, and no mention of an assistant anywhere in a commit message or a PR body.
· Open every PR as a draft and flip it ready only when its own gate is green in CI. Never merge your own PR.
· Message the orchestrator with the head sha when a PR is ready and again when its CI goes green.
· No em dashes on any line you author, in code, docs, commit messages or PR bodies.
· Never name the reference product this programme drew prior art from, in code, comments, docs, commits or PR bodies.
· Never touch `ainb-tui/crates/ainb-core/src/app/*`. Merge `origin/v2` before touching a file another lane is on, and name shared files in the PR body.
· Tauri and SolidJS versions are pinned and committed: one Tauri v2 minor, SolidJS at 1.9.15 to match `ainb-tui/bench/mirror-fanout/package.json`, TypeScript at 5.6.3 to match `ainb-tui/crates/ainb-app/bindings/package.json`, Node 22 as CI already uses.
· Every frontend dependency is in a committed lockfile and CI installs with `npm ci`, so the build reaches the network for nothing the lockfile does not name.
· The app must build headless in CI on both runners: no interactive Xcode step, no display needed to compile or to run the unit and host-contract tests, and the wdio leg on Linux runs under `xvfb-run`.
· `cargo clippy --workspace -- -D warnings` and `cargo fmt --all -- --check` in the pre-push gate on every commit.
· Record every decision in this goal file's progress log as you take it, in the voice the sibling goals use.

─ OPERATING RULES, NON-NEGOTIABLE ─

1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose and fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components and real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note and keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

─ QUALITY BAR ─

· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern, env var and decision logged

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

File each as an issue with its evidence. Do not fix it in this node.
· The reducer reads the wall clock: `Instant::now()` in the observer settle and retry rules, in lease renewal and in tick pacing, recorded as a D1 input in `docs/plans/goals/2026-09-14-p5-hosts-close-slice-2.md`. Replaying a second host's intents needs time on the intent or a host clock the reducer is given.
· `AppState::default` loading `AppConfig` from disk (`state.rs:3330`), if D1a documents the divergence rather than adding the injecting constructor.
· A frame cache keyed by `(screen, viewport)` so two hosts at different sizes do not share one plugin render, named as a pre-D1 follow-up in the P5 log, together with #1046 (a per-host screen watch lease, which needs a host id on the intent).
· The Config section's size after the changelog split (#1052, closed by #1076). Measure what the desktop actually receives on a settings toggle and file anything near the 4 MiB ceiling.
· The presence badge naming the other live surfaces from `hangar/connections_list`, which the spec asks for as "also open in: tui, web" but which no section carries today.
· The sidebar width key. The TUI stores `sessions_sidebar_fraction` after P5c; whether the desktop shares that key or keeps its own in `desktop.json` is a one-line decision to file rather than pick silently.
· `Effect::AttachTerminal(TerminalTarget::InPlace)` on a desktop host. P5a made in-place portable through a report, so if D1d's answer differs from the report contract, the difference belongs on the effect doc.
· Plugin screens with no desktop component, which the spec resolves as a `WireBuffer` painted into an xterm cell (`:259`). D1 draws no plugin screen; the fallback cell is D3's.

─ OPEN QUESTIONS THE LANE ANSWERS ON A PR BODY ─

· The crate layout: `ainb-tui/crates/ainb-desktop/` with the frontend under `ui/`, or the conventional `src-tauri` nesting if the Tauri CLI fights it. Answer on D1a.
· What the desktop subscribes to in this node. Recommended: Sessions, Shell, Tmux, Fleet, Config and `AgentStatus`, because that is what the sidebar, the header counts and the terminal tabs draw, and the rest arrives with D2 and D3. Answer on D1b with the list.
· Where the wdio journey lives. The spec puts the parity half in `ainb-app/tests/parity` and the DOM half in `ainb-desktop/e2e` (`:348`). Confirm the second path on D1d, since nothing under that name exists yet.
· How a shell accelerator reaches the reducer. Recommended: as `Intent::Command(CommandId, Args)`, because `Chord::parse` rejects `cmd` unconditionally and widening it would change the TUI's keymap vocabulary. Answer on D1d if the lane takes another route.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (screenshot, test output, URL). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the four PRs:
1. The spec amendment for `:195` (the local terminal has no listener), as its own docs PR: #1111.
2. D1a: the excluded crate, `AppState::with_config`, the embedded host with its frame pump, the desktop executor, the sidecar supervisor with presence, the xtask staging step, the Tauri window and a first frontend tree, the desktop CI job.
3. D1b: the Solid store with the D15 invariants and the sessions sidebar.
4. D1c: the Rust-owned PTY on a byte channel and the terminal tab.
5. D1d: the palette, the `d1-shell` proof scenario, the wdio job, the programme row.

D1a, on `stevengonsalvez/d1a-desktop-host`:
- `ainb-tui/crates/ainb-desktop/` is its own workspace with its own `Cargo.lock`, in the parent's `exclude`. The library (`host`, `executor`, `sidecar`) builds and tests without Tauri; the window is a `[[bin]]` behind the `app` feature, and `build.rs` runs `tauri-build` only for it. So `cargo test` needs no webview, and `cargo clippy --all-features` still covers the window.
- `AppState::with_config(AppConfig)` builds the state on a given config; `Default` loads from disk and calls it. The desktop loads the config itself in `main.rs` and hands it over, so the injecting constructor exists and no divergence is inherited.
- `DesktopHost` owns the `AppState`, the `Keymap`, a `Mirror` and a `FrameSink`. `dispatch` applies an intent and frames what moved before returning its effects; `tick` drains the outbox and frames; `run` executes each effect after the write and applies the reports the same way, cut after 32 rounds; `reframe` sends every subscribed section again to a renderer that attaches late. `DesktopLayout` is its `RendererHost`: layout work is queued for the webview, and `pointer` finds nothing because the webview hit-tests its own DOM and sends the command.
- `DesktopExecutor` runs `Detach` (the `detached` report), `OpenEditor` (the terminal host's resolution), `PasteClipboard` (arboard, else `clipboard_failed`), `Persist` (the shared writer, else `persist_failed`) and `RunDaemonAction` (`ainb daemon` from `PATH` on a worker, reported through `take_deferred`). Every `AttachTerminal` target is answered with its documented failure until D1c: `in_place_failed` and `observer_failed` with `unsupported`, `attach_finished` failed, `shell_prepared` failed, `login_finished` not ok. `ForwardToPlugin` with `back` reports `plugin_input_undelivered`, `RunPluginAction` reports `plugin_action_undelivered`.
- The sidecar supervisor probes with hello, spawns the bundled daemon in its own session (`setsid`, stdout and stderr to `<home>/hangar/desktop-sidecar.log`), attaches to the winner when the child exits 0 inside the grace window, gives up as `Degraded` after three failed spawns, and holds a `PresenceLease` as `SurfaceKind::Desktop` once connected. A lost presence connection moves to `Reconnecting` and runs the probe again, which respawns a killed daemon. Dropping the supervisor closes the lease and never signals the daemon.
- `cargo xtask stage-desktop-sidecar [--release]` builds the daemon and copies it to `crates/ainb-desktop/binaries/ainb-hangar-daemon-<host triple>` for `bundle.externalBin`.
- The window (`main.rs`) subscribes Sessions, Shell, Tmux, Fleet, Config and AgentStatus, sends frames on one Tauri `Channel<FrameBatch>` after `invoke("subscribe")`, takes intents through `invoke("dispatch")`, ticks every 250 ms, and emits the sidecar state as the `sidecar` event. `ui/` is vite with Solid 1.9.15 and TypeScript 5.6.3 under strict `tsc`, and draws the connection banner, a retry on degraded, and the sections this window holds.
- Evidence: `tests/host_contract.rs` (5: first batch frames exactly the subscription, an unmoved section frames nothing, the config write is framed before its persistence effect runs, reframe, an injected config writes nothing under `HOME`); `tests/sidecar.rs` (4, the real daemon in a private home: cold start spawns and lists one desktop row that goes when the app closes while the daemon lives on; two hosts on a cold home end on one daemon with exactly one spawner; a SIGKILLed daemon leaves the app reconnecting and then connected to a fresh pid; a binary that always fails leaves it degraded naming the log). The window ran headless under `xvfb-run` against a private home: it spawned the daemon, and the daemon outlived the app.
- CI: `.github/workflows/desktop.yml`, one job per runner: the Tauri packages on Linux, `npm ci` and the frontend build, the staging step, `cargo fmt --check`, `cargo clippy --all-targets --all-features -D warnings` and `cargo test` in the excluded workspace.

Decisions:
- "Reconnecting" is the banner a lost daemon shows while the supervisor finds one again; "Degraded" is only the state after the spawn retries are spent, which is when "show log" and "retry" apply.
- The crate layout is `ainb-desktop/` with the Tauri config at the crate root and the frontend under `ui/`; the Tauri CLI did not need the `src-tauri` nesting.

D1a review (#1116, "Review of ed3069109"), applied on `stevengonsalvez/d1a-desktop-host`:
- The host and the executor sit behind one `Mutex` in `ainb_desktop::shell::Shell`, so the window's `dispatch` and its tick cannot take two locks in opposite orders; `tests/shell.rs` runs 500 ticks against 200 dispatches and requires the dispatches to return.
- The bundled daemon resolves beside the executable. `AINB_DESKTOP_DAEMON_BIN` is honoured in a debug build only, there is no `PATH` fallback, and a build that cannot place its own executable fails at setup.
- The supervisor bounds the wait for presence by `hello_budget` and degrades past it; reconnects back off 1 s, 4 s, 16 s (`SidecarConfig::reconnect_backoff`) and a fourth loss in a row degrades, with the count starting over after a connection held for a minute; Retry leaves Degraded whatever put it there. A daemon child that never answers is killed and reaped; the one that became the daemon is reaped by a thread whenever it exits.
- The window's tracing goes to `<hangar home>/desktop.log`.
- `ainb` for daemon verbs resolves once from absolute `PATH` entries to an executable file, and the choice is logged.
- The desktop workflow's `paths` filter applies to pull requests as well as pushes.
- The webview gets `SidecarView`: no daemon pid, error text with every path replaced by `<path>` (`scrub_paths`), and `has_log` with a `show_log` command returning the last 64 KiB of the sidecar log. The paths stay in `SidecarState` and in the log file.
- Carried into D1b by the review: narrow `dispatch` to a renderer intent subset, take the tick from `ui.app_tick_ms`, a `Mirror::reframe()` helper, test HOME isolation. Required by D1d: a bundle smoke step in CI and a `productName` that is not `ainb`.
