# Spec: ainb desktop app on a shared core with the TUI

**Generated from:** brainstorm stub (interview 2026-09-04), research/2026-09-04_14-10-02_desktop-app-shared-core.md
**Date:** 2026-09-04
**Format:** diagram-first, table-second, no prose paragraphs
**Research:** research/2026-09-04_14-10-02_desktop-app-shared-core.md
**Interview:** 9 rounds, all forks resolved by Stevie

## Problem

| Question | Answer |
|----------|--------|
| What? | `ainb-desktop`, a Tauri v2 app equal to the TUI, both rendering the same `ainb-app` state machine |
| Why? | Mouse-native control center for many agents, with zero drift from the TUI in data, keys, or behaviour |
| Who? | Stevie and other operators running 5+ coding agents across a laptop and a tailnet box |

## Users + use cases

| Persona | Goal | Primary use case |
|---------|------|------------------|
| Operator at laptop | see every agent, answer what needs input, read diffs | sidebar of sessions, attention ring, answer-from-card, review tab |
| Operator away from box | drive agents on the Hetzner box from the laptop | host switcher over SSH-forwarded socket, WS terminal |
| TUI loyalist | keep using `ainb` in tmux, sometimes open the desktop too | both surfaces on one daemon, no clash |
| Plugin author | show a plugin screen on both surfaces | publish plugin state struct, ship a desktop component |

## Approach

| Option | Summary | Tradeoff | Picked? |
|--------|---------|----------|---------|
| A | typed per-screen ViewModel snapshots | fewer wire fields, every new desktop fact is a Rust change | |
| B | whole `AppState` mirror, sectioned + versioned | every field is API, TS regen breaks loudly, plugins follow same rule | ✓ |
| C | change-event stream, TS mirror store | least traffic, two stores to sync | |

**Why B:** desktop composes cross-screen layouts freely; no field is ever "not in the VM yet"; CI typecheck catches drift in the same PR.

## Architecture

```
┌──────────────────────┐                     ┌───────────────────────────────┐
│ ainb (TUI binary)    │                     │ ainb-desktop (Tauri v2)       │
│ ratatui renderer     │                     │ SolidJS · xterm.js · CM6      │
│ UiState: scroll,Rect │                     │ shell: Vec<HostApp>, tabs     │
│ tty attach handoff   │                     │ WS terminal · Channel<T>      │
└──────────┬───────────┘                     └───────────────┬───────────────┘
           │ Intent in · AppState sections out · Vec<Effect> │
           ▼                                                 ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ ainb-app (new crate, ~41k LOC moved)                                      │
│ AppState { 17 Versioned<Section> }  · AppEvent (intents) · process_event  │
│ keymap: const table + ~/.agents-in-a-box/keymap.toml · CommandId registry │
│ transcript section (ACP) · config section · serde + specta on everything  │
└──────────┬──────────────────┬──────────────────┬──────────────────────────┘
           ▼                  ▼                  ▼
   ainb-hangar-client    fleet-core       plugin-runtime (+ ui.state topic,
   (+ reconnect)                            + plugin/handle_action)
           ▼
┌────────────────────┐   ┌──────────────────────────┐
│ daemon, local      │   │ daemon, remote host       │
│ flock singleton    │   │ ssh -L sock forward       │
│ owns sessions etc. │   │ ainb-web WS behind tunnel │
└────────────────────┘   └──────────────────────────┘
```

| Component | Purpose | Owns |
|-----------|---------|------|
| `ainb-app` | renderer-agnostic state machine | `AppState`, reducer, keymap, command registry, sections, effects |
| `ainb-core` | ratatui renderer + `ainb` binary | draw fns, `UiState` (scroll, Rect, mouse), tty attach, tripwires |
| `ainb-desktop` | Tauri v2 app | shell state (hosts, tabs, layout), Solid components, WS terminal client, sidecar supervisor, in-tree plugin components |
| `ainb-hangar-client` | the one daemon client | dial, hello, reconnect + resync, subscriptions, transcript stream |
| `ainb-hangar-daemon` | control plane | flock singleton, sessions table (moved from sessions.json), attention answers, presence |
| `ainb-web` | browser dashboard | keeps `PtyBridge` + WS framing (reused by desktop); migrates onto `ainb-hangar-client` |
| plugin runtime | plugin host | `ui.state` topic read, `plugin/handle_action` |

