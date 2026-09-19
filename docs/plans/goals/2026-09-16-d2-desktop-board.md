# /goal D2 is on v2: the desktop shell D1 landed grows a board of agent cards drawn from the `agent_status` and Fleet frames, an attention banner a person answers from without leaving the window, an answer that rides the reducer's one `AskState::send` path through the daemon's verified last-mile send and reads as answered on every surface, and an ACP card that frames a session's conversation as a redacted projection, proved by a wdio answer journey green on CI and a `d2-board` proof scenario

─ CONTEXT ─

· First act in the worktree: the lane runs on claude-hetzner under `~/orca/workspaces/agents-in-a-box/p1-ainb-app`, whose checkout sits on `v2`. Run `git fetch origin v2 && git switch -c d2-desktop-board origin/v2` before any edit, then copy this file to `docs/plans/goals/2026-09-16-d2-desktop-board.md` and commit it as the first signed commit. Every later file change is its own signed commit, and never `git add -A`. Push the branch and open each PR as a draft against `v2`. Message the orchestrator (session `agents-in-a-box-desktop-app-part2-28`) with the head sha at each PR, and again when its CI goes green; never poll CI yourself, the orchestrator brings the verdict.

· Project: agents-in-a-box (ainb) desktop programme, slice 3 node D2.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:133`, "D2 board, attention, answer, ACP card | 3 | planned | P3 | wdio answer journey | base spec D2". Gate to start: P3, met (#1008 at `36e84831`, programme `:125`). Gate to finish: "wdio answer journey".
  - The node this one extends: D1, programme `:132`. D1a #1116 at `6fd991ea4`, D1b #1130 at `e80066f98`, D1c #1137 at `909be1c69`, #1140 and #1143 for #1131, D1d #1154 merged at `ccfa4afd2`, which is the `v2` tip this node starts from. Its goal file is `docs/plans/goals/2026-09-15-d1-desktop-shell.md` and its carries are #1155 to #1162.
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: the D2 row (`:116`), the "D2 after P3" rule (`:120`), the architecture diagram (`:37-63`), the component table (`:65-73`), the renderer contract (`:75-101`), the interface block and surface table (`:155-204`), the D1 amendment on the local terminal (`:206`), the screen inventory (`:208-227`), the answer happy path (`:231-239`), the focus rules (`:241-248`), the edge cases (`:250-265`), the hosts and daemon state machine (`:267-287`), the surface-clash hazards (`:289-330`), the errors table (`:332-343`), the testing strategy (`:345-356`) and packaging (`:358-370`).
  - Multi-surface spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md`: D10 to D18 at `:49-61`.
  - Surface-safety plan `docs/plans/2026-09-05-desktop-p0-surface-safety.md`: the chord grammar that reserves `cmd+` for this renderer (`:58`), the draw path sealed on `&AppState` plus a renderer-local `UiState` (`:48`), and its own scope line "no desktop crate (D1)" (`:38`).
  - Sibling goals whose format and voice this one matches: `docs/plans/goals/2026-09-15-d1-desktop-shell.md` and `docs/plans/goals/2026-09-16-p6-client-web.md`.

· Stack:
  - Rust workspace under `ainb-tui/`. `ainb-tui/crates/ainb-desktop` is the one excluded workspace, in the parent's `exclude` and never in `members`, with its own `Cargo.toml` and `Cargo.lock`, so the 29-crate `--all-features` Test job never grows a webkit2gtk dependency.
  - Frontend: Tauri v2, SolidJS 1.9.15, xterm.js 6, vite, TypeScript 5.6.3, Node 22, all pinned in `ainb-tui/crates/ainb-desktop/ui/package.json` and `package-lock.json`, installed with `npm ci`.
  - The journey runner is `@wdio/tauri-service` at `driverProvider: 'embedded'`, under `ainb-tui/crates/ainb-desktop/e2e/` with its own `package.json` and lockfile; the embedded WebDriver sits behind the `wdio` cargo feature, off by default.
  - CI is `.github/workflows/desktop.yml` with three jobs: `desktop` (`:45`, fmt, clippy, the frontend store tests, the host contract and sidecar tests, per runner), `e2e` (`:107`, the journey under `xvfb-run -a npm test` on Linux at `:162`, the macOS leg switched by the `MACOS_E2E_LEG` env at `:42`, its recorded substitute at `:182`, the leg recorded and asserted at `:191`, the throughput figure uploaded at `:210`) and `bundle` (`:220`, the release smoke that asserts no WebDriver). `.github/workflows/ci.yml` holds the workspace jobs D2 must not disturb.
  - The proof harness is `ainb-tui/scripts/proof/run.sh` with `lib.sh`, `summarize.py` and the scenarios under `scenarios/`. `ALL_NODES` is the array at `run.sh:71-78`, nineteen entries ending in `d1-shell`; `:81` fails the run if a named node has no scenario file. `lib.sh:78` is `skip`, which records a node that cannot run on this box rather than failing it (`SKIPPED` at `:43`, the field at `:101-109`).

