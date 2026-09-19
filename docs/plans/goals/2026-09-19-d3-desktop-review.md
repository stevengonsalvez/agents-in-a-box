# /goal D3 closes the desktop's parity gap: the review tab paints the hunks the reducer already frames, an inbox screen comes back on both surfaces over one framed source, settings carries the config section plus the daemons panel and a native path for the onboarding writes a webview may not run, the stats tab and the plugin fallback cell give a plugin somewhere to draw, and the palette's rows become the reducer's for every surface, proved by the full parity suite and a wdio review journey

─ CONTEXT ─

· First act in the worktree: the lane runs on claude-hetzner under `~/orca/workspaces/agents-in-a-box/p1-ainb-app`. Run `git fetch origin v2 && git switch -c d3-desktop-review origin/v2` before any edit, then copy this file into the branch and commit it as the first signed commit. Every later file change is its own signed commit, and never `git add -A`. Push the branch and open each PR as a draft against `v2`. Message the orchestrator with the head sha at each PR; never poll CI, the orchestrator brings the verdict.

· Project: agents-in-a-box (ainb) desktop programme, slice 3 node D3.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:134`, "D3 review, inbox, settings, burndown, fallback cell | 3 | planned | P5 | full parity suite | base spec D3". Gate to start: P5, met (programme `:125`, P5a #1043 at `ddb4ef6d`, P5b #1057 at `28e6bf7d`, P5c #1061 at `a023f7d1`, P5d #1065 at `eace2637f`). Gate to finish: "full parity suite".
  - The node this one extends: D2, programme `:133`. D2·spec #1170, D2a #1173 at `ac19a9c08`, then #1177, #1182, #1186 and #1190 in stack order. Its goal file is `docs/plans/goals/2026-09-16-d2-desktop-board.md`, and its carries to this node are #1161, #1162 and #1175.
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: the D3 row (`:117`), the "D3 after P5" rule (`:120`), the interface block (`:155-175`), the surface table (`:180-193`), the screen inventory (`:216-230`), the errors table (`:334-343`), the testing strategy (`:353-362`), and the four D2 amendments (`:208`, `:210`, `:212`, `:214`) of which two hand work directly to this node.
  - Multi-surface spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md`: D10 to D18 at `:49-61`. D14 (one status truth) and D18 (mutation envelope) bind anything this node frames.
  - Sibling goals whose format and voice this one matches: `docs/plans/goals/2026-09-15-d1-desktop-shell.md` and `docs/plans/goals/2026-09-16-d2-desktop-board.md`.