## Renderer contract

```
 renderer ──Intent──▶ ainb-app ──sections(v)──▶ renderer
                         │
                         └──Vec<Effect>──▶ host executes
```

| Type | Shape | Producer | Consumers |
|------|-------|----------|-----------|
| `Intent` | `Key(Chord) \| Command(CommandId, Args) \| Mouse(Pos, Btn) \| Text(String)` | renderers | `ainb-app::dispatch` |
| `Chord` | `"ctrl+k"`, `"g g"`, `"cmd+1"`; parsed once, platform-normalised | renderers | keymap |
| `CommandId` | `sessions.attach`, `board.answer`, `palette.open`; registry derived from keymap table | palette, clicks, keymap | reducer |
| `AppState` | 17 `Versioned<T>` sections, `#[serde(skip)]` on mpsc receivers | reducer | ratatui draw, Tauri `Channel<T>` |
| `Versioned<T>` | `{ v: u64, data: T }`, `v` bumped by reducer on any mutation of `data` | reducer | tick sender |
| `Effect` | `AttachTerminal(SessionId) \| Detach \| Notify{title,body,kind} \| OpenEditor(Path) \| Clipboard(String) \| OpenUrl` | reducer | TUI host, Tauri host |
| TS types | generated by `tauri-specta` at build from the same structs | build | Solid components, `tsc` in CI |

- Keymap resolution: renderer sends `Key(Chord)`; core looks up `(ScreenId, Chord)` in merged table (defaults const in `ainb-app/src/keymap.rs`, overrides from `~/.agents-in-a-box/keymap.toml`); result is an `AppEvent`. Palette lists the merged table. `docs/tui/keyboard-shortcuts.md` generated from it.
- Wire tick: after each `dispatch` and each `App::tick`, for every section with `v > last_sent[section]`, send `(section_id, Versioned<T>)` on the host's `Channel`. Full section, no patches.
- Transient UI state split (decided per field, table below).

| Field class | Owner | Examples |
|-------------|-------|----------|
| navigation | renderer | list selection, scroll offset, pane focus, Rect map, hover |
| flow | core section | wizard step + `LaunchSpec`, open modal, filter text, pending answer draft, active screen |
| terminal | renderer host | attached tabs, xterm buffers, cap enforcement |

## Extraction plan

| Step | Moves | Stays | Gate |
|------|-------|-------|------|
| P0 | keymap const table + TOML merge; `Versioned<T>` wrappers on the 17 sections; scroll events deleted from `AppEvent`, `UiState` in `ainb-core` takes `main.rs:676-760` logic; Rect map moves out of `AppState` | everything else | tripwires green, `keyboard-shortcuts.md` regenerated and byte-equal to old prose content list |
| P1 | `state.rs`, `events.rs` (de-crossterm: `KeyEvent` → `Chord` at the host edge), `session_loader`, `snapshot`, `event_bus`, `registry`, `screens::ids`, `EventOutcome` into `ainb-app`; `ainb-core` does `pub use ainb_app::*` | render fns, `screens/builtin.rs`, `attach_handler.rs` | tripwires green, `cargo test -p ainb-app` runs the ~3.5k LOC moved tests |
| P2 | `session_list`, `fleet_panel`, `new_session/*` `*State` structs cut at first render fn; `Effect::AttachTerminal` replaces direct attach | their render fns | sessions tripwires green |
| P3 | hangar plugin host wiring, `daemons` state, `ui.state` topic read, `plugin/handle_action` | | hangar tripwires green, CTS lock regenerated once |
| P4 | `git_view`, `code_review` state | | review tripwires green |
| P5 | `inbox`, `session_recovery`, `skill_manager_screen`, `log_history_viewer`, `config` state | | remaining tripwires green |
| P6 | `ainb-hangar-client` reconnect + resync; `ainb-web` onto the client; sessions.json → daemon table | | web e2e green, TUI + web + CLI concurrent smoke |
| S | surface-safety fixes and `ConnectionRegistry` (see "Concurrency between surfaces"); daemon, notifyd, config only; runs in parallel with P0-P5 | | concurrency tests green |
| D1 | `ainb-desktop` crate: shell, sidecar supervisor, WS terminal, sessions sidebar + tabs, palette | | wdio e2e sessions journey |
| D2 | board + attention + answer + ACP card; hangar `ui.state` component | | wdio e2e answer journey |
| D3 | review tab, inbox, settings, burndown component, plugin fallback cell | | full parity suite |
| D4 | host switcher + ssh forward, updater, release matrix | | release-branch human-driver run |