· What is on `v2` that D2 builds on, seam by seam. Every path and line below exists on `origin/v2` at `ccfa4afd2`.

  The desktop shell D1 landed. `ainb-tui/crates/ainb-desktop/`, 3,404 lines of Rust and 1,437 of TypeScript, none of it a second state machine.
  - `src/lib.rs:19-25` is the whole module list: `clipboard`, `executor`, `host`, `intent`, `shell`, `sidecar`, `terminal`. Its header states the shape D2 keeps: none of it needs a window, the Tauri binary wires these to channels and commands behind the `app` feature, and the tests drive them headless.
  - `src/host.rs:95` is `DesktopHost<S: FrameSink>`, holding the `AppState`, the `Keymap`, a `DesktopLayout` (`:37`) and a `Mirror`. `new` (`:110`) takes the config as an argument through `AppState::with_config`, so nothing is read from disk for it. `dispatch` (`:144`) applies an intent and frames what moved before returning its effects; `tick` (`:161`) is the one D2 extends; `run` (`:197`) executes each effect after the write and folds its reports back, cut after `MAX_REPORT_ROUNDS` (`:61`, 32); `subscribe` (`:306`), `resubscribe` (`:275`) and `reframe` (`:282`) serve a webview that attached or reloaded; `pump` (`:312`) is the only send.
  - What `tick` already does, line by line, because D2 adds to exactly this list: `check_workspace_loading_complete` (`:163`), the cadence stamp from the end of a scan (`:167`), `fleet::attention_poll::spawn` read by shared reference so Fleet does not bump (`:173`), `state.refresh_attention(...)` whose cadence lives in the reducer (`:182`), the rescan when `WORKSPACE_RESCAN` has elapsed and no scan is running (`:186`), and `state.take_effects()` (`:189`).
  - `WORKSPACE_RESCAN` (`src/host.rs:91`) is `AppState::workspace_rescan_floor() + 5s`, measured from the end of a scan. D1d's review fixed the cadence there, so D2 does not re-pick a number: the floor is the state's own, and #1156 adds the daemon-news trigger beside it.
  - `src/host.rs:238` is `palette()`, built from `crate::intent::palette_offers` and `keymap::command_contexts`; `:65` is `PaletteEntry` (id, doc, context, chord, active); `:259` is `key_only_command`.
  - `src/host.rs:223` is `open_sessions`, which moves the reducer from the home screen onto the session list through the home sidebar's own `home.click_sidebar_item` row, twice as a double click, so the host writes no state. D2's board needs the same trick for the tab it opens, and there is no pointer row for it yet (see "THE FOUR SEAMS D2 HAS TO OPEN").
  - `src/intent.rs:12` is `RendererIntent`: `Key(Chord)`, `Command(CommandId, Args)`, `Text(String)`, with no `Mouse`. `:29` is `refused_from_webview`, the one list of host-authored report ids, plugin action ids and `KEY_ONLY_COMMANDS`; `:43` is `palette_offers`, which adds the rows whose action refuses `Args::Null`; `:47` is the `TryFrom` that refuses at the seam. D1d's rule: the palette is built from the same list the dispatch gate refuses by, so the two cannot drift, and `tests/host_contract.rs` walks every offered row through the seam.
  - `src/shell.rs:23` is `Shell<S>`, the host and the executor behind one `Mutex` so a dispatch arriving during a tick waits rather than deadlocking. `dispatch` (`:40`), `dispatch_renderer` (`:49`, which refuses a key landing on a key-only row under the same lock that would apply it), `tick` (`:66`, which runs the effects, drains `take_deferred` and folds the reports), `palette` (`:81`), `open_sessions` (`:86`), `subscribe` (`:95`, which answers the host id of the frames it just sent).
  - `src/executor.rs:35` is `DesktopExecutor`. `execute` (`:79`) covers `AttachTerminal` (`:81`, through `tab_target` at `:141`), `Detach` (`:88`), `OpenEditor` (`:89`), `PasteClipboard` (`:93`), `RunDaemonAction` (`:94`, on a worker reporting through `take_deferred` at `:73`), `Persist` (`:117`, the shared writer), `ForwardToPlugin` (`:121`) and `RunPluginAction` (`:134`), the last two reporting undelivered because `NO_PLUGIN_RUNTIME` (`:29`): this shell runs no plugin runtime. That constant is why the board cannot come from the plugin, below.
  - `src/terminal.rs:31` `MAX_ATTACHED_TABS` = 8, `:36` `REDIAL_DELAYS`, `:45` `WINDOW_BYTES` = 4 MiB, `:63` `READ_QUEUE` = 64, `:114` `TabView`, `:124` `TabsView`, `:442` `Terminals`, `:467` `open`. D2 touches none of it except to keep the tab strip beside the board.
  - `src/main.rs` is the window. `:31` `ChannelSink`, `:79` `renderer_applied` (the telemetry the proof scenario reads, section names and a session count only), `:140` `palette`, `:146` `terminal_tabs`, `:158` `terminal_output`, `:222` `subscribe`, `:234` `dispatch`, `:242` `sidecar_state`, `:248` `show_log`, `:254` `retry_sidecar`, `:457` the `generate_handler` list every new command joins.
  - `ui/src/main.tsx:32` is `SUBSCRIBED`, the one place the subscription list lives: Sessions, Shell, Tmux, Fleet, Config, `AgentStatus`, `WorkspaceLoad`. `:43` `DRAIN_MS` = 16, `:46` `HEADER_COUNTS` over `ROOT_SELECTORS`, `:54` `TOAST_MS`, `:224` the `subscribe` invoke that answers the host id.
  - `ui/src/store.ts:93` `createFrameStore`, keyed by (host id, section) under per-host epochs with the channel's peer as the key, `MAX_HOSTS` = 64 at `:84` and the refusal at `:99`, one `batch()` per drain, effects after commit, host-keyed oversize staleness. This is the second implementation of the four D15 invariants; `ainb-tui/crates/ainb-app/src/wire/store.rs` is the first and the specification.
  - `ui/src/selectors.ts:17` is `ROOT_SELECTORS`, every entry a scalar: `hostCount`, `askCount`, `approveCount`, `waitCount`, `errCount`, `idleCount`, `sessionsStale`, `workspacesLoading`. A new board or banner count joins this object and nothing else.
  - `ui/src/sessions.ts`: `ATTENTION_ORDER` (`:16`), `BLOCKING` (`:19`), `rowStatus` (`:23`), `ringFor` (`:44`, reading each row's framed `attention` since #1131, tightest kind first), `LABEL_CHARS` = 80 and `label` (`:54`, `:61`, which drops control and format characters), `allSessions` (`:68`), `passesFilter` (`:77`, the TypeScript copy of the reducer's filter that #1157 exists to delete), `visibleRows` (`:102`), `ringCount` (`:113`), `idleCount` (`:118`).
  - `ui/src/sidebar.tsx:22` is `Sidebar`, `ui/src/tabs.ts:36` the hand-written `RendererIntent` mirror #1158 exists to generate, `:45` `openRowIntent`, `:82` `accelerator` (the shell chords: cmd on macOS, ctrl+shift elsewhere), `:111` `escEsc`, `:121` `stepTab`. `ui/src/palette.ts:60` `score`, `:78` `commandRows`, `:91` `sessionRows`, `:110` `rank`. `ui/src/terminal.tsx:26` `TerminalView`. `ui/src/transport.ts:7` `TerminalTransport` with `:34` `TAURI` as an injectable bridge.
  - `tests/host_contract.rs` (371 lines) is the headless contract: the first batch frames exactly the subscription, an unmoved section frames nothing, a config write is framed before its persistence effect runs, reframe, an injected config writes nothing under `HOME`, the session list context is active after `open_sessions` and not before, the merge moves no session row, the cadence holds on the next tick, and every palette row survives the dispatch seam. `tests/sidecar.rs` (386), `tests/terminal.rs` (529, against a real tmux on its own `-S` socket in its own directory), `tests/shell.rs` (62, 500 ticks against 200 dispatches), `tests/intent.rs` (46), `tests/workspace_load.rs` (97), `tests/support/mod.rs` (16, one isolated HOME per test binary).
  - `e2e/world.js` builds the journey's world: `APP_BIN` (`:24`), `AINB_BIN` (`:27`), `DAEMON_BIN` (`:34`), `WORLD_ENV` (`:63`, one world per launcher handed to the workers through one environment variable, which is #1160), `up` (`:102`), `seed` (`:146`), `seeded` (`:163`), `down` (`:168`). `e2e/specs/journey.e2e.js:31` is the one spec: the sidebar leg, the terminal leg with `data-painted` (`:29`), the palette leg on `MOD` (`:24`), and the CLI-created session arriving without a restart. `e2e/wdio.conf.js` runs at `maxInstances: 1`.

  The renderer contract, unchanged by this node.
  - `ainb-tui/crates/ainb-app/src/lib.rs:43` re-exports `AppState`, `Btn`, `Chord`, `CommandId`, `Effect`, `Intent`, `Key`, `Keymap`, `Mods`, `Pos`, `SectionId`, `Versioned`, `dispatch`.
  - `src/app/intent.rs:56` is `dispatch(&mut AppState, &Keymap, &mut dyn RendererHost, Intent) -> Vec<Effect>`, `#[must_use]`. `src/app/events.rs:30` is `trait RendererHost`. `src/app/effect.rs:23` is the `Effect` enum, every variant already documenting its desktop half.
  - `src/wire/frame.rs:88` `Frame`, `:21` `HostId`, `:62` `DaemonRead`, `:149` `MAX_FRAME_BYTES` at 4 MiB, `:163` `FrameBatch` with its `oversize` list, `:213` `Subscription` wire-encoded as section names so an unknown name is skipped, `:273` `Mirror` with `new`, `resubscribe`, `reframe`, `set_host` and `batch`.
  - `src/wire/mod.rs:52` `section_json` and `:136` `serialize_section` are the only path from state to JSON and the redaction lives below them; `:62` `daemon_read`; `:102` `section_name`, twenty sections and no more (the 21st was refused in #1076, programme `:131`).
  - `src/wire/fields.rs` is the scrubber set every new framed field must pick from: `in_frame` (`:50`), `omit_in_frame` (`:58`), `omit_outside_frame` (`:70`), `attention_marks` (`:76`), `scrub_in_frame` (`:84`), `scrub_opt_in_frame` (`:93`), `scrub_vec_in_frame` (`:104`), `env_values_in_frame` (`:117`), `char_count` (`:137`), `opt_char_count` (`:142`), `is_some` (`:153`), `len_of` (`:158`), `withheld` (`:163`), `scrub_json` (`:185`), `scrub_editor` (`:205`), `secret_source` (`:215`), `scrub_receipts` (`:228`), `locked_shared` (`:242`).
  - `src/wire/store.rs:50` `Scalar`, `:59` `RootSelector`, `:78` `MirrorStore`, `:182` the rule that the channel peer and never the frame chooses the key.
  - The gates: `tests/state_serde.rs:44` every section has one object frame, `:60` a mirror frame's body is `section_json` byte for byte, `:83` no frame carries a session's cwd, label or pending request, `:107` the leaf key paths match the committed fixture (regenerated with `UPDATE_SECTION_KEY_PATHS=1` at `:111`), `:141` `DENY_WORDS`, `:238` the allow-list of deny-word keys that still carry text with a reason each, `:631` no deny-listed key carries text unless allow-listed. `tests/fixtures/section_key_paths.txt` is 966 lines. `tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt` fails on `Serialize` for `AppState` or any section and on any new serialisation call site. `tests/bindings.rs` and the CI job "TypeScript bindings freshness" diff `bindings/AppState.ts`, regenerated with `UPDATE_APP_STATE_TS=1`.

  What the board, the attention list and the answer can already read off a frame. These are the exact key paths, from `tests/fixtures/section_key_paths.txt`.
  - The board's cards: `agent_status.view.cards[]` (fixture `:6-23`) carries `state`, `tier`, `lifecycle`, `management`, `provider`, `session_key`, `host_id`, `wait_kind`, `has_open_request`, `turn_complete`, `transport_health`, `provenance`, `attachment`, `evidence_observed_at`, `pane_unbound` and `pane_unbound_detail`; `agent_status.absent` (`:5`), `agent_status.view.health.{kind,reason,stale_since_ms,head_revision,read_revision}` (`:22-27`). This is D14's six-tier truth as section 20 publishes it, and it is what a column of cards is grouped by.
  - The board's second source: `fleet.fleet_snapshot[]` (fixture `:265-305`) carries `session_key`, `provider`, `provider_session_id`, `model`, `reasoning_effort`, `lifecycle`, `attention`, `attention_updated_at`, `active_work_count`, `confidence`, `transport_health`, `tmux_target`, `host_id` and the nineteen `capabilities.*` flags including `structured_answer` and `verified_picker`.
  - The attention list: `fleet.daemon_attention` (fixture `:235-257`) carries `all{}` and `by_session_id{}[]`, each row with `kind`, `detail`, `since_ms`, `source`, `options[].label`, `options[].description` and `answerable` in its three variants (`Daemon.attention_id`, `Broker.session_id`, `No`), plus `reachable`, `not_running` and `error`. There is no `by_cwd` index on the wire, deliberately: #1131 removed the renderer's cwd correlation in favour of the host's own merge. `fleet.attention_elsewhere` (`:219`) is the count of daemon rows that matched no row on screen.
  - The per-row ring: `sessions.workspaces[].sessions[].attention[].{kind,detail}` (fixture `:699-700`), set by `AppState::refresh_attention` since #1131 and read by `ui/src/sessions.ts:44`. Kind and scrubbed detail only: no options and no `answerable` ride a session row, which is why the answer box reads `fleet.daemon_attention.by_session_id` for the request it is answering.
  - The answer composer's own state: `fleet.ask_state` (fixture `:209-217`) carries `request`, `focus`, `cursor`, `free_text_len` and `phases[][]` with `Delivered.via`, `InFlight.draft_len` and `Failed.{reason,draft_len}`. The typed text itself is a length, never text, per `fields.rs:137`. So a renderer draws a caret and a masked run from the frame and keeps its own echo of what the person typed, exactly as the TUI does.
  - The screen's own flow state: `shell.session_tab` (fixture `:783`) and `shell.focused_pane` (`:757`), on `ShellView` at `wire/mod.rs:645`, `:657`.

  The answer path that already exists and that D2 must ride rather than rebuild.
  - `src/fleet/answer.rs:93` is `AskState`, the answer machine: `AskFocus` (`:28`), `AnswerPhase` (`:38`), `request_id` (`:150`), `retarget` (`:165`), `push_char` (`:307`), `backspace` (`:312`), `answer_text` (`:321`), `tick` (`:344`) and `send` (`:380`). Its header states the split: the state machine is testable without a socket or a pane, and the worker does the IO.
  - `send` (`:380`) refuses a second send while one is in flight (`:389`), refuses an unanswerable chip (`:392`), resolves the text (`:395`), captures the request identity before the worker runs (`:402`) and spawns the `ainb-ask-send` thread (`:408`) which picks one of three routes by `Answerable`: `answer_via_daemon_blocking` (`:411`), `answer_via_tmux_blocking` (`:414`) or `answer_via_broker_blocking` (`:426`, `:429`) matched against `APPROVE_LABEL` and `DENY_LABEL` and refusing anything else rather than guessing. The outcome lands in the shared inbox (`:442`) as `Delivered { via }` or `Failed { reason }`.
  - `src/fleet/control.rs:610` is `answer_via_daemon_blocking`, the one verified send path: it calls the daemon's `attention/answer` with `AnswerParams`, and its header says plainly that the daemon runs first-answer-wins and performs its own verified last-mile send, so this reports what the daemon decided rather than deciding anything. `AnswerResult::Delivered { via }` and `AlreadyAnswered { by }` both read as delivered (`:632`, `:637`); `Ambiguous`, `NoTarget` and `DeliveryFailed` each carry their own reason and put the chip back (`:642-651`). `src/fleet/attention.rs:654` is `route_answer`, the four-outcome table that decides which of the three routes a chip takes.
  - `src/app/events.rs:3235` is `AppEvent::SessionAskSend`: it takes the selected blocking chip (`:3236`), the row's tmux name and worktree as the identity and cwd the verified send correlates on (`:3245-3253`), retargets (`:3254`) and sends (`:3255`), showing a refusal rather than swallowing it (`:3259`).
  - The keys that reach it: `src/app/keymap_defaults.rs:1233-1238` binds the `session_list.ask` sub-context, so the command ids are `session_list.ask.enter` (`SessionAskSend`), `session_list.ask.previous`, `session_list.ask.next` and `session_list.ask.backspace`. `src/app/keymap.rs:421` maps the context name; `:834-840` pushes the text context when `ask_state.focus()` is `FreeText`, which is what makes `Intent::Text` reach the composer. `src/components/session_tabs.rs:66` is `SessionTab`, `:285` `selected_blocking`, `:333` `cycle`, `:357` `resolve`, all in `ainb-app` and callable from any host.
  - The daemon's half, which D2 does not touch: base spec `:285` records that a double answer is already guarded at `answer.rs:71,116,160` with `AlreadyAnswered { by }` to the loser and a compensating revert; `:307` is the hazard row, "every surface folds `AttentionAnswered` as authoritative and disables its pending form for that id; `answered_by = "<kind>@<host>"`; toast names the winner". S-C landed the fold for the TUI and the web (programme `:121`), and #1075 stamped a TUI answer as `tui@host` (programme `:152`).

  The ACP card's conversation, which is the one thing D2 needs that no frame carries.
  - `src/fleet/chat_host.rs:62` is `ChatHost`, one conversation plus its in-flight effects: `pal` (`:71`), `thread` (`:77`), `topic` (`:94`), `state` (`:100`), `tick` (`:119`), `dispatch` (`:151`), `ChatOutcome` (`:22`). Its header states why it is its own module: two surfaces drive the same conversation and a second copy of "spawn a worker, page the daemon, fold the answer back" is how two chat surfaces drift apart.
  - Both handles live in `HostOnlyState`: `pal_chat` at `src/app/sections.rs:1099` and `session_chat` at `:1119`. `HostOnlyState` starts at `:1011` and its own doc says it is deliberately not a section, none of it crosses to another process, writing it bumps no version and no frame carries it. It never derives `Serialize` and a probe test in `tests/host_side_effects.rs` proves it.
  - `AppState::open_session_chat` and its sibling are at `src/app/state.rs:11779` and `:11828`, building the `ChatHost` lazily on the tab's first render (`:11788`, `:11802`).
  - So the conversation is real, it is already driven off the UI thread, and it is invisible to the webview. Getting it onto the desktop is the one new wire field this node adds, and "THE FOUR SEAMS" below says how, without moving anything out of `HostOnlyState`.

· Locked decisions, as the specs state them. A node PR does not reopen one; a change needs a spec amendment PR first ("Rules of the road", programme `:195`).
  - D10 tmux (multi-surface `:53`): hybrid. tmux stays PTY owner, multi-attach substrate and truth; the daemon adds a headless VT emulator as a cache for snapshot, status and per-viewer flow control; the feed is tmux control mode.
  - D11 host model (`:54`): peer daemon on the box, `HostId` on every wire type and schema row, `SessionRef { host_id, session_key }` on every cross-host surface with `answered_by` named among them, `ssh -L` one carrier among LAN and tailnet. D4's host switcher and transport moved into R1.
  - D12 relay (`:55`): none now. The pairing offer carries an optional `relay` field from day one.
  - D13 off-box transport and auth (`:56`): WebSocket carrying today's JSON-RPC envelope, Noise IK, host static key pinned in the pairing offer, single-use invite redeemed inside the Noise session, per-device revocable tokens, scope allowlist at dispatch and at subscribe.
  - D14 agent status (`:57`): six tiers, hook push highest and pane text lowest. Only tiers 0 and 1 open a turn or assert needs-input. Silence is `unverifiable` or `idle`, never `done`. The store is the `fleet_session` and `fleet_event` family with one writer, and `attention` rows are a projection written by the same apply path in the same transaction.
  - D15 renderer contract (`:58`): Plan B with four invariants. Frames name changed sections. One store transaction per channel drain, effects after commit. Root selectors return scalars, never lists or objects. Renderers may subscribe to a section subset. Specta sits behind the one workspace feature `typescript-bindings` with a CI freshness diff.
  - D16 mobile (`:59`): Expo and React Native, xterm in a webview, thin RPC client, interactive terminal in v1 behind a separate `mobile+type` pairing scope.
  - D17 wire versioning (`:60`): one integer `PROTOCOL_VERSION` in `auth/hello` as `{min, max}`, capability strings negotiated both ways, handshake-negotiated opcodes with permanent numbers, a two-direction skew harness including the local leg.
  - D18 mutations (`:61`): a client-minted opaque 128-bit op id plus a mutation-specific fence on every mutation; the daemon commits then replies; receipts written in the same SQLite transaction as the state flip.

· What D2 must NOT do:
  - No second state machine, and no second answer machine above all. `AskState::send` (`answer.rs:380`) is the only send, and `answer_via_daemon_blocking` (`control.rs:610`) the only verified path. The desktop dispatches the command; it does not call the daemon's `attention/answer` itself, and it does not reimplement first-answer-wins, the in-flight latch or the three routes.
  - No raw daemon reads for anything a section already carries. The board reads `agent_status` and Fleet frames. The desktop's only direct daemon traffic stays the presence lease, the sidecar handshake and nothing else; the terminal is a local PTY per base spec `:206`.
  - No `Serialize` on a section or on `AppState`, and no serialisation call site outside `wire/`: `tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt` stays green.
  - No new wire field without the key-path fixture and the bindings regenerated in the same PR: `tests/fixtures/section_key_paths.txt`, the fixture test at `tests/state_serde.rs:107`, and `bindings/AppState.ts`. D2 adds framed fields on purpose, so this is a step in the plan rather than a prohibition: every one is triaged against the deny-lists at `state_serde.rs:141` and `:238` in the commit that adds it.
  - No conversation text on the wire unscrubbed. A message, a thought, a tool call's input and a permission request's detail are all captured text, so each picks a scrubber from `wire/fields.rs` and the deny-list test proves the choice. A field nothing can prove safe carries a count (`len_of`, `char_count`) or is withheld.
  - No plugin `ui.state` on a frame. `wire/mod.rs:610-613` states the reason in the tree: each view is JSON its plugin wrote, with keys no key-path check can know in advance, so nothing proves it free of a secret. `watched_plugin_screens` stays out for the same class of reason. This is why the board cannot be the hangar plugin's board, and why D2's first act is a spec amendment (below).
  - No 21st section. #1076 closed #1052 with "no 21st section" (programme `:131`); a new framed field joins an existing section's view.
  - No `cmd+` chord through `Chord::parse`, which rejects `cmd`, `command`, `super` and `meta` outright (`src/app/keymap.rs:149`) because surface-safety `:58` reserves that prefix for this renderer. A desktop accelerator resolves in the shell and enters the reducer as `Intent::Command(CommandId, Args)`.
  - No host-only state on a frame. `HostOnlyState` never derives `Serialize`, and D2 does not move `pal_chat` or `session_chat` out of it.
  - No new host-effect crate reachable from `ainb-app`: the `REACHABLE_TODAY` fence in `tests/host_side_effects.rs` never grows.
  - No edits to `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule (programme `:163`).
  - No command that bypasses the gate. `Intent::Command` resolves through the overlay precedence P5d landed (`tests/command_gate.rs`), and the webview seam refuses the host-authored and key-only families (`src/intent.rs:29`). A board card click under a dialog changes nothing, exactly as in the TUI.
  - No PTY in the renderer, no second reconnect implementation, and no change to D1c's credit window or tab cap.

· What D2 draws, from the screen inventory at base spec `:208-227` and the surface table at `:192-204`.
  - The board tab: columns of agent cards from `agent_status.view.cards[]` grouped by `state`, each card carrying its `provider`, `model`, `lifecycle`, `wait_kind`, `transport_health` and the row's own attention, with `has_open_request` as what floats a card to the top of its column. Base spec `:196`.
  - The attention list and the banner: the header counts D1b landed grow a list the counts are a summary of, drawn from `fleet.daemon_attention.by_session_id` plus each session row's framed `attention`, with `fleet.attention_elsewhere` shown rather than swallowed because the sessions surface is the one attention surface (`attention.rs:624-637`). Base spec `:215`.
  - The answer box: the request, its options, a composer, and what the last send did, from `fleet.ask_state` and the chip it names. Base spec `:197`, and the happy path at `:231-239`.
  - The ACP chat card: the selected session's conversation as message, thought, tool call, plan and permission chunks. Base spec `:200`, and the zero-tmux leg at `:264`: an ACP session with no tmux gets no terminal tab and the chat card is the session detail, which is the case D1c left as "no tab and no error".
  - Not in D2: the review tab, the inbox, the stats and burndown tab, the plugin fallback cell, the settings page, the daemons panel, the new session form and the modals. Those are D3 and their rows stay planned. The board's stat strip and turn timeline are a named deferral, below.

· Edge cases D2 must handle, quoted from base spec `:250-265` and `:332-343`. Each one needs a test or a named deferral on its PR.
  - The same ASK answered twice: the loser's card refreshes with the winning answer and a toast names the winner, no new RPC (`:307`, `:339`). On the desktop that is the `AlreadyAnswered { by }` arm of `control.rs:637` reaching the composer as delivered with a note.
  - Both TUI and desktop drawing the same flow: last input wins, accepted, the same as two TUI clients today (`:260`). The answer composer is flow state on the Fleet section, so both surfaces show the same draft length and the same phase.
  - An ACP session with no tmux: no terminal tab, and the chat card is the session detail (`:264`).
  - The daemon socket vanishing (`:255`): the banner and the stale badge D1 landed already cover the board, because staleness is keyed by (host, section) in `ui/src/store.ts`. What D2 adds is the answer composer refusing a send while the daemon is gone, which `route_answer` already decides as `Unanswerable::DaemonGone` (`attention.rs:669`), so the box shows the sentence rather than a dead button.
  - An answer whose send failed: the chip goes back to ASK and the typed draft is restored, which `AskState::tick` does at `answer.rs:355-369`. On the desktop that only happens once the fold runs on the host's tick, which is seam 1 below.
  - Channel backpressure (`:265`): unchanged, `Mirror` coalesces to the current version or nothing.
  - A conversation larger than the frame ceiling: `MAX_FRAME_BYTES` is 4 MiB (`frame.rs:149`) and an oversize section lands in the batch's `oversize` list and stays stale until framed again. The ACP projection therefore carries a bounded window of chunks, and the bound is stated on the PR with the measured size of a real conversation.

· Working dir: the Orca worktree the lane runs in, `~/orca/workspaces/agents-in-a-box/p1-ainb-app` on claude-hetzner, on a fresh branch off `origin/v2` at `ccfa4afd2`.
  - This box shares RAM and disk with other sessions: build with `-j 4` and `CARGO_INCREMENTAL=0`, one test invocation at a time, push after every commit. Disk ceiling is 92 percent; `cargo clean -p` inside `target-desktop` is allowed when it nears that, and nothing else is deleted.

· Audience: D3 (review, inbox, settings, burndown, the plugin fallback cell), which extends this board and inherits the two deferred carries; R1, which replaces the single local host with a real host set and needs `answered_by` to carry a host; the security reviewer, who checks that no conversation text and no plugin-authored JSON reaches the webview outside the scrubbers this node names; P6's concurrency gate, which will start every surface against one daemon and race the answer this node adds; and Stevie.

─ WHY THE BOARD IS NOT THE PLUGIN'S BOARD ─

The spec's D2 row says "board + attention + answer + ACP card; hangar `ui.state` component" (`:116`), and the screen inventory maps "hangar board (plugin)" to "board tab" via "plugin `ui.state` + desktop component" (`:216`). On `v2` that path does not exist and cannot be opened without reopening a locked rule.

```
┌──────────────────────────┐   ┌─────────────────────────────┐
│ hangar plugin ui.state   │──▶│ wire/mod.rs:610  NOT FRAMED │ ✗
│ JSON the plugin wrote    │   │ "no key-path check can know"│
└──────────────────────────┘   └─────────────────────────────┘
┌──────────────────────────┐   ┌─────────────────────────────┐
│ agent_status section 20   │──▶│ view.cards[] state · tier  │ ✓
│ + fleet.fleet_snapshot   │   │ lifecycle · wait_kind · att │
└──────────────────────────┘   └─────────────────────────────┘
```

Three facts, each with a citation:
· `PluginsHostSection`'s `plugin_ui_states` is deliberately not on the wire. `wire/mod.rs:610-613`: "each view is JSON its plugin wrote, with keys no key-path check can know in advance, so nothing proves it free of a secret". Framing it would break the key-path fixture's whole premise and the deny-list test at `state_serde.rs:631`.
· The desktop runs no plugin runtime at all. `executor.rs:29` is `NO_PLUGIN_RUNTIME`, and `ForwardToPlugin` and `RunPluginAction` are answered with `plugin_input_undelivered` and `plugin_action_undelivered` (`:121`, `:134`). `tests/host_side_effects.rs` proves no module in `ainb-app` owns the runtime (`:510`). A plugin `ui.state` read is not a renderer change; it is a second plugin host.
· The data the board draws is already framed, and by a better source. D14 made `agent_status` the one truth across every surface (multi-surface `:57`), T0-section landed it as section 20 (programme `:130`), and `agent_status.view.cards[]` carries exactly the card the board wants. Drawing the board from the plugin's render of that same data would be a second projection of one truth, which is the drift D14 exists to stop.

So D2's first PR is a one-paragraph spec amendment, in the shape D1a's #1111 took: the desktop board draws from the `agent_status` and Fleet frames, the hangar plugin's `ui.state` component moves to D3 beside the plugin fallback cell it belongs with, and the screen inventory row for the board says which frame feeds it. Nothing else in the D2 row changes.

Named deferral in the same amendment, because the spec's interface line promises it and no section carries it: the board's stat strip and turn timeline ("LAST REPLY · 12k tok · +12/-3 · 4 tools · 3m · timeline", base spec `:166-167`, `:196`) need token counts, diff stats, tool counts and a per-turn timeline. No frame carries any of them: `fleet.fleet_snapshot[]` has `active_work_count` and `confidence` and nothing else numeric, `agent_status.view.cards[]` has no counters, and the web's cost projection was deliberately kept off the sections (#1113, #1055, programme `:160`). Adding them is a new daemon read and a new framed family, not a component. D2 draws the columns and the cards; the strip and the timeline are filed for D3 with the measurement of what a read would cost.

─ THE FOUR SEAMS D2 HAS TO OPEN ─

D1 left the desktop able to read every section it draws and to send every intent it needs. Four things the answer journey depends on are still TUI-only, and each is a reducer-side fix rather than a desktop workaround.

```
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 1 ask_state.tick() runs only   │──▶│ move the fold into the reducer's │
│   in ainb-core layout.rs:522   │   │ own tick, every host gets it     │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 2 answered_by hardcoded "tui"  │──▶│ the surface names itself;        │
│   control.rs:625               │   │ desktop@<host> per spec :307     │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 3 no pointer row selects a tab │──▶│ session_list.select_tab, beside  │
│   pointer.rs:21-45 has none    │   │ home.click_sidebar_item          │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 4 the conversation is host-only│──▶│ the reducer projects a bounded,  │
│   sections.rs:1099, :1119      │   │ scrubbed window onto Fleet       │
└────────────────────────────────┘   └──────────────────────────────────┘
```

**Seam 1, the answer outcome never lands on the desktop.** `AskState::tick` (`answer.rs:344`) is what folds the worker's `Delivered` or `Failed` back into the section, restores a failed draft and clears the `SENT` chip. It is called from exactly one place in the tree: `ainb-tui/crates/ainb-core/src/components/layout.rs:522`, inside the TUI's own frame, whose comment says "EVERY frame, not only on the `ask` tab" for precisely the reason D2 hits. `DesktopHost::tick` (`host.rs:161`) does not call it, so on the desktop today an answer would go out and the card would read SENT forever. Two more lines in that same block belong to the same class: the tab reconcile at `layout.rs:515-516` (`session_tabs::resolve`, which a dead tab needs) and the retarget at `:541` (which points the composer at the request it is showing before the first key press).

The fix is the one #1131 already proved: the reducer's own tick owns the cadence, and every host gets it. `AppState::refresh_attention` was made a reducer method called from every host's tick for exactly this reason (`host.rs:182`, and the D1 log's #1131 entry). D2 does the same for the answer fold: a reducer method that ticks the answer machine, reconciles the tab and retargets the composer, called from `DesktopHost::tick` and from the TUI's frame, with the TUI's three call sites in `layout.rs` collapsing into it. `layout.rs` is a renderer file, not `ainb-core/src/app/*`, so the lane rule is not touched. A test in `tests/host_contract.rs` proves a desktop host that never draws still lands an answer outcome.

**Seam 2, `answered_by` is hardcoded.** `control.rs:625` sends `answered_by: "tui".to_string()` from every surface that calls it. Base spec `:307` requires `answered_by = "<kind>@<host>"` and names the toast that reads the winner; D11 (multi-surface `:54`) lists `answered_by` among the cross-host surfaces that must carry a `SessionRef`; #1075 stamped the TUI as `tui@host` on the daemon side (programme `:152`). A desktop answer stamped `tui` is a lie the concurrency gate will read back. The fix is that the surface names itself: the kind reaches `answer_via_daemon_blocking` as an argument the caller supplies, the TUI supplies `tui` and the desktop `desktop`, and the daemon keeps stamping the host. The test is the journey's own assertion that a second surface reads the winner as the desktop.

**Seam 3, nothing clicks a tab.** The desktop reaches the session list through `home.click_sidebar_item` twice (`host.rs:223`), and `pointer.rs:21-45` lists every pointer row there is: `session_list.select_row`, `open_row_menu`, `focus_pane`, `save_pane_layout`, the four skill-manager rows, `home.save_sidebar_width`, `home.click_sidebar_item`, `git_view.select_review_row` and `git_view.scroll`. There is no way to name a session tab. The reducer moves the tab only by cycling (`events.rs:3221` through `session_tabs::cycle`), which a mouse cannot express and a board card certainly cannot. So D2 adds one pointer row, `session_list.select_tab`, carrying a `SessionTab` the same way `select_row` carries a `SessionListRowId`, going in `pointer.rs::ids::ALL` so `refused_from_webview` keeps it out of the palette (a palette row with no payload cannot run it, `intent.rs:43`) while a click can send it. That is the seam the board's card and the attention list's row both use to open the answer box.

**Seam 4, the conversation is host-only.** `pal_chat` (`sections.rs:1099`) and `session_chat` (`:1119`) sit in `HostOnlyState`, which is not a section by design and which D2 does not move. The projection is the pattern #1131 used for attention: the reducer writes a bounded, scrubbed window of the open conversation into a framed field on its tick, read from the `ChatHost` it already holds, and the handle stays exactly where it is.

Recommended placement, to answer on the PR body: the field rides `FleetSection` and so `FleetView` (`wire/mod.rs:577`), beside `ask_state` and `broadcast`, because the conversation belongs to the same attention-and-answer family and because there is no 21st section to add (#1076, programme `:131`). `ClaudeChatSection` (`sections.rs:82`) is the Docker claude-chat pane, a different surface, and putting an ACP transcript there would mean two unrelated things under one name.

Every chunk field picks a scrubber and says why in the commit: message and thought text through `scrub_in_frame` (`fields.rs:84`), a tool call's arguments through `scrub_vec_in_frame` (`:104`) or `scrub_json` (`:185`), a permission request's detail through `scrub_in_frame`, and anything nothing can prove safe through `len_of` (`:158`) or `withheld` (`:163`). The window is bounded so a long conversation cannot cross `MAX_FRAME_BYTES` (`frame.rs:149`), and the bound is stated with the measured size of a real conversation on the PR.

─ SCOPE, STAGED AS PRs ─

Six PRs. Each is mergeable alone, targets `v2`, and carries its own proof. The amendment goes first for the same reason D1a's #1111 did: the programme's rules of the road (`:195`) require the spec to change before the node that needs it.

**D2·spec, the amendment (docs only).**
· Touches: `docs/plans/2026-09-04-desktop-shared-core-spec.md`.
· Says three things in one paragraph each: the desktop board draws from the `agent_status` and Fleet frames rather than the hangar plugin's `ui.state`, with the three citations above; the hangar `ui.state` component moves from the D2 row to D3, beside the plugin fallback cell; and the board's stat strip and turn timeline are deferred with the reason that no section carries a token count, a diff stat, a tool count or a turn timeline.
· Also records the ACP transcript decision: the conversation reaches the wire as a bounded scrubbed projection written by the reducer, and `HostOnlyState` keeps the `ChatHost` handles.
· Gate: no code, so the docs jobs only. Merged before D2a opens.

**D2a, the four seams.**
· Touches: `ainb-tui/crates/ainb-app/src/` (the reducer tick method, `pointer.rs`, `keymap_defaults.rs`, `control.rs`, `answer.rs`, `wire/mod.rs`, `wire/fields.rs` if a new scrubber is needed), `ainb-tui/crates/ainb-core/src/components/layout.rs` (the three call sites collapsing into the reducer method), `ainb-tui/crates/ainb-desktop/src/host.rs`, `bindings/AppState.ts`, `tests/fixtures/section_key_paths.txt`.
· Seams consumed: `AskState::{tick, retarget, send}`, `session_tabs::{resolve, selected_blocking, cycle}`, `pointer::ids::ALL`, `AppState::refresh_attention` as the precedent for a reducer-paced tick step.
· Builds: seam 1 (the answer fold, the tab reconcile and the retarget as one reducer tick step every host calls), seam 2 (`answered_by` supplied by the surface, `tui` from the TUI and `desktop` from the desktop), seam 3 (`session_list.select_tab` as a pointer row in `ids::ALL`), seam 4's wire half (the bounded scrubbed conversation projection on `FleetView`, its scrubber choices, the fixture and the bindings regenerated in the same commit with the triage reason in the message).
· Also lands the D2-bound carries, each in its own commit: #1155 (a scan keeps the selection by identity, which the journey depends on because a rescan mid-answer would otherwise move the row being answered), #1156 (rescan when the daemon's generation moved, cadence as the floor), #1157 (the frame carries the rows a surface draws, deleting `passesFilter` from `ui/src/sessions.ts:77` and keeping each row's reducer index because the selection is expressed in it), #1158 (`RendererIntent` and `PaletteEntry` generated with the freshness check covering them), #1159 (the desktop drains the queued `AsyncAction::RefreshWorkspaces` so a stopped session reaches the sidebar, with a test that one does).
· Proof: `ainb-app` tests for the reducer tick step (an answer outcome lands with no renderer in the process; a dead tab reconciles; the composer is retargeted before the first key), for the new pointer row through the command gate, and for the conversation projection's scrubbing (a credential-shaped string assembled at runtime does not survive the frame, the way `wire/mod.rs:688` already tests attention). `ainb-desktop/tests/host_contract.rs` for the desktop tick landing an answer, for the selection surviving a rescan, for the stopped session arriving, and for a generation bump starting a scan before the cadence would have.
· Gate: `cargo test -p ainb-app`, `cargo test -p ainb-core`, the tripwires, `cargo test` in the excluded workspace, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`, the CI jobs "TypeScript bindings freshness" and "Contracts".

**D2b, the board and the attention list.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/` and `ainb-tui/crates/ainb-desktop/src/host.rs` only if a new command needs offering.
· Seams consumed: `bindings/AppState.ts` for the types, `ui/src/store.ts` for the frames, `ui/src/selectors.ts` for the scalar roots, `ui/src/sessions.ts` for the ring and label rules.
· Builds: the board tab, columns of cards from `agent_status.view.cards[]` grouped by `state`, each card drawing `provider`, `model`, `lifecycle`, `wait_kind`, `transport_health` and its row's attention, with `has_open_request` floating a card to the top of its column and `agent_status.view.health` drawing absent, stale and unreachable in the body rather than as an empty board; the attention list the header counts summarise, from `fleet.daemon_attention.by_session_id` and each row's framed `attention`, with `fleet.attention_elsewhere` shown as its own line; a card click sending `session_list.select_row` with `open: false` and `session_list.select_tab`.
· Every new count is a scalar memo in `ROOT_SELECTORS` (`ui/src/selectors.ts:17`) with the existing test that every entry is a scalar still passing.
· Proof: `node --test` with type stripping, the pattern `ui/src/store.test.ts` and `sessions.test.ts` already use with no test framework dependency: the columns group by state and not by the renderer's own guess, an absent `agent_status` draws its health rather than an empty board, a card with no matching session row still draws, `attention_elsewhere` is never swallowed, and each new root selector returns a scalar. Each invariant checked by breaking it.
· Gate: `tsc --noEmit --strict` over the desktop sources with the generated bindings, `npm test` in `ui/`, both in the `desktop` job.

**D2c, the answer.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/`, `ainb-tui/crates/ainb-desktop/src/host.rs` and `src/main.rs` for the commands the composer sends.
· Seams consumed: `session_list.ask.enter`, `session_list.ask.previous`, `session_list.ask.next`, `session_list.ask.backspace` (`keymap_defaults.rs:1233-1238`), `Intent::Text` for the free-text composer via the text context at `keymap.rs:834-840`, `session_list.select_tab` from D2a, and `fleet.ask_state` plus `fleet.daemon_attention.by_session_id` from the frame.
· Builds: the attention banner a person answers from without leaving the window, the options list and the free-text composer, the caret and masked run drawn from `free_text_len` with the renderer keeping its own echo, and the four phases from `ask_state.phases`: in flight, delivered with its `via`, failed with its reason and the restored draft, and already answered reading as delivered with the winner named in a toast (base spec `:307`, `:339`). A send while the daemon is gone shows `route_answer`'s own sentence rather than a dead button.
· Named deferral, stated on the PR: a `Broker` route answer (a parked permission request) is offered only as the two labels `APPROVE_LABEL` and `DENY_LABEL`, because `answer.rs:424-435` refuses anything else rather than guessing, and a free-text composer over that route would offer a send that cannot land.
· Named deferral, recorded during review: the two-surface cursor race. Two surfaces on one reducer can interleave a cursor move between the frame a window read and the Enter that reads the cursor, so a pick can land on the option another surface moved to. The in-flight latch stops a double send, not a mis-pick. It is inherent to having no desktop-authored send path, and the window already refuses any sequence when the frame is pointed at a different request.
· Proof: `ui/` tests for the four phases and for the option-versus-free-text focus rule; `ainb-desktop/tests/host_contract.rs` for a full round trip with no window, dispatching the ask commands against a seeded chip and asserting the frame's phase moves through in flight and then delivered.
· Gate: as D2b, plus the journey's answer leg once D2e lands it.

**D2d, the ACP card.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/` and whatever the projection needs on the host side.
· Seams consumed: the conversation projection D2a put on `FleetView`, and `ChatHost::state` behind it.
· Builds: the ACP chat card as the session detail, drawing message, thought, tool call, plan and permission chunks, and standing in for the terminal tab when a session has no tmux (base spec `:264`), which is the case D1c left as no tab and no error.
· Proof: `ui/` tests that each chunk kind draws, that a scrubbed chunk draws its redaction marker rather than an empty card, and that a session with no tmux draws the card where the tab would be; `ainb-desktop/tests/host_contract.rs` that the projection reaches a frame on the tick and that a conversation past the window's bound still frames within `MAX_FRAME_BYTES`.
· Gate: as D2b.

**D2e, the journey, the proof scenario and the programme row.**
· Touches: `ainb-tui/crates/ainb-desktop/e2e/`, `ainb-tui/scripts/proof/run.sh`, `ainb-tui/scripts/proof/scenarios/d2-board.sh`, `.github/workflows/desktop.yml`, `docs/plans/2026-09-12-desktop-programme.md`.
· Adds the wdio answer journey as its own spec file beside `specs/journey.e2e.js`, seeding a real daemon ASK the way `scripts/hangar/run_web_e2e.sh` and `ainb-tui/crates/ainb-web/e2e/tests/ask-answer.spec.ts` do, through the `seed_control_center` example. In one run: the ASK reaches the board and the attention banner, the banner is answered from the window, the daemon's verified last-mile send lands it in the pane (proved by the pane's own `capture-pane`, the way `world.js:89` `paneText` already does), the card reads answered on the desktop, and a separate CLI read of the daemon shows `answered_by` naming the desktop.
· Fixes #1160 in the same PR, because this is the second spec file it names: a world per wdio worker keyed by the worker id, or an explicit refusal to run more than one. Required, not optional, since two specs sharing one HOME, one hangar home and one tmux server would pull the ground from under each other in teardown.
· Adds the proof scenario `d2-board` in the shape of `scenarios/d1-shell.sh`: an `EXPECT` line, a `scenario` function, the `skip` at `lib.sh:78` when `xvfb-run` is missing so a box with no display records why rather than failing, and `d2-board` appended to `ALL_NODES` at `run.sh:71-78`. Its observed lines carry the attention id seeded, the sections the renderer applied (read from the `renderer_applied` telemetry at `main.rs:79`, never the window's pixels), the board columns drawn, the `answered_by` the daemon recorded, and a second surface reading the same answer.
· Wires the journey into `.github/workflows/desktop.yml`: the Linux leg real under `xvfb-run` beside the existing `:162` step, the macOS leg on the recorded substitute declared by `MACOS_E2E_LEG` (`:42`) with the named step at `:191` recording which leg ran and failing if neither reported one.
· Flips the D2 row in the programme doc with the run ids, in this PR (programme `:194`).
· Gate: the three success criteria below, all green.

─ WHICH EXISTING TESTS MUST STAY GREEN ─

Unchanged on every PR of this node, or changed only in their own commit with the reason in the message:
· `ainb-tui/crates/ainb-app/tests/host_side_effects.rs`: `REACHABLE_TODAY`, `CALL_SITES`, `REDUCER_DISK_WRITES`, `HOST_STATE_READS`, the host tmux lookup fence, the `HostOnlyState` not-`Serialize` probe, and `no_module_in_the_crate_owns_the_plugin_runtime` (`:510`).
· `ainb-tui/crates/ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
· `ainb-tui/crates/ainb-app/tests/state_serde.rs` with `tests/fixtures/section_key_paths.txt`. D2 regenerates the fixture with `UPDATE_SECTION_KEY_PATHS=1` in the same commit as each new field, after triage against the deny-lists at `:141` and `:238`.
· `ainb-tui/crates/ainb-app/tests/bindings.rs` and the CI job "TypeScript bindings freshness". `AppState.ts` regenerates with `UPDATE_APP_STATE_TS=1` and is committed in the same PR.
· `ainb-tui/crates/ainb-core/tests/mirror_renderer.rs`, the Rust reference renderer, and `ainb-tui/crates/ainb-core/tests/keymap_parity.rs` with `tests/fixtures/keymap_rows.txt`, which the new pointer row regenerates.
· `ainb-tui/crates/ainb-app/tests/command_gate.rs`, `parity.rs`, `renderer_free.rs`, `terminal_host_contract.rs`, `plugin_actions.rs`, `persistence.rs`, `effects.rs`, `intent_dispatch.rs`, `key_only_commands.rs`.
· `ainb-tui/crates/ainb-desktop/tests/`: `host_contract.rs`, `sidecar.rs`, `terminal.rs`, `shell.rs`, `intent.rs`, `workspace_load.rs`.
· `ainb-tui/crates/ainb-desktop/ui/src/*.test.ts`: `store.test.ts`, `sessions.test.ts`, `selectors.test.ts`, `tabs.test.ts`, `palette.test.ts`, `transport.test.ts`.
· CI jobs "Mirror fan-out bench", "TypeScript bindings freshness", "Contracts", "Test (ubuntu-latest)", "Test (macos-latest)", "ainb-core tripwires (ubuntu-latest)", the excluded-set tripwire workflow, and all three jobs of `desktop.yml`.
· The proof harness: a full `bash ainb-tui/scripts/proof/run.sh` still reports every other node passing, nineteen before this node and twenty after.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. **The wdio answer journey is green in CI**, on `ubuntu-latest` under `xvfb-run` with the real leg and on `macos-latest` with the leg `MACOS_E2E_LEG` declares, against a real daemon, a real tmux server and a real seeded ASK. In one run: a three-option ASK seeded into the daemon by a separate process reaches the desktop's board as a card in the blocking column and the attention banner as a row, within the journey's own timeout and with no restart; a person answers it from the window by picking option two, which dispatches `session_list.ask.enter` and nothing else the desktop authored; the answer reaches the agent through the reducer's `AskState::send` (`answer.rs:380`) and `answer_via_daemon_blocking` (`control.rs:610`), proved by the pane's own `capture-pane` carrying the option's label and by the frame's `fleet.ask_state.phases` reading `Delivered`; a separate CLI read of the daemon shows the row answered with `answered_by` naming the desktop and not `tui`; and the card reads answered on the desktop while a second surface started against the same daemon reads the same winner. The job name and run id are recorded on the PR. Nothing in this criterion is measured by a screenshot.

2. **The proof harness scenario `d2-board` passes on a `v2` build.** `bash ainb-tui/scripts/proof/run.sh --only d2-board` writes a `result.json` with `pass: true`, and its `observed` lines carry the attention id the scenario seeded, the section names the renderer applied from the `renderer_applied` telemetry, the board columns and the card count drawn, the `answered_by` the daemon recorded for that row, and the same answer read back by a second surface. `d2-board` is in `ALL_NODES` (`run.sh:71-78`), a box with no headless X server records a `skip` with its reason through `lib.sh:78` rather than a failure, and a full harness run reports every other node passing.

3. **Every wire field D2 added is provably scrubbed and provably in sync.** `tests/fixtures/section_key_paths.txt` names each new leaf and was regenerated in the same commit that added it, with the triage reason in the message; the deny-list tests at `state_serde.rs:631` and the type deny-list pass with each new conversation field either allow-listed with its reason or carrying a scrubber from `wire/fields.rs`; a test proves a credential-shaped string assembled at runtime does not survive the conversation projection, the way `wire/mod.rs:688` already proves it for attention; `tests/serialize_guard.rs`, the `host_side_effects.rs` fences and the `HostOnlyState` probe pass unchanged, so `pal_chat` and `session_chat` did not move; `bindings/AppState.ts` is regenerated and the freshness job is green; `RendererIntent` and `PaletteEntry` are generated rather than hand-written (#1158) and covered by that same job; and `tsc --noEmit --strict` over the desktop sources plus the generated bindings is clean.

4. Final deliverable runs without errors

5. You can show proof (screenshot · test output · URL)

The softer ones, measured and recorded rather than gated:
· The largest conversation the projection framed, in bytes, against `MAX_FRAME_BYTES` at 4 MiB, with the window's bound and the reason for it on the PR.
· How long after the daemon's generation moves a scan starts, with #1156 in, versus the `WORKSPACE_RESCAN` floor without it.
· The number of sections a board-and-answer drain applies, from `renderer_applied`, so D3 knows what a fifth surface costs.

─ CONSTRAINTS ─

· PRs target `v2`, never `main`. Staged as the six above, each reviewable alone.
· Every file change is its own signed commit: `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`. Never `git add -A` and never `git add .`; stage by named path.
· No attribution trailers, no `Co-Authored-By`, and no mention of Claude, an AI or any assistance anywhere in a commit message or a PR body.
· No em dashes on any line you author, in code, docs, commit messages or PR bodies.
· Never name the reference product this programme drew prior art from, in code, comments, docs, commits or PR bodies.
· Open every PR as a draft and flip it ready only when its own gate is green in CI. Never merge your own PR. Message the orchestrator (session `agents-in-a-box-desktop-app-part2-28`) with the head sha when a PR is ready and again when its CI goes green.
· Never poll CI. The orchestrator reads this lane and brings the verdict; keep working on anything that does not depend on the result.
· Locked decisions D10 to D18 are not reopened. A change needs a spec amendment PR first, which is why D2·spec is the first PR of this node.
· Never touch `ainb-tui/crates/ainb-core/src/app/*`, the standing lane rule. Merge `origin/v2` before touching a file another lane is on, and name shared files in the PR body. Never two nodes editing `HelloParams`, `rpc/mod.rs` or `state.rs` at once (programme `:197`): this node edits `state.rs`, so check with the orchestrator before the first commit that touches it.
· The wdio answer journey must run in CI on the PR, with the Linux leg real and the macOS leg the recorded substitute declared by `MACOS_E2E_LEG` in `.github/workflows/desktop.yml:42`.
· The proof scenario `d2-board` is registered in `ALL_NODES` and a missing display is a `skip` through `lib.sh:78`, never a failure.
· Tauri, SolidJS 1.9.15, TypeScript 5.6.3 and Node 22 stay pinned; every frontend dependency is in a committed lockfile and CI installs with `npm ci`, so the build reaches the network for nothing the lockfile does not name.
· The app must build headless in CI on both runners: no interactive Xcode step, no display needed to compile or to run the unit and host-contract tests.
· `cargo clippy --workspace -- -D warnings` and `cargo fmt --all -- --check` in the pre-push gate on every commit, plus `cargo clippy --all-targets --all-features -D warnings` and `cargo fmt --check` inside the excluded workspace.
· The lane runs on claude-hetzner under `~/orca/workspaces/agents-in-a-box/p1-ainb-app`, on a fresh branch off `origin/v2`. Disk ceiling is 92 percent; `cargo clean` of `target-desktop` is allowed when it nears that and nothing else is deleted.
· Never kill a tmux server, never `tmux kill-server`, never a bulk or wildcard kill. Kill a session by exact name only, and let a test's own server exit on its own, the rule D1c's tests already follow.
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

─ WHICH D1 CARRIES THIS NODE TAKES, AND WHICH IT DEFERS ─

| issue | title | D2 | why |
|---|---|---|---|
| #1155 | a workspace scan resets the operator's selection | D2a | the answer journey clicks a row then answers it; a rescan mid-answer would move the row being answered, so the fix is a gate of this node's, not a nicety |
| #1156 | rescan when the daemon reports news | D2a | the attention banner has to appear when the daemon has news, not up to twenty seconds later; the generation the reducer already folds is the trigger |
| #1157 | the frame carries the rows a surface draws | D2a | the board draws rows too, so leaving `passesFilter` in TypeScript would spread one duplicated rule to a second surface |
| #1158 | generate the bindings for `RendererIntent` and `PaletteEntry` | D2a | D2 sends new shapes across the same seam; a hand-written mirror of a shape this node changes is how the window compiles and is wrong |
| #1159 | the desktop never runs the queued full refresh | D2a | the board has a column for stopped rows, and a stopped session does not reach the desktop at all today |
| #1160 | the journey's world is one per launcher | D2e | D2e adds the second spec file the issue names, so two runs would share one HOME, one hangar home and one tmux server |
| #1161 | the reducer owns the palette's rows, for every surface | **D3** | the issue is filed as D3's; the palette D1d landed works and D2 adds no new palette row family, so moving palette ownership would widen this node's diff into a surface it does not change |
| #1162 | say which throughput number each path can prove | **D3** | it is a wording fix on the base spec's terminal gate, owned by the node that closes the parity suite; D2's gate is the answer journey, which measures no throughput |

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

File each as an issue with its evidence. Do not fix it in this node.
· The board's stat strip and turn timeline (base spec `:166-167`, `:196`): no section carries a token count, a diff stat, a tool count or a turn timeline, and the web's cost projection was deliberately kept off the sections (#1113, #1055). File with the measurement of what a daemon read would cost.
· The hangar plugin's `ui.state` desktop component, moved to D3 by this node's amendment: it needs a plugin host the desktop does not have (`executor.rs:29`) and a framed path the redaction rule refuses (`wire/mod.rs:610`). File as D3's, with both citations.
· The presence badge naming the other live surfaces from `hangar/connections_list`, which base spec asks for as "also open in: tui, web" (`:308`) and which no section carries. D2 makes it visible, because an answer race now has a second surface in the picture.
· `AnswerParams.mutation` is `MutationEnvelope::default()` at `control.rs:627`, so an answer carries no op id and no fence, against D18 (multi-surface `:61`). Two surfaces answering the same row rely on first-answer-wins alone. File with the D18 citation; fixing it is a daemon-side change, not D2's.
· The reducer reads the wall clock (`Instant::now()` in the settle and retry rules, in lease renewal and in tick pacing), carried from D1 and P5. Replaying a second host's intents needs time on the intent or a host clock the reducer is given. The answer machine's `AnswerPhase::InFlight { since: Instant }` (`answer.rs:464`) is one more instance.
· The sidebar width key: the TUI stores `sessions_sidebar_fraction` after P5c and whether the desktop shares it or keeps its own in `desktop.json` is still a one-line decision to file rather than pick silently. Carried from D1.
· A frame cache keyed by `(screen, viewport)` so two hosts at different sizes do not share one plugin render, carried from D1 together with #1046.
· Open tabs are not persisted to `desktop.json`, carried from D1c.

─ OPEN QUESTIONS THE LANE ANSWERS ON A PR BODY ─

· Where the conversation projection lives. Recommended: a field on `FleetSection` and `FleetView` (`wire/mod.rs:577`), beside `ask_state` and `broadcast`, because the conversation is part of the attention-and-answer family and there is no 21st section to add. Answer on D2a with the placement and the scrubber per chunk field.
· How many chunks the projection carries, and from which end. Recommended: a bounded window of the most recent chunks, sized from the measured bytes of a real conversation against `MAX_FRAME_BYTES`. Answer on D2a with the number and the measurement.
· How a board card opens the answer box. Recommended: `session_list.select_row` with `open: false` followed by `session_list.select_tab`, so the host writes no state and the context gate decides, exactly as `open_sessions` (`host.rs:223`) does today. Answer on D2b if the lane takes another route.
· What the board subscribes to that D1 does not. D1's list is Sessions, Shell, Tmux, Fleet, Config, `AgentStatus`, `WorkspaceLoad` (`ui/src/main.tsx:32`), which already covers the board, the banner, the answer and the conversation if the projection rides Fleet. Confirm on D2b that the list is unchanged, or name what joined it and why.
· Whether the answer journey is a second spec file or a second `it` in the existing one. Recommended: a second file, because #1160's world-per-worker fix is required either way and two files keep a failing leg readable. Answer on D2e.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (screenshot, test output, URL). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the six PRs:
1. D2·spec: the amendment (the board's source, the `ui.state` component to D3, the stat strip and timeline deferred, the conversation projection recorded).
2. D2a: the four seams (the answer fold in the reducer's tick, `answered_by` from the surface, `session_list.select_tab`, the conversation projection on the wire) plus #1155, #1156, #1157, #1158, #1159.
3. D2b: the board and the attention list in the webview.
4. D2c: the attention banner and the answer box.
5. D2d: the ACP card.
6. D2e: the wdio answer journey with #1160, the `d2-board` proof scenario, the CI wiring, the programme row.
