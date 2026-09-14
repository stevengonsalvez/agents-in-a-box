# /goal P5 closes slice 2: the host owns the terminal client, every reducer write to disk goes through a persistence effect, the runtime handle leaves AppState, the in-place attach has a desktop story, the command gate matches the key gate, and the tripwire ratchet closes both ways, so D1 can start from a host contract a second renderer can implement from the docs alone

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, slice 2 node P2-P5 (`docs/plans/2026-09-12-desktop-programme.md`), last of the four staged PRs under `docs/plans/goals/2026-09-13-p2-p5-effects-and-hosts.md` (P5 section plus the deferrals recorded in P3 and P4). Issue #1035 carries the nine criteria from the P4 design review; items 1 to 4 block D1 (desktop shell). Spec: `docs/plans/2026-09-11-multi-surface-decisions-spec.md` D15 (renderer contract) and the Effect row as amended by P2, P3 and P4.
· Stack: Rust workspace under `ainb-tui/`; `ainb-app` (reducers, `Effect` outbox, report intents, `app/effect.rs` validating newtypes `TmuxSessionName` and `EditorPath`, `app/reports.rs` with `LocalEmbed` and the process-global embed registry, `tmux/embed_client.rs`, `plugins_host.plugin_runtime`, `watched_plugin_screens`), `ainb-core` (`effect_host.rs` executor `execute(effect, terminal, ui, plugins)`, `main.rs` run loop, tripwires under `tests/`), `ainb-plugin-runtime` (snapshot store with `remove`, per-plugin `ui.state/<plugin>` topics), CI `ci.yml` job `ainb-core tripwires (ubuntu-latest)` with `tests/tripwire_ci_exclusions.txt` and `ci_tripwire_exclusions.rs`; `ainb-app/tests/host_side_effects.rs` fence (`REACHABLE_TODAY` still names `portable-pty`).
· Current state: `v2` after P4 (#1021): reducers perform no side effect except the preview PTY the fence still allows; the executor takes no `&AppState`; `ui.state` is per plugin with eviction, a size cap and lease-renewed screen watches; `Intent::Command` is gated by screen context with `Context::Global` split from host-authored report ids; `AppState.tmux.embed` still holds an `EmbedClient` and `LocalEmbed` parks live PTY clients in a process-global map (a second host gets a silent no-op on `in_place_opened`); `app_config.save()` and the other stores (`favorites_store`, `session_label_store`, session meta, onboarding config) are still written from reducer arms (8 in `events.rs`, 6 in `state.rs`); `plugins_host.plugin_runtime` is a runtime handle inside `AppState`; `watch_screen` carries no viewport; the tripwire ratchet is per binary with a ratio SKIP guard; 14 tripwires are red with no verdict (#1023, #1024, #1025, #1027, #1028); `sessions_sidebar_width` is still a column count. Lane C is on #1031 (section 20 single owner, touches `main.rs` and the hangar plugin) and lane K on W0-mirror (`ainb-app/src/wire/*`, frames and subscription); rebase daily and name shared files.
· Working dir: the Orca worktree this session was launched in (`p1-ainb-app` on claude-hetzner), branch `stevengonsalvez/p5-hosts` from `origin/v2` after a `cargo clean` (this box has 150 GB and 15 GB RAM shared with other sessions: build with `-j 4` and `CARGO_INCREMENTAL=0`, one test invocation at a time, push after every commit).
· Constraints: PRs target `v2`, staged as needed (host-owned client and in-place story; persistence effect; runtime handle and viewport; gate parity and tripwire ratchet), one file per commit, GPG-signed with `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`, no attribution trailers, `cargo fmt --all -- --check` in the pre-push gate; no em-dashes on lines you author; never touch `ainb-core/src/app/*`; the fence in `ainb-app/tests/host_side_effects.rs` shrinks (drop `portable-pty` from `REACHABLE_TODAY`) and never grows; no `Serialize` added to any section outside the `wire/` seam and every newly serialised field triaged against the key-path fixture; the executor stays free of `&AppState`; every effect that changes state reports through a named intent; the persistence effect runs after the store commit and a source guard in the `CALL_SITES` style asserts no `.save()` on a reducer-reachable path; parity snapshots byte-identical or changed in their own commit with the reason; keymap golden and CLI reference regenerated in their own commits; gates: `cargo test -p ainb-app --features test-support -p ainb-core -p ainb-plugin-runtime`, `cargo clippy --workspace -- -D warnings`, the sessions and review tripwires green in CI; update the P2-P5 row in the programme doc in the PR that flips it and record every decision in the goal doc.
· Audience: the D1 desktop shell lane, which starts from this contract; W0-mirror (lane K) which carries the frames; the security reviewer who checks the command gate and the client ownership; and Stevie.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. The host owns the terminal client and the in-place attach has a story: `AppState.tmux.embed` holds only `embed_session: Option<TmuxSessionName>` plus focus, the host keeps the `EmbedClient`, `LocalEmbed` and the process-global registry are deleted, `enter_interactive_pane` and `sync_terminal_observer` leave `AppState`, `portable-pty` leaves `REACHABLE_TODAY`, `tripwire_interactive_pane` drives the effect and the report; and either the `in_place_opened` report is portable to a second host (a headless test host adopts and releases it) or the effect doc and the programme doc state in one line that `TerminalTarget::InPlace` is terminal-host-only and name what D1 does instead, with a test that a foreign host gets an explicit `in_place_unsupported` report rather than a silent no-op.
2. Persistence and the runtime handle: every reducer write to disk (`app_config`, `favorites_store`, `session_label_store`, session meta, onboarding config) goes through one persistence effect the host runs after the store commit, with a source guard asserting no `.save()` on a reducer-reachable path and a test that a failed write surfaces as a report and never blocks `dispatch`; `plugins_host.plugin_runtime` moves to the host and `main.rs` passes its own handle (or `effect_host.rs` names the divergence D1 inherits); `watch_screen` carries `{screen, watching, width, height}` with the largest requested size winning across hosts and the tick's fallback size no longer hardcoded; `sessions_sidebar_width` becomes a fraction of its row with the same one-time migration `home_sidebar_width` got.
3. Gate parity and the ratchet: `Intent::Command` resolves through the same precedence the key path uses so an overlay or a confirm dialog blocks a screen-scoped command (test: `git_view.scroll` with a confirmation dialog open changes nothing), and the decision whether `HostFlags` moves into `AppState` or intents carry it is written in the goal doc; the tripwire ratchet is per test, a scheduled job runs only the excluded set and warns when one goes green, the SKIP guard asserts the expected skip set rather than a ratio; each of the 14 red tripwires has a one-sentence verdict (test rot or product bug, with evidence) on its issue; the mirror granularity decision (diff by key path or by section) is written in the goal doc and agreed with lane K; the P2-P5 row flips to done with the run ids.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

— OPERATING RULES — NON-NEGOTIABLE —
1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose + fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components + real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note + keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

— QUALITY BAR —
· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern / env var / decision logged

— FINAL DELIVERABLE —
✅ Confirmation each criterion is satisfied
📂 Every file created / modified
🚀 How to run / test / deploy
📊 Proof (screenshot · test output · URL)
📝 Decisions made + anything to know
⚠️ Known limitations + follow-ups

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.

-- PROGRESS LOG --

Plan, staged as the constraints allow:
1. P5a, criterion 1: the host owns the terminal client and the in-place attach is portable.
2. P5b, criterion 2 part: one persistence effect over every reducer write to disk, a failed-write report, a source guard.
3. P5c, criterion 2 rest: the runtime handle out of `AppState` (or the named divergence), `watch_screen` with a viewport, `sessions_sidebar_width` as a fraction.
4. P5d, criterion 3: command gate precedence and the `HostFlags` decision, the per-test tripwire ratchet with a scheduled excluded-set job and a skip-set guard, a verdict per red tripwire, the mirror granularity decision with lane K, the programme row.

Also taken in P5 from the #1021 fix verification: `TmuxSessionName::new` refuses whitespace at either end (e0bc31076).

P5a, done on `stevengonsalvez/p5-hosts`:
- `TmuxSection` holds `embed_session: Option<TmuxSessionName>` and no client. `LocalEmbed` and its process-global registry are deleted. `enter_interactive_pane` went in P4; `sync_terminal_observer` is replaced by `AppState::request_terminal_observer`, which decides and returns `Effect::AttachTerminal(TerminalTarget::Observe)` without touching a PTY.
- The terminal host keeps the client in `ainb-core/src/terminal_clients.rs` (`TerminalClients`), threaded through `run_intent`, `run_effects` and `effect_host::execute`. `embed_client.rs` and `pty_wrapper.rs` moved to `ainb-core/src/tmux/`; `ainb-app` no longer depends on `portable-pty` or `vt100`, and `portable-pty` left `REACHABLE_TODAY`.
- Reports by session name: `in_place_opened`, `observer_opened`, `observer_failed {unsupported}`, `terminal_exited`, `terminal_input_closed`.
- Evidence: `ainb-app/tests/terminal_host_contract.rs` (a headless host with no PTY attaches in place, releases on detach, on leaving the session list and on a client exit, and mirrors the selection read-only); `tripwire_interactive_pane` (5) through `TerminalClients`; `tripwire_keymap_surface` (real binary, Ctrl+C reaches the embed and Ctrl+Q returns); `state_tests.rs` observer retry rules through reports.

- Host-only fields out of the sections (lane K's list for W0-mirror, #1036): `HostOnlyState` is a non-versioned `AppState.host` holding the tmux session handles and preview task, the observer's settle and retry bookkeeping, the workspace load receiver and pacing timers, the three new-session receivers, the log streaming coordinator, sender, per-session update times and session log handles, and the fleet live-window watcher, Pal chat, Pal dial, daemon start offer, session chat, attention poller flag and generation, and the Headroom and token refresh timers. Writing any of them bumps no section. The only test that pinned a bump on one (`reports.rs`, a tmux section bump when a missing session's handle is dropped) now expects none. The sessions tripwires that drive those paths pass locally: tab strip, Pal engine swap, Pal daemon CTA, log tab, attention chips, keymap surface.

P5b, on `stevengonsalvez/p5b-persist` (stacked on P5a):
- `Effect::Persist(Persist)` replaces every `.save()` in `ainb-app/src/app` and `src/components`: the user config (11 sites), favourites (3, including the repo picker), session labels (3), the onboarding record and its git directories (2), the Claude auth provider (2) and the session store's Headroom flag (1). The writer is `ainb_app::config::persist::write`, which the terminal host runs in `effect_host::execute` after the step; a failure comes back as `global.persist_failed {store, error}` and becomes an error notice.
- Evidence: `ainb-app/tests/persistence.rs` (a settings change leaves `config.toml` untouched until the host writes it, and a write over a directory is reported and noticed); `host_side_effects.rs::no_reducer_module_saves_a_store_outside_the_persistence_effect` (a `CALL_SITES`-style fence over `app/` and `components/`: no `.save()`; the remaining `.save_to(` and `fs::write(` lines listed per module with a reason); the width migration test expects the settings write as an effect.

Decisions:
- In-place is portable, not terminal-host-only. A report names the session and never carries a client, so any host implements it from the effect docs. There is no `in_place_unsupported` report because no host needs one; a host with no terminal widget answers `Observe` with `observer_failed {unsupported: true}` and the reducer stops asking for that row.
- Release is reconciliation, not an effect. The host closes any client whose session `embed_session` no longer names, checked each loop before the frame. A declined report, a row change, a screen change and a detach all release the same way, and a report the reducer ignores cannot leak a client.
- The client lives outside `ainb-core/src/app/*` (the constraint), so `TerminalClients` is a module of its own that the run loop owns, not a `UiState` field, and the preview pane is handed its screen before each frame (`TmuxPreviewPane::show_terminal`).
- The reducer keeps the observer's decisions (settle delay, retry backoff, the own-session rule) and their bookkeeping fields next to `embed_session`. The host keeps what only a client can know: whether tmux supports a read-only client, whether it exited, whether input was written.
- New pane output no longer bumps the tmux section. The bytes are the local host's; a mirrored host renders its own client, so a section bump per PTY write told a subscriber nothing it could draw.
- A failed input write is a report (`terminal_input_closed`), so the host no longer writes state or posts the notice itself.
- The Pal and session chat hosts and the Pal dial tick every frame as host-only state and ask for a repaint when they moved; they no longer bump the fleet section, because no other host can draw from a chat handle this process holds.
- A persistence effect carries the whole store when the reducer holds it (config, favourites, labels, the onboarding record) and only the field when it never loaded the record (git directories, auth provider, the Headroom flag), which the host sets on the file under the store's own lock or load.
- The writer is a service in `ainb-app/src/config/persist.rs`, so every host writes the same files the same way. The terminal host writes synchronously in its loop; `dispatch` only queues.
- Notices that waited on a write now show when the step runs. A failed write says so through `persist_failed`. Two behaviours changed with it: a label rename closes even if the label file then fails to write, and finishing onboarding no longer aborts when the onboarding record fails to write.
- The fence covers `app/` and `components/`, the reducer's modules. Services the reducer calls (`interactive::session_manager`, `git::workspace_scanner`) write their own files and are not in it. Writes left in reducer modules, listed in the fence: session defaults (5 lines: picker back, configure back, launch, advance, Ctrl+R) and the skill manifest (3), which the same step or the next key reads back, and files that are not stores (session snapshots, the abtop marker, the API key env file, the discovery skip marker, recovery archives). They are not in the criterion's list of stores; moving the session defaults needs the read-back removed first.

P5a review (#1043, "Review of c1065b26"), applied on `stevengonsalvez/p5-hosts`:
- The host closes a released client before every effect, not only once per loop, so a full-screen attach or an editor never runs with a stale preview client open.
- A read-only observer takes no input; `in_place_failed` carries `unsupported`, and the reducer stops asking a host that set it.
- The routing rule a host owes while the pane is live (every key to the client except the chord `Keymap::releases_in_place_pane` names) and the report order (`in_place_opened` only once a client is held) are in the `InPlace` doc and pinned by the headless host.
- Terminal host reads of `AppState.host` are fenced (`HOST_STATE_READS`: the session log, the Pal dial and the daemon start offer draw from it until D1), and a probe keeps `HostOnlyState` from ever deriving `Serialize`.

D1 inputs, not P5 work (from the design review of #1043):
- The reducer shells out: `AppState` asks `tmux::process_detection::host_tmux_session_name()` (a `tmux display-message` subprocess) to apply the own-session rule. A second host has its own answer, so it belongs on the host's report or a host fact, not a reducer call.
- `AppState::default` loads `AppConfig` from disk. A reducer built for a second host, or a test, reads the user's files; construction should take the config as an argument.
- The reducer reads the wall clock (`Instant::now()` in the observer settle and retry rules, lease renewal and tick pacing). Replaying a mirrored host's intents needs time to arrive on the intent or a host clock the reducer is given.