- Each step is its own PR. `ainb-core` never breaks because it re-exports. D1 can start after P2; D2 after P3; D3 after P5; D4 after P6 + S.
- Hard knots and their resolution: 17 component fields on `AppState` move with P2-P5 in screen order; crossterm in ~130 handler signatures becomes `Chord` in P1; `Rect` hit-test leaves core in P0; `attach_handler.rs` becomes the TUI host's `Effect::AttachTerminal` executor in P2.

## Data model

```
┌─ HostApp ───────────┐ 1:1  ┌─ AppState ───────────────┐
│ host_id, transport  │────▶ │ sessions  Versioned      │
│ connection FSM      │      │ board     Versioned      │
│ ainb-hangar-client  │      │ git       Versioned      │
└─────────┬───────────┘      │ review    Versioned      │
          │ 1:N              │ inbox     Versioned      │
          ▼                  │ transcript Versioned     │
┌─ TerminalTab ───────┐      │ config    Versioned      │
│ session_id, ws_url  │      │ plugins   Versioned      │
│ xterm buffer        │      │ ..17 total               │
└─────────────────────┘      └──────────────────────────┘
┌─ DesktopShell ──────┐      ┌─ keymap.toml ────────────┐
│ hosts: Vec<HostApp> │      │ [screen] cmd = "chord"   │
│ active_host, layout │      └──────────────────────────┘
│ desktop.json        │
└─────────────────────┘
```

| Entity | Fields (key only) | Relationships |
|--------|-------------------|---------------|
| `HostApp` | host_id, label, transport (local socket / ssh forward), state | 1:1 `AppState`, 1:N `TerminalTab` |
| `AppState` section | `v`, `data` | per host |
| `TerminalTab` | session_id, host_id, ws_url, attached_at | N:1 host, cap 8 per host |
| `DesktopShell` | hosts, active_host, sidebar_width, window, open tabs | persisted to `~/.agents-in-a-box/desktop.json` |
| keymap override | `[screen] command = chord` | merged over const table at startup and on file change |
| plugin `ui.state` | plugin's own serde state struct, versioned by snapshot bus | read by desktop via `snapshot_get_versioned` |

## Interface

Desktop shell:

```
┌──────────────────────────────────────────────────────────────┐
│ ⌘K palette │ host: laptop ▾ box │ ● 2 ASK 1 ERR 3 IDLE  [⚙] │
├────────────┬─────────────────────────────────────────────────┤
│ SESSIONS   │ [feat-x ●] [fix-y] [board] [diff: fix-y] [+]   │
│ ▶ feat-x ● │ ┌──────────────────┐ ┌──────────────────┐      │
│   fix-y    │ │ $ claude ...     │ │ $ codex ...      │      │
│   docs-z   │ │                  │ │                  │      │
│ BOARD      │ └──────────────────┘ └──────────────────┘      │
│ REVIEW     │ LAST REPLY · 12k tok · +12/-3 · 4 tools · 3m   │
│ INBOX      │ ▸ timeline: read(2s) edit(1s) bash(14s) ...    │
│ STATS      │                                                 │
└────────────┴─────────────────────────────────────────────────┘
```

Rust contract:

```rust
pub struct AinbApp { state: AppState, keymap: Keymap, .. }
impl AinbApp {
    pub fn dispatch(&mut self, intent: Intent) -> Vec<Effect>;
    pub fn tick(&mut self) -> Vec<Effect>;                 // drains mpsc, bumps versions
    pub fn changed_since(&self, seen: &SectionVersions) -> Vec<SectionDelta>;
    pub fn state(&self) -> &AppState;                      // ratatui draws from this
}
```

Plugin wire (no protocol churn for the topic, one new method):

```
topic "ui.state"        payload = serde of the plugin's state struct, versioned by bus
plugin/handle_key       existing, Chord wire shape from PluginScreen
plugin/handle_action    NEW { action_id: String, payload: Value }   (clicks, palette)
```