· What is on `v2` that D3 builds on, seam by seam. Every path and line below exists on `origin/v2` at `361e8ad8a`, which is before the D2 stack merges: #1177, #1182, #1186 and #1190 add the board, the answer banner, the ACP card, `ainb-desktop/src/agent_status.rs` and the `ui/src` modules that go with them. Re-read this section against the tip the lane actually starts from.

  The twenty sections are the boundary set, and three of the screens D3 draws already have one.
  - `wire/mod.rs:104-124` names all twenty. `git_view` (`:108`), `inbox` (`:116`), `config` (`:118`), `plugins_host` (`:117`) and `hangar` (`:114`) are the ones this node draws from.
  - `wire/mod.rs:544` frames `GitViewView` from `GitViewSection` (`sections.rs:59-66`), which holds the whole `GitViewState`, so the review model the TUI paints is already on the wire: the desktop needs a renderer, not a new family.
  - There is no review screen id. Review is a tab inside the git view: `ainb-core/src/components/git_view.rs:70` lists `Review`, `Commits`, `Markdown` and dispatches to `code_review::render::render` at `:91-92`, under `ids::GIT_VIEW` (`ainb-app/src/app/screens/mod.rs:42`). The model is already reduced in `ainb-app/src/components/code_review/{model.rs,parse.rs,render.rs}`, held on `GitViewState` (`ainb-app/src/components/git_view.rs:15`, `:44` `review: ReviewModel`, `:45` `review_ui: CodeReviewUi`, `:99` `enum GitTab`) with the reducer helpers at `:178-277`. What stays in `ainb-core` is paint only: `components/code_review/render.rs` (850 lines of ratatui) and `highlight.rs`. The click path already exists as a pointer row, `git_view.select_review_row` (`ainb-app/src/app/pointer.rs:46`, builder `:185`), pinned by `ainb-app/tests/review_commands.rs`.
  - `wire/mod.rs:163` frames the inbox as `InboxView {}`, an empty struct (`:608-610`). `sections.rs:763-772` says why: the inbox screen's state was removed from `AppState` before the extraction, the section kept its place so nothing renumbers, and it "gains fields when the screen does". `components/layout.rs:947` records the other half, the `b inbox` hint and its unread badge gone with the screen. **The inbox is not a port in this node. It is a screen that has to come back, on a framed source, for both surfaces at once.**
  - The inbox belongs to **D3-prime**, not to this node: see "WHAT MOVES TO D3-PRIME". The facts stay here because they are what that node starts from.
  - The daemon already serves it: `hangar/inbox_list` (`ainb-hangar-proto/src/methods.rs:1173`) and `hangar/inbox_mark_read` (`:1192`), rows as `InboxEntryRow` (`ainb-hangar-proto/src/events.rs:1061-1086`), written by the aggregator at `ainb-hangar-daemon/src/lib.rs:147`, spawned `:971`. The only surface reading it today is the hangar plugin (`ainb-plugin-hangar/src/plugin.rs:146`, `:337`, `:1529`, `:2248-2267`); `ainb-web` reads `attention/list` instead (`ainb-tui/crates/ainb-web/src/data.rs:283`).
  - Two different things are called the inbox, and this node must not conflate them: the notification inbox above, and the per-parent completion JSONL behind `ainb hangar inbox {peek,drain,commit}` (`ainb-core/src/cli/registry.rs:2926-2956`), which ATC consumes (`cli/fleet/atc.rs:45`, `:247`, `:1201`). The screen is the first.
  - `wire/mod.rs:624-629` frames `ConfigView` from `ConfigSection` (`sections.rs:405-416`: `app_config`, `config_screen_state`, `config_popup_state`, `statusline_status`), and `tests/config_screen_keys.rs` and `tests/config_screen_merge_save.rs` are the behaviour already pinned. The writer is `AppConfig::save` (`ainb-app/src/config/mod.rs:2218`, `save_to_dir` `:2576`) behind the config lock (`config/lock.rs:45`), so a settings form writes through the same path the TUI does.
  - The desktop's settings entry is already drawn inert, with the node's name on it: `ainb-desktop/ui/src/main.tsx:282-285`.
  - `wire/mod.rs:612-615` is the standing refusal this node has to answer or respect: `plugin_ui_states` and `watched_plugin_screens` stay off the wire, because each view is JSON its plugin wrote with keys no key-path check can know in advance, so nothing proves it free of a secret. `PluginsHostView` (`:616-622`) frames presence, capture flags and scrubbed render errors, and nothing else. The host's own storage says the same in one line, `sections.rs:135-137`, "Never in a frame", over `plugin_ui_states` (`:173-176`), `plugin_ui_state_spent` (`:179-182`) and `watched_plugin_screens` (`:184-187`), with `PluginUiState` at `:270-281` and the reducer's writer at `state.rs:13457`. `ainb-app/tests/plugin_actions.rs:111`, `:182-220` and `:301-338` pin it.
  - There is a seam that does not need the view on the wire: `ainb-app/src/app/plugin_action.rs:15` `plugin.owned.action` and `:20` `plugin.owned.watch_screen`, with builders at `:28-60`, which is how a renderer acts on a plugin screen it is watching. The hangar plugin's own contract lists the actions and tokens it accepts (`ainb-plugin-hangar/src/ui_view.rs:10-76`, including `inbox` and `settings`).
  - The TUI's fallback cell already exists and is the shape a desktop cell would copy: `PluginScreen` (`ainb-core/src/app/screens/builtin.rs:54-63`) stashes the viewport and blits the plugin's own cells at `:422-462`, and `build_placeholder_for_unloaded_plugin` (`:196-214`) covers not registered, registered with a render error (`:232-291`) and registered with no frame yet (`:292-330`).
  - Burndown is a plugin that owns the `analytics` screen (`ainb-app/src/app/screens/builtin.rs:16` maps `ids::ANALYTICS` to `burndown`), publishes no `ui.state` at all, only `ui.close_request` (`ainb-plugin-burndown/src/plugin.rs:339-350`), and lives off the session topics it subscribes to (`plugin.rs:153`, `:158`, `:165`). A stats tab that wants its numbers reads those topics' source, not the plugin's paint.

  The desktop shell after D1 and D2.
  - `ainb-tui/crates/ainb-desktop/src/lib.rs` lists the modules: `agent_status`, `bindings`, `clipboard`, `executor`, `host`, `intent`, `shell`, `sidecar`, `terminal`.
  - `src/executor.rs:29` is `NO_PLUGIN_RUNTIME`: this build answers `ForwardToPlugin` and `RunPluginAction` undelivered, and `ainb-app/tests/host_side_effects.rs` proves no module in `ainb-app` owns a runtime. A stats tab and a fallback cell are the first things in the desktop that need a plugin host at all.
  - `src/intent.rs:29` is `Refusal`, `:55-58` is `refused_from_webview` (host-authored rows at `:41-44`, key-only rows through `Keymap::is_key_only`, `ainb-app/src/app/keymap.rs:1700`), `palette_offers` filters the palette at `:73-75`, and `keymap.rs:1581` derives `key_only` from `KeyAction::writes_outside_ainb` (`:993-1000`) rather than from a list. The toast the window shows is emitted at `ainb-desktop/src/main.rs:54-56` and worded at `ui/src/main.tsx:110`.
  - The writes #1175 names are inventoried already, in `ainb-app/tests/key_only_completeness.rs:99-127`: `otel::ensure_settings_env` (`ainb-app/src/otel/mod.rs:325`), `ensure_settings_env_at` (`:334`), `ensure_shell_rc_at` (`:427`), `start_alloy` (`:502`), `setup::provision::install_dep_capture` (`ainb-app/src/setup/provision.rs:195`) and `install_tmux_config` (`:274`, writes `~/.tmux.conf`). Any new row this node adds whose action writes outside ainb joins that list, or the test fails.
  - `src/host.rs` holds `palette()`, built in the host from `intent::palette_offers` and `keymap::command_contexts`. #1161 moves that ownership into the reducer.
  - The webview subscribes through one list, `ui/src/subscription.ts:15-24`: `SUBSCRIBED` is `sessions`, `workspace_load`, `shell`, `tmux`, `fleet`, `config`, `agent_status`, and `AHEAD_OF_READERS` (`:29`) still carries `config` with the comment "settings in D3". `subscription.test.ts` fails when a section is read but not subscribed, or subscribed with neither a reader nor a declared place (#1132). D3 adds `git_view` and `inbox` to the list, and takes `config` out of `AHEAD_OF_READERS` when the settings page reads it.
  - What a new screen costs, end to end: a `ScreenId` in `ainb-app/src/app/screens/mod.rs:14-55`; a section with its `Versioned` slot, a `view!` in `ainb-app/src/wire/mod.rs` plus its arms at `:79-96`, `:102-123` and `:147-165`; keymap rows in `keymap_defaults.rs` with `key_only_completeness.rs` updated for any outside-ainb action; a reader and a subscription entry in the webview; `bindings/AppState.ts` regenerated for the CI job at `.github/workflows/ci.yml:259-296`; and the desktop jobs at `.github/workflows/desktop.yml:45`, `:114`, `:229`, including the `Desktop.ts` freshness step at `:105`.

  The parity suite, which is this node's gate.
  - `ainb-app/tests/parity.rs` builds every committed fixture and asserts it opens the screen it names, with a floor of twelve fixtures. `ainb-core/tests/parity_snapshots.rs` draws the same fixtures through a ratatui `TestBackend`.
  - The committed fixtures are `config`, `daemons`, `git_view`, `home`, `log_history`, `new_session_pick_repo`, `onboarding`, `session_list`, `session_list_help`, `session_recovery`, `setup_menu`, `skill_manager`. There is no fixture for a review screen, an inbox or a stats screen, and no DOM half at all: the base spec's parity layer (`:358`) asks for "ratatui `TestBackend` text and Solid DOM text, both diffed against an expected-facts list", and only the ratatui half exists.
  - So "full parity suite" as a gate means two things this node builds: a fixture per screen D3 draws, and the DOM half that renders the same fixture in the webview and compares the same expected facts.

─ WHAT D3 MAY NOT REOPEN ─

· A plugin's `ui.state` does not ride a frame (`wire/mod.rs:612-616`). The stats tab and the fallback cell are drawn some other way, or the node writes an amendment that says how a plugin view crosses the wire safely and what proves it. No silent exception.
· `ainb-app` owns no plugin runtime (`host_side_effects.rs`), and the desktop answers plugin effects undelivered (`executor.rs:29`). A plugin host in the desktop is a new process boundary with its own quarantine rules, not a module.
· A row that writes outside ainb still runs only from its key (`keymap.rs:1574`). #1175 adds a path the webview cannot script; it does not relax the rule.
· Locked decisions D10 to D18 stand. D14 keeps one status truth, so nothing in this node adds a second projection of a status a section already carries. D18 binds any write this node sends: a mutation carries its envelope, with an op id and a fence, and a new write that ships without one is the debt this programme already recorded once against `AnswerParams.mutation` and does not repeat. D15 binds what a root selector returns: scalars, never lists or objects (multi-surface `:58`), so a review row list is read by the screen that draws it, not by a header count.
· `ainb-tui/crates/ainb-core/src/app/*` is untouched, the standing lane rule.

─ THE SEAMS D3 HAS TO OPEN ─

```
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 1 inbox section frames nothing │──▶│ give it fields from one source,  │
│   wire/mod.rs:163, :608        │   │ read by the host, framed bounded │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 2 palette rows built in the    │──▶│ the reducer owns the row set;    │
│   host, host.rs palette()      │   │ each surface ranks and draws     │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 3 onboarding writes refused    │──▶│ a shell-owned confirmation the   │
│   at the webview seam (#1175)  │   │ webview cannot script            │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 4 no DOM half of parity        │──▶│ one fixture, two renderers, one  │
│   parity.rs + parity_snapshots │   │ expected-facts list              │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 5 git_view frames a whole diff │──▶│ bound it like the D2 conversation│
│   twice, 4 MiB withholds all   │   │ row window + per-file cap        │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 6 nothing draws the oversize   │──▶│ the webview says a section was   │
│   signal, frame.rs:169         │   │ withheld and why                 │
└────────────────────────────────┘   └──────────────────────────────────┘
```

The fifth seam is the one the review tab cannot skip. `diff_content` and the review rows are two copies of the same diff on one section, so the bound is a row window plus a per-file cap, the treatment the D2 amendment gave the conversation (`spec:214`): the reducer writes a bounded projection, each new field carries a scrubber or an allow-list reason, and what was cut says so rather than reading as a short diff. The sixth is its partner: `FrameBatch.oversize` exists and no renderer in this repository reads it, so a withheld section today is a screen that silently stops updating. D3b's webview draws it, on the review tab first, and the `ui/` test for it names the section and the reason.

─ SCOPE, STAGED AS PRs ─

Four PRs: the amendment, the seams, the review tab, and settings with the journey, the proof scenario, the CI wiring and the programme row folded into it. Each is mergeable alone, targets `v2`, and carries its own proof. The amendment goes first, as D1a's #1111 and D2·spec did, because the programme's rules of the road require the spec to change before the node that needs it.

**D3·spec, the amendment (docs only).**
· Touches: `docs/plans/2026-09-04-desktop-shared-core-spec.md`.
· Says four things, one paragraph each: which throughput number each path can prove (#1162, the terminal gate at `:360` reads "50MB in under 2s with zero dropped bytes", which the desktop journey cannot measure because an attached tmux client is sent rendered screen updates rather than a replay of the pane's bytes, so the byte-exact number is D1c's Rust-level one and the journey's figure is recorded); that the `git_view` section is bounded the way the conversation was (`:214`), because its diff rides the frame twice and an oversize section is withheld whole; that the D3 row splits into D3 and D3-prime, with nothing dropped; and that "full parity suite" means a fixture per screen the node draws, both renderers, with a mutation check that proves the suite can fail.
· Gate: docs jobs only. Merged before D3a opens.

**D3a, the seams: the inbox source, the palette rows, the parity harness.**
· Touches: `ainb-tui/crates/ainb-app/src/components/git_view.rs`, `src/components/code_review/`, `src/wire/mod.rs`, `src/wire/fields.rs`, `src/app/pointer.rs`, `ainb-tui/crates/ainb-desktop/src/host.rs`, `bindings/AppState.ts`, `tests/fixtures/section_key_paths.txt`, `ainb-app/tests/parity/`.
· Builds: seam 5 (the `git_view` section bounded: a row window and a per-file cap, written by the reducer, with what was cut said on the frame rather than read as a short diff), seam 2 (#1161: the reducer owns the palette's rows, the host asks for them, `palette_offers` becomes a ranking rather than a filter), seam 4's harness half (one fixture loader both renderers read, and the expected-facts list beside each fixture, with the mutation check that proves it can fail).
· Order inside the PR: the commit BEFORE the palette moves captures the host's current row set as a golden fixture, so the move is diffed against what the host built rather than against itself.
· Proof: `ainb-app` tests that a diff past the bound frames inside `MAX_FRAME_BYTES` and says what it cut; that a credential-shaped string in a hunk does not survive the projection, with `scrub_rows` (`ainb-app/src/components/code_review/model.rs:75-80`) kept over the diff text; that the palette's rows match the golden row set for every context; the fixture and the bindings regenerated in the same commit with the triage reason in the message.
· Gate: `cargo test -p ainb-app`, `cargo test -p ainb-core`, the tripwires, the excluded workspace's own `cargo test`, "TypeScript bindings freshness", "Contracts".

**D3b, the review tab.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/src/review.ts`, `review.tsx`, `subscription.ts`, `main.tsx`, `shell.css`, `ui/package.json` for the editor dependency, `ui/src/*.test.ts`.
· Builds: the review tab over the framed `git_view` section, hunks and rows drawn from the reducer's own `ReviewModel` (`ainb-app/src/components/git_view.rs:44`), selection and scroll sent as `git_view.select_review_row` (`pointer.rs:46`) and the wheel rows `review_commands.rs` pins, no diffing and no hunk parsing in TypeScript.
· Proof: `ui/` tests for the projection (a hunk renders its rows; a file with no hunks says so; a selection maps to the reducer's row id); a parity fixture `code_review` drawn by both renderers against one expected-facts list.
· Gate: as D3a, plus `tsc --noEmit --strict` and the frontend tests.

**D3d, settings, the daemons panel and the onboarding writes (#1175).**
· Touches: `ainb-tui/crates/ainb-desktop/src/` (a shell-owned confirmation command), `ui/src/settings.ts` and `settings.tsx`, `ui/src/main.tsx`, `ainb-tui/crates/ainb-app/src/app/` if a row's refusal reason gains a pointer to the new path.
· Builds: the settings page over the framed `config` section as a form, the daemons panel the screen inventory maps to settings (`:229`), and the native path for the three onboarding writes the webview may not run: install the focused dependency or all of them, write `~/.tmux.conf`, and finish an OpenTelemetry setup. The confirmation is owned by the Tauri shell, never a keymap row sent from the webview, and the refusal toast points at it once it exists.
· Also lands, folded in rather than as a sixth PR: the wdio review journey as its own spec file beside `journey.e2e.js` and `answer.e2e.js`, under the one-runner refusal #1160 landed; the `d3-review` proof scenario in the shape of `d2-board.sh`, registered in `ALL_NODES` with a `skip` when `xvfb-run` is missing; the CI wiring in `.github/workflows/desktop.yml`; and the programme row split with the run ids.
· Proof: a desktop test that the webview cannot reach the write without the shell's own confirmation, and that the refused rows stay refused (`intent.rs` seam tests); `ui/` tests for the form; a parity fixture for the settings page; the journey and the scenario green.
· Gate: the success criteria below, all green.

─ WHAT MOVES TO D3-PRIME ─

The inbox screen is its own node. Bringing a screen back on two surfaces over a framed family that does not exist yet is D2a plus D2b in size: a new field set on `InboxSection`, its scrubbers, its fixture, a TUI screen, a webview screen, and a write. D3's gate does not need it, because "a fixture per screen D3 draws" holds only for the screens D3 draws.

D3-prime, when it is written, carries:
· The inbox section's fields, read through `hangar/inbox_list` (`ainb-hangar-proto/src/methods.rs:1173`) by the host, bounded, each field scrubbed or allow-listed with its reason.
· The screen on both surfaces at once, the TUI's and the desktop's, because the section is the boundary and a screen that exists on one surface only is the drift the section set exists to stop.
· `hangar/inbox_mark_read` (`methods.rs:1192`) as **a mutation under D18**: it carries a mutation envelope with an op id and a fence, and the node states what a replay of it does. This programme already carries one envelope-less write as debt (`AnswerParams.mutation`); a second one is not added quietly.
· The stats tab and the plugin fallback cell, with the costs recorded under "WHAT D3 DEFERS".

─ WHICH EXISTING TESTS MUST STAY GREEN ─

Unchanged on every PR of this node, or changed only in their own commit with the reason in the message:
· `ainb-app/tests/host_side_effects.rs`, including `no_module_in_the_crate_owns_the_plugin_runtime`.
· `ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
· `ainb-app/tests/state_serde.rs` with `tests/fixtures/section_key_paths.txt`, regenerated with `UPDATE_SECTION_KEY_PATHS=1` in the same commit as each new field, after triage against the deny-lists.
· `ainb-app/tests/bindings.rs` and the CI job "TypeScript bindings freshness"; `bindings/AppState.ts` regenerated in the same PR.
· `ainb-app/tests/parity.rs` and `ainb-core/tests/parity_snapshots.rs`: this node adds fixtures, and no existing fixture's expected facts change without its own commit.
· `ainb-app/tests/key_only_commands.rs`, `key_only_completeness.rs`, `command_gate.rs`, `review_commands.rs`, `config_screen_keys.rs`, `config_screen_merge_save.rs`, `intent_dispatch.rs`, `plugin_actions.rs`, `renderer_free.rs`, `persistence.rs`, `effects.rs`.
· `ainb-core/tests/keymap_parity.rs` with `tests/fixtures/keymap_rows.txt`, and `docs/tui/keyboard-shortcuts.md` regenerated by `ainb-tui/scripts/gen-keymap-docs.sh` on any branch that adds a bound row, so the "CLI reference freshness" job is green per head.
· `ainb-desktop/tests/`: `host_contract.rs`, `sidecar.rs`, `terminal.rs`, `shell.rs`, `intent.rs`, `workspace_load.rs`, `bindings.rs`.
· `ainb-desktop/ui/src/*.test.ts`, including `subscription.test.ts`, which fails when a section is read without being subscribed or declared.
· The three jobs of `.github/workflows/desktop.yml`, the workspace jobs of `ci.yml`, and a full `bash ainb-tui/scripts/proof/run.sh` reporting every other node passing, twenty before this node and twenty-one after.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. **The full parity suite passes, both halves.** Every screen D3 draws has a committed fixture, and each fixture is rendered by the ratatui backend and by the webview and diffed against one expected-facts list, so a fact drawn by one surface and not the other fails. The suite runs in CI on both runners. The fixture count and the screens covered are recorded on the PR.
2. **The wdio review journey is green in CI**, on `ubuntu-latest` under `xvfb-run` and on `macos-latest` with the leg `MACOS_E2E_LEG` declares. In one run against a real daemon and real sessions: a repository with real changes opens the review tab, the hunks drawn are the reducer's own model rather than a diff computed in the webview, a selection sent from the window lands on the row the frame named, the inbox screen lists what the framed source carries, and the settings page reads the config section. Nothing in this criterion is measured by a screenshot.
3. **The proof harness scenario `d3-review` passes on a `v2` build**, writing a `result.json` with `pass: true`, whose observed lines carry the sections the renderer applied, the review rows drawn, the inbox rows drawn, and the daemons panel's reading of each daemon. It is in `ALL_NODES`, and a box with no headless X server records a `skip` rather than a failure.
4. **Every field D3 added to the wire is provably scrubbed and provably in sync**: `section_key_paths.txt` names each new leaf, regenerated in the commit that added it; each new inbox field either carries a scrubber from `wire/fields.rs` or is allow-listed with its reason; a credential-shaped string assembled at runtime does not survive the inbox projection; `bindings/AppState.ts` is fresh and `tsc --noEmit --strict` is clean.
5. **#1161, #1162 and #1175 are closed by this node**, each with the evidence on its PR: the palette's rows come from the reducer for every surface, with a test that the two lists agree; the spec says which throughput number each path can prove; and the onboarding writes have a desktop path the webview cannot script, with the refusal toast pointing at it.
6. Final deliverable runs without errors.
7. You can show proof (screenshot, test output, URL).

The softer ones, measured and recorded rather than gated:
· The bytes the inbox projection frames at its bound, against `MAX_FRAME_BYTES`.
· What a plugin view would cost to frame safely, if D3·spec defers it again.
· The number of sections a review-and-inbox drain applies, from `renderer_applied`, so D4' knows what the last surface costs.

─ CONSTRAINTS ─

· PRs target `v2`, never `main`. Staged as the six above, each reviewable alone.
· Every file change is its own signed commit: `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`. Never `git add -A`, never `git add .`; stage by named path.
· No attribution trailers, no `Co-Authored-By`, and no mention of Claude, an AI or any assistance in a commit message or a PR body.
· No em dashes on any line you author, in code, docs, commit messages or PR bodies.
· Never name the reference product this programme drew prior art from.
· Open every PR as a draft and flip it ready only when its own gate is green. Never merge your own PR.
· Never poll CI. The orchestrator brings the verdict; keep working on anything that does not depend on it.
· Never touch `ainb-tui/crates/ainb-core/src/app/*`. Merge `origin/v2` before touching a file another lane is on, and name shared files in the PR body.
· No plugin JSON on a frame without the D3·spec amendment that says how it is proved safe.
· Tauri, SolidJS 1.9.15, TypeScript 5.6.3 and Node 22 stay pinned, every frontend dependency in a committed lockfile, CI installing with `npm ci`. A new editor dependency for the review tab is pinned the same way and justified on the PR.
· The app builds headless in CI on both runners.
· Disk ceiling 92 percent; `cargo clean` of the desktop target is allowed when it nears that.
· Never kill a tmux server, never a bulk or wildcard kill; kill a session by exact name only.
· Record every decision in this goal file's progress log as you take it.

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

─ WHICH CARRIES THIS NODE TAKES ─

| issue | title | where | why |
|---|---|---|---|
| #1161 | the reducer owns the palette's rows, for every surface | D3a | D3 adds palette rows for three screens; leaving the row set in the host would spread one duplicated rule across the surface this node finishes |
| #1162 | say which throughput number each path can prove | D3·spec | a wording fix on the terminal gate, owned by the node that closes the parity suite |
| #1175 | a desktop-native path for the onboarding steps that write outside ainb | D3d | settings is the surface those steps belong to, and the refusal toast has nowhere to point until it exists |
| #1188 | desktop and terminal read agent status through two different hosts | **not this node** | it waits on #1166 landing on `v2`, which is P6's; take it only if #1166 is in before D3a opens |
| #1049 | Claude sessions never learn `provider_session_id` | **not this node** | it is a daemon and hook identity fix; D2 recorded it as the reason a board card stays waiting, and D3 draws no new card |

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

File each as an issue with its evidence. Do not fix it in this node.
· The board's stat strip and turn timeline, deferred by the D2 amendment (`spec:212`): no section carries a token count, a diff stat, a tool count or a turn timeline. File with the measurement of what a daemon read would cost, unless D3's stats tab lands the read, in which case record that instead.
· The presence badge naming the other live surfaces from `hangar/connections_list`, which the base spec asks for as "also open in: tui, web" (`:316`) and which no section carries.
· `AnswerParams.mutation` is `MutationEnvelope::default()`, so an answer carries no op id and no fence, against D18.
· The reducer reads the wall clock in settle, retry, lease renewal and tick pacing; replaying a second host's intents needs time on the intent or a host clock the reducer is given.
· The sidebar width key: whether the desktop shares the TUI's `sessions_sidebar_fraction` or keeps its own.
· A frame cache keyed by `(screen, viewport)` so two hosts at different sizes do not share one plugin render.
· Open tabs are not persisted to `desktop.json`.

─ OPEN QUESTIONS THE LANE ANSWERS ON A PR BODY ─

· **Where the inbox's rows come from.** Recommended: `hangar/inbox_list` (`ainb-hangar-proto/src/methods.rs:1173`), read by the host the way `agent_status` reads `fleet/roster_status`, with `hangar/inbox_mark_read` (`:1192`) as the only write. The spec's inventory still says "list from notifyd SQLite via core section" (`:199`), which is stale: the daemon aggregates the inbox itself (`ainb-hangar-daemon/src/lib.rs:147`), a second process opening notifyd's database would be a third reader of one store, and the rows already have a wire shape in `InboxEntryRow` (`ainb-hangar-proto/src/events.rs:1061-1086`). Answer on D3·spec with the bound on the rows and what each field's scrubber is.
· **Whether the inbox screen returns to the TUI in this node or only to the desktop.** Recommended: both, in D3c, because the section is the boundary and a screen that exists on one surface only is the drift the section set exists to stop. Answer on D3·spec if the lane splits it.
· **How a plugin view reaches the desktop.** Recommended: the fallback cell paints the plugin's own cells, the way `PluginScreen` already blits them in the TUI (`ainb-core/src/app/screens/builtin.rs:422-462`), and the window acts on that screen through `plugin.owned.watch_screen` and `plugin.owned.action` (`ainb-app/src/app/plugin_action.rs:15-33`), so no plugin JSON rides a frame. Burndown makes the stats half easy to scope: it publishes no `ui.state` at all (`ainb-plugin-burndown/src/plugin.rs:339-350`) and lives off the session topics at `:153-165`, so a stats tab is a component over that data rather than a render of the plugin's paint. If the lane wants `ui.state` on the wire instead, that is an amendment carrying a proof that a plugin view can be shown free of secrets, not a field. Answer on D3·spec.
· **Whether the review tab brings a text editor dependency.** Recommended: yes, one pinned editor for the merge view, because hand-rolling selection, folding and syntax in the webview is a larger surface than the dependency, and the hunks themselves still come from the reducer. Answer on D3b with the package, its version and its licence.
· **What the DOM half of parity renders with.** Recommended: the existing node test runner over the same fixtures, rendering the Solid components headless and comparing the expected-facts list, rather than a second browser run, because the wdio journeys already cover the real window. Answer on D3a.
· **How `git_view` is bounded, not whether it is measured.** Decided, not open: the section is bounded in D3a. The shape is enough, because `GitViewView` frames the whole `GitViewState` (`wire/mod.rs:544`) and `git_view.select_review_row` already carries the click (`pointer.rs:46`, `tests/review_commands.rs`). The size is not: the diff rides the frame twice, as `diff_content` (`ainb-app/src/components/git_view.rs:21`) and again as `review.files[].hunks[].rows`, and a section over `MAX_FRAME_BYTES` (`wire/frame.rs:149`, 4 MiB) is withheld WHOLE into `FrameBatch.oversize` (`frame.rs:169`, `:398`), so one large diff blanks the file tree and the commit box with it. Answer on D3b with the measured bytes of a real repository's diff at the bound, and the count of how often it was hit.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (screenshot, test output, URL). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the six PRs:
1. D3·spec: the amendment (#1162's wording, the inbox's source, how a plugin view reaches a surface, what the parity gate means).
2. D3a: the seams (the inbox section's fields, #1161's palette rows in the reducer, the parity harness both renderers share).
3. D3b: the review tab over the framed `git_view` section.
4. D3c: the inbox screen, on the TUI and the desktop, from one source.
5. D3d: settings, the daemons panel, and #1175's native path for the onboarding writes.
6. D3e: the stats tab and the fallback cell, the wdio review journey, the `d3-review` proof scenario, the CI wiring and the programme row.