| Surface | Trigger | Shape |
|---------|---------|-------|
| sidebar sessions | tick, `sessions` section change | tree grouped by workspace, ring per row |
| terminal tab | `Effect::AttachTerminal` | xterm.js on `ws://127.0.0.1:<port>/ws/session/<id>` (local) or tunnelled |
| board | `board` section change | columns, cards with status line, stat strip, LAST REPLY, timeline |
| answer box | `Command(board.answer)` | reply → daemon answer RPC → card refresh |
| review tab | `Command(review.open)` | CodeMirror 6 merge view, hunks from `ainb-diff`, Shiki highlight |
| inbox | `inbox` section change | list from notifyd SQLite via core section |
| ACP chat card | `transcript` section change | message / thought / tool_call / plan / permission chunks |
| palette | `cmd+k` | fuzzy over merged keymap + sessions + cards + hosts |
| settings | `Command(config.open)` | core `config` section as a form + desktop-only: theme, fonts, layout |
| host switcher | header click or `cmd+shift+h` | swaps active `HostApp` |
| new session | `Command(sessions.new)` | core wizard state (`configure.rs`) as one form page, same `LaunchSpec` |

## Screen inventory

| TUI screen | Desktop screen | Mapping | v1 |
|------------|----------------|---------|----|
| home menu | sidebar nav | merged | yes |
| sessions + tmux preview | sessions sidebar + terminal tabs | 1:1, preview becomes live tab | yes |
| new session wizard | new session form | 1:1 state, one page | yes |
| fleet panel | attention list (header counts) | 1:1 | yes |
| hangar board (plugin) | board tab | plugin `ui.state` + desktop component | yes |
| code review | review tab | 1:1 hunks, CM6 paint | yes |
| inbox | inbox | 1:1 | yes |
| stats / burndown (plugin) | stats tab | plugin `ui.state` + desktop component | yes |
| abtop, witr, learnings, skills (plugins) | WireBuffer painted in an xterm cell | fallback | yes, fallback |
| daemons overlay | daemons panel in settings | 1:1 | yes |
| config | settings page | 1:1 + desktop extras | yes |
| MCP pool overlay | settings panel | 1:1 | yes |
| help overlay | palette + shortcuts page (generated) | merged | yes |
| recovery, onboarding, auth setup, changelog | modals | 1:1 | yes, P5 |
| desktop-only | host switcher, multi-terminal tabs, drag cards, palette | new | yes |
| desktop-only | tiled splits, detachable windows | new | v2 |

## Behavior

Happy path, answer an ASK from the desktop:

```
[ASK arrives via hangar/event] ──reducer──▶ [board.attention v+1] ──tick──▶ [Channel send]
      ──Solid──▶ [card floats to top, ring on row, dock badge +1, OS notify if unfocused]
      ──user cmd+u──▶ [focus card] ──type + enter──▶ [Command(board.answer)]
      ──reducer──▶ [pending_async: answer RPC] ──daemon──▶ [attention answered]
      ──hangar/event──▶ [board v+1] ──▶ [card clears, badge -1]
```

Keyboard focus rules:

```
[sidebar focus] ──enter──▶ [terminal focus] ──esc esc <300ms──▶ [sidebar focus]
     │                          │
     │ all app chords           │ only cmd+k, cmd+1..9, cmd+w, cmd+[ ], cmd+u, cmd+shift+h
     ▼                          ▼ everything else to PTY (ctrl+c, ctrl+b, arrows)
```

Edge cases:

| Scenario | Trigger | Expected behavior |
|----------|---------|-------------------|
| 9th terminal tab opened | cap 8 per host | oldest idle tab detached, toast names it, tab stays listed and re-attaches on click |
| daemon socket vanishes | daemon crash | client reconnect with backoff 1s/4s/16s, banner "reconnecting", sections frozen with stale badge, resync on hello |
| daemon down after 3 spawn retries | sidecar crash loop | degraded banner with "show log" and "retry"; sidebar still lists tmux sessions via fleet discovery |
| host unreachable (ssh) | tailnet down | host chip red, its tabs greyed, other hosts unaffected |
| keymap.toml invalid | bad chord string | ignore file, toast with line number, defaults apply |
| keymap conflict with terminal | user binds `ctrl+c` in TOML | rejected at load: only cmd-prefixed chords allowed while terminal focused |
| both TUI and desktop draw the wizard | wizard is core flow state | both show step; last input wins; acceptable, same as two TUI clients today |
| plugin without desktop component | third-party plugin | WireBuffer painted into an xterm cell, keys forwarded, mouse ignored |
| plugin render wedged | 2s timeout | same watchdog; desktop shows "plugin unresponsive" in the cell |
| Linux, `cmd` absent | Super or Alt | keymap layer: `cmd` → `super`, fallback `alt` if super captured by WM, set in settings |
| ACP session with no tmux | zero-tmux leg | no terminal tab; chat card is the session detail |
| section version wraps or lags | Channel backpressure | send coalesces to latest per section; never queues stale versions |

## Hosts + daemon

```
[unknown] ──probe sock──▶ [hello ok] ─────────────────────▶ [connected]
    │                                                           │ lag
    └──no sock──▶ [flock try] ──won──▶ [spawn sidecar] ──ready──┘ ▼
                       │                    │ crash x3      [resync] ──▶ [connected]
                       └──lost──▶ [wait hello, attach]      ▼
                                                        [degraded] ──retry──▶ [flock try]
```

| Mode | Transport | Auth | Spawn | Reconnect |
|------|-----------|------|-------|-----------|
| local | Unix socket `{hangar_home}/hangar.sock` | `auth/hello` token + `SO_PEERCRED` | attach if hello answers, else flock + spawn `ainb-hangar-daemon` sidecar (bundled, target-triple named); never kill on exit | in `ainb-hangar-client`, shared |
| remote (tailnet) | `ssh -L <local.sock>:<remote hangar.sock>` owned by desktop; terminal via `ainb-web` WS on the box behind the same tunnel | same token (read from box over ssh once), peer_cred sees ssh user | never spawns remotely; "start daemon on box" runs `ssh box ainb hangar daemon start` | same client, tunnel restart on ssh exit |
| multi-host | one `HostApp` per host | per host | per host | per host; attention counts aggregate in shell |

- Daemon singleton already exists: `single_instance::acquire` takes a flock on `<home>/hangar/daemon.lock` before store or bind (`ainb-hangar-daemon/src/lib.rs:762`, `single_instance.rs:75`); loser exits 0. The desktop sidecar supervisor reuses it: spawn, and if the child exits 0 within the grace window, attach to the winner.
- Double answer already guarded: `answer.rs:71,116,160` resolves the target, conditionally claims, and returns `AlreadyAnswered { by }` to the loser with a compensating revert on delivery fault. Desktop shows the toast, no new RPC.
- Durable state: sessions.json, snapshots index, usage cache ownership moves into the daemon (converged invariant 1), sessions first. Surfaces read via RPC and `workspace/subscribe`.
- Surface clash policy: see "Concurrency between surfaces" (pending two independent design passes).

## Concurrency between surfaces

Requirement: TUI, desktop, and web run in any combination against one daemon and never clash. Grounded in a 17-hazard code audit plus two independent design passes (Opus, Codex); they agreed on three of four hazards.

```
┌──────┐  ┌─────────┐  ┌─────┐      answer ──▶ mark_answered_if_open (SQLite, first wins)
│ TUI  │  │ desktop │  │ web │ ──▶  hello{surface,host,pid} ──▶ ConnectionRegistry (in-mem)
└──┬───┘  └────┬────┘  └──┬──┘      AttentionAnswered / ConnectionsChanged ◀── broker
   │ tty       │ WS PTY    │ WS PTY
   ▼           ▼           ▼
 ┌──────────────────────────────┐   window-size latest pinned at create
 │ one tmux session, N clients  │   no -d, no -r, no input lease
 └──────────────────────────────┘
```

| Hazard | Today | Policy | Change |
|--------|-------|--------|--------|
| two daemons | flock `daemon.lock` before bind (`single_instance.rs:75`) | keep | desktop supervisor treats child exit 0 as "attach to winner" |
| same ASK answered twice | `mark_answered_if_open` conditional UPDATE, loser gets `AlreadyAnswered { by }`, compensating revert (`attention.rs:344`, `answer.rs:71,116,160`) | keep, reject-second | client rule: every surface folds `AttentionAnswered` as authoritative and disables its pending form for that id; `answered_by = "<kind>@<host>"`; toast names the winner |
| N clients on one tmux session | no `-d`, no `-r` on any attach; `-x 80 -y 24` at create (`session.rs:151`); nothing pins `window-size` | native tmux, no input lease (a lease is a distributed keyboard lock and breaks "watch on desktop, type in TUI") | pin `tmux set-option window-size latest` in `configure_session` (`session.rs:213`); never `aggressive-resize`; presence badge "also open in: tui, web" |
| resize mid-answer | `deliver_picker` verifies by reading `❯ N.` from `capture_pane` (`answer.rs:558-687`) | test it; reflow could flip `Confirmed` to `Gone` | concurrency test: three clients at different sizes, one resizes during answer |
| plugin subprocess duplication | session-reader cache is WAL + 5s `busy_timeout` + bounded retry, upserts keyed `(path, mtime, size)` (`cache.rs:48-70, 268`); no plugin binds a fixed port | accept; cost is CPU, not data | none |
| who is connected | daemon has uid only (`auth.rs:218`), no connection map | minimal in-memory `ConnectionRegistry` {surface_kind, host, pid, connected_at, attached sessions}; dies with daemon | `auth/hello` gains optional `{surface_kind, host, pid}`; new `hangar/connections_list`; `ConnectionsChanged` on the existing broker; daemon folds `tmux list-clients` per attached session |
| config.toml lost update | read-merge-`write_atomic`, no lock; burndown writes the same file (`config/mod.rs:2029`, `plugin-burndown/src/config.rs:169`) | data loss | `fs2` flock around both writers |
| favorites / ssh names / onboarding JSON | in-place `fs::write`, no rename (`favorites_store.rs:176`, `ssh_display_names.rs:59`, `onboarding.rs:104`) | data loss + torn reads | temp + rename under the shared lock |
| notifyd double-start | `PidFile::write_current` is `File::create`, no lock; listener `remove_file` + bind steals the socket (`pid.rs:29`, `listener.rs:222,378,398`) | data loss (approve map dropped) | real flock before unlink + bind; fix the false doc comment |
| headroom proxy double-spawn | `SPAWN_LOCK` is process-local; fixed port 8787; loser writes `proxy.pid` after failed bind (`headroom/mod.rs:100-155`) | orphaned proxy | cross-process guard; write pid only after health poll |
| MCP pool unlink TOCTOU | probe-then-spawn, `socket_alive_or_cleanup` unlinks (`mcp_pool/proxy.rs:94`) | low | lock held across the bind |
| usage_cache SQLite | WAL, no `busy_timeout` (`usage_cache/db.rs:36`) | warn spam | add 5-10s timeout |
| sessions.json | cross-process flock + temp + rename, best-effort (`session_registry.rs:168`) | low today; Stevie's call: daemon owns it | P6 moves it into the daemon behind RPC |
| hangar / notifyd SQLite | WAL + `busy_timeout`, single writer | none | none |

Phase S (surface safety), independent of extraction, can start immediately:

1. notifyd flock + doc fix.
2. config.toml flock around both writers.
3. headroom cross-process guard + late pid write.
4. `window-size latest` pin in `configure_session`.
5. `AttentionAnswered` folding rule in TUI and web; `answered_by = "<kind>@<host>"`.
6. temp + rename for the three JSON stores; MCP pool lock; usage_cache timeout.
7. `ConnectionRegistry` + hello extension + `hangar/connections_list` + `ConnectionsChanged` + `tmux list-clients` fold.
8. Test: two concurrent answers on one row expect one `Delivered` + one `AlreadyAnswered`; three attached clients, resize during picker answer, expect `Confirmed`.

## Errors

| Failure mode | User-visible surface | Recovery |
|--------------|----------------------|----------|
| daemon hello fails (bad token) | modal with token path and "copy fix command" | user runs `ainb hangar daemon token`, retry |
| WS terminal drops | tab shows "reconnecting" overlay, buffer kept | auto-redial 3x, then "reattach" button |
| tmux session gone | tab closes with toast, sidebar row marked exited | recovery flow from `session_recovery` section |
| answer RPC rejected (already answered) | card refreshes with the winning answer, toast "answered from tui" | none needed |
| specta TS out of date | CI `tsc` failure | regenerate in build, same PR |
| sidecar binary missing (bad bundle) | degraded banner, "install daemon" link | `cargo install` / brew instructions |
| updater signature mismatch | update declined, log entry | manual download |
| plugin crash | cell shows crash + restart button | runtime quarantine rules apply |

## Testing strategy

| Layer | Scope | Coverage gate |
|-------|-------|---------------|
| Core | `ainb-app` behavioural tests: dispatch → state, keymap merge, version bumps, effects emitted; the ~3.5k LOC moved tests plus new per-section tests | must pass, `cargo test -p ainb-app` |
| Parity | one JSON fixture `AppState` per screen → ratatui `TestBackend` text and Solid DOM text → both diffed against an expected-facts list | must pass, lives in `ainb-app/tests/parity` + `ainb-desktop/e2e` |
| E2E | `@wdio/tauri-service` against the real window with a real daemon and real tmux sessions; macOS runner + Linux `xvfb-run`; scripts mirror tripwire scenarios (new session, answer ASK, review diff, plugin screen, host switch) | must pass on PR |
| Throughput | `cat` 50MB through the WS terminal in under 2s with zero dropped bytes, on both runners | must pass on PR |
| Visual | window PNG per screen, vision-model review posted as PR comment | advisory |
| Human driver | peekaboo on macOS + computer-use agent operates the app per screen script, screen-recorded, recording reviewed by vision model | gating on release branch, advisory on feature PRs |
| Concurrency | TUI + desktop + web started in every combination against one daemon; answer race, double spawn, shared file writes | must pass on PR after P6 |
| TUI regression | existing 101 tripwires | must pass on every extraction PR |

## Packaging + CI

| Target | Artifact | Runner | Signing |
|--------|----------|--------|---------|
| macOS universal | `.dmg` with sidecar for both triples | macos-latest | Apple ID + notarize via `tauri-action` |
| Linux x86_64 | AppImage (embeds webkit2gtk 4.1) + `.deb` | ubuntu-22.04 | none |
| Linux arm64 | AppImage | native ARM runner | none |
| updater | `latest.json` + signed bundles on GitHub Releases | release workflow | `tauri signer` key in secrets |
| WSL | Linux AppImage under WSLg, `"icon": []` workaround documented | n/a | n/a |

- Frontend build: vite + SolidJS + `tauri-specta`, run by `beforeBuildCommand`; Node toolchain pinned in repo; `tsc --noEmit` in CI.
- Sidecar: `bundle.externalBin` = `ainb-hangar-daemon`, copied to `src-tauri/binaries/<name>-<triple>` by a workspace `xtask` step.
- Spikes before the CI matrix is locked: universal dmg sidecar pickup; minimum macOS version for Tauri 2 + updater; WS terminal throughput on webkit2gtk with xterm.js WebGL addon; specta generation for a 14k-LOC `AppState` (compile time, TS size).

## Out of scope

- Ticket import (Linear / Jira / GitHub) as session creation.
- v2, not v1: Windows native (MSI, ConPTY, signing), tiled splits and detachable windows, third-party plugin desktop component bundles.
- Swift `apps/ainb-fleet-macos`: untouched.
- Replacing notifyd with the daemon attention table as inbox source (converged D10) is a later migration for both surfaces at once.

## Open questions for /plan

- [ ] Section boundaries: exact list of the 17 `Versioned` sections and which existing `AppState` fields land in each (needs a field-by-field pass of `state.rs:3261-3710`).
- [ ] `Chord` normalisation table across macOS / Linux / terminal (`cmd` vs `super` vs `alt`, `ctrl` chords reserved for PTY while terminal focused).
- [ ] `plugin/handle_action` payload schema and CTS axis; protocol version bump plan.
- [ ] sessions.json → daemon table: migration path for existing `~/.agents-in-a-box/sessions.json` and the CLI commands that read it.
- [ ] ssh tunnel ownership: `ainb-desktop` spawns `ssh` directly vs `ainb-hangar-client` gains a `Transport::SshForward`.
- [ ] `ConnectionRegistry` stale-entry bound: confirm the rpc idle-timeout path clears a SIGKILLed surface's row.
- [ ] Read before P3: Termic `sandbox.rs` / `proxy.rs` / `wdio.conf.ts`, Codexia `web/router.rs`, Jean worktree lifecycle (`research/2026-09-04_16-25-00_tauri-agent-apps-prior-art.md`).
- [ ] Lift Termic's OSC 9;4 "agent finished" detection into the fleet pane-fallback classifier (orthogonal, helps TUI today).
