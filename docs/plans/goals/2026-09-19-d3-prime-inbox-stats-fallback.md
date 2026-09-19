# /goal D3-prime brings the inbox back on both surfaces at once, over a framed `inbox` section the host fills from the daemon's `hangar/inbox_list`, with `hangar/inbox_mark_read` sent as the D18 mutation it already is, then gives the desktop a stats tab over the daemon's own usage projection rather than a third scan of the provider logs, and a plugin fallback cell painted from a plugin host the Tauri shell owns, each of the three landing as its own separable PRs, proved by the full parity suite with its mutation check, a wdio inbox journey and a `d3p-inbox` proof scenario

─ CONTEXT ─

· First act in the worktree: this lane runs on the laptop under `~/orca/workspaces/agents-in-a-box/d3p-inbox-goal`, which is a worktree, not the main checkout. Run `git fetch origin v2` and branch from `origin/v2` before any edit: `docs/d3-prime-goal` for this file, then one branch per PR below. Every file change is its own signed commit (`git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`), never `git add -A`, never `git add .`. Push and open every PR as a draft against `v2`. Report by a PR comment carrying the head sha; never poll CI, the orchestrator brings the verdict. Three other lanes share this laptop: build with `-j 3` and `CARGO_INCREMENTAL=0`.

· Gate to start: D3 is merged on `v2`. On 2026-09-19 that is not yet true: D3·spec #1205 is merged (the amendments this file cites), D3a #1217 is in review on `d3a-seams` at `9059ddc13`, and D3b and D3d are not open. Every anchor below is verified on `origin/v2` at `175a421ff`; anchors marked "D3a" are on #1217 and move when it merges. Until D3 lands this lane reads and does not code, except for this file.

· Project: agents-in-a-box (ainb) desktop programme, slice 3 node D3-prime.
  - Programme row: `docs/plans/2026-09-12-desktop-programme.md:135`, "D3' inbox, burndown stats, plugin fallback cell | 3 | planned; split out of D3 on 2026-09-19 with nothing dropped ... | D3 | full parity suite | base spec D3, amended 2026-09-19". The row after it, `:136`, is D4-prime, which depends on this node. The mermaid at `:60` still draws D3 straight into D4-prime; the last PR of this node fixes the graph with the row.
  - The node this one extends: D3, goal `docs/plans/goals/2026-09-19-d3-desktop-review.md`. Its "WHAT D3 DEFERS, WITH ITS COST" (`:114-118`) and "WHAT MOVES TO D3-PRIME" (`:120-128`) are this node's brief, and its open questions at `:221-223` are answered there with recommendations this node inherits. Its follow-up at `:217`, what a surface does when it meets a section set it does not know, is the one that this node's first PR touches, because it adds fields to a section that has been empty since the extraction.
  - Base spec `docs/plans/2026-09-04-desktop-shared-core-spec.md`: the D3-prime row (`:118`), the screen inventory row for the inbox (`:200`), the surface table rows for the inbox, the stats tab and the plugin cell (`:237-239`), the parity layer (`:369`), and the four amendments of 2026-09-19 (`:217`, `:219`, `:221`, `:223`, `:225`). Two of those are this node's contract: `:221` records the cost of the stats tab and the fallback cell, and `:223` says the inbox reads the daemon, not notifyd's database, with mark-read as a mutation under D18. `:396` records that replacing notifyd with the daemon's attention table as the inbox source is a later migration, not this node's.
  - Multi-surface spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md`: D10 to D18 at `:53-61`. D14 (one status truth), D15 (root selectors return scalars) and D18 (mutation envelope) bind what this node frames and writes.
  - Sibling goals whose format and voice this one matches: `docs/plans/goals/2026-09-19-d3-desktop-review.md` and `docs/plans/goals/2026-09-19-p6e-sessions-one-source.md`.

· What is on `v2` that D3-prime builds on, seam by seam.

  The inbox: an empty section, a daemon that already serves it, and a write that already carries its envelope.
  - `wire/mod.rs:104-124` names the twenty sections; `inbox` is `:116`. `wire/mod.rs:159` frames it as `InboxView {}`, an empty struct at `:711`, registered for bindings at `:1026` and `:1053`. `sections.rs:763-771` is the other half: `InboxSection {}`, "Empty on purpose ... keeps its place and gains fields when the screen does". `components/layout.rs:961` records the `b inbox` hint and its unread badge gone with the screen.
  - What was deleted, so the rebuild knows its own size: commit `62dedf28e`, "feat(sessions): delete the host notifyd Inbox", removed `ainb-core/src/components/inbox.rs` (629 lines), 18 lines of screen shim in `ainb-core/src/app/screens/builtin.rs`, the id in `screens/mod.rs`, 292 lines of `layout.rs`, and `tests/tripwire_inbox_opens_and_renders.rs` (385 lines). That screen read notifyd. This one reads the daemon, so nothing from it is restored by revert.
  - The daemon serves the rows. `hangar/inbox_list` is `ainb-hangar-proto/src/methods.rs:1191` (params `InboxScopedParams`, result `InboxListResult`: entries newest-first plus the recipient's unread count), `hangar/inbox_mark_read` is `:1210` (stamps `read_at` on every unread entry addressed to that actor; "Idempotent (a re-sweep flips nothing ...)"). Both are dispatched at `ainb-hangar-daemon/src/rpc/mod.rs:1528-1529`; `handle_inbox_mark_read` is `:12905`. The rows are written by the aggregator, `ainb-hangar-daemon/src/lib.rs:139-147`, spawned at `:996-1004`.
  - The row shape is on the wire already: `InboxEntryRow` at `ainb-hangar-proto/src/events.rs:1062-1086`: `id` (ULID), `kind` (`issue` / `comment` / `task`), `event` (the discriminant), `subject_id`, `summary` ("a short pre-rendered human line"), `recipient` (`member:<id>` / `agent:<id>`), `created_at`, `read_at: Option<i64>` where `None` is unread. `summary` is the one free-text field and is the one that needs a scrubber; every other field is an id, an enum or a number, and is allow-listed with that reason.
  - `InboxScopedParams` (`ainb-hangar-proto/src/snapshots.rs:47-60`) is `{ workspace_id, recipient?, ..MutationEnvelope flattened }`: **the write already carries its D18 envelope on the wire type** (`:53-60`, `mutation: crate::mutation::MutationEnvelope`, `#[serde(flatten)]`). The committed mutation registry lists it: `ainb-hangar-proto/src/mutation.rs:1186-1192`, `HANGAR_INBOX_MARK_READ`, tier `Dedupe`, fence `Fk::None`. The daemon applies the ledger generically at dispatch, `ainb-hangar-daemon/src/rpc/mod.rs:1338-1346` ("Every mutation goes through the ledger guard ... a mutation whose caller sent no op id, passes straight through"), with the claim outcomes documented at `rpc/mutation.rs:8-19`: a committed op id returns the stored reply verbatim. So the D3 goal's "carries a mutation envelope with an op id and a fence" is, on the daemon side, already true; **what is missing is a client that mints the op id.** The only client today sends none: `ainb-plugin-hangar/src/plugin.rs:2248-2261` fires mark-read with the params built at `:337-339` (`inbox_params(ws)`, workspace plus `SELF_AUTHOR_REF`), and the plugin's other writes send `MutationEnvelope::default()` (`:2906`, `:2928`). The same default is sent from nine sites in `ainb-app` (`fleet/chat_host.rs:192`, `fleet/control.rs:198`, `:229`, `:380`, `:642`, `:725`, `fleet/pal_dial.rs:311`, `interactive/session_manager.rs:377`, and the answer path the D3 goal named). The D3 goal counted "one envelope-less write as debt"; the count is that. This node adds none and mints one.
  - The workspace the read names: the plugin uses `DEFAULT_WORKSPACE_ID` (`ainb-plugin-hangar/src/connection.rs:33`, `"default"`) and recipient `member:me`. No section carries a workspace id and no desktop screen picks one; the read uses the same default and the same recipient, and a picker is not this node.
  - The only readers today: the hangar plugin (`plugin.rs:146-151` request ids, `:1529` and `:1769-1781` `apply_inbox`, `:2760-2761` the list request) draws the daemon's inbox as a sub-screen of the plugin-owned `hangar` screen (`ui_view.rs:29` `ACTIONS`, `:42` and `:66` the `inbox` token); `ainb-web` reads `attention/list` instead (`ainb-web/src/data.rs:284`). The CLI's `ainb hangar inbox {peek,drain,commit}` (`ainb-core/src/cli/registry.rs:2926-2942`) is the per-parent completion JSONL, a different thing with the same name; this node does not touch it.
  - The read pattern to copy: `AgentStatusSection` (`sections.rs:776-793`: `absent: Option<String>`, `head_revision`, `mark_absent` at `:831`, `observe_head` at `:840`) is filled by a host-owned reader of `fleet/roster_status`. On `v2` the desktop's is `ainb-desktop/src/agent_status.rs` (`:4-8`, one read in flight on a worker thread, reported into an inbox the tick drains) and the terminal's is `ainb-core/src/agent_status_host.rs`; #1215 (`ad804d595`, in review, closes #1188) moves the reader into `ainb-app/src/fleet/agent_status_reader.rs` as `AgentStatusReader { spawn, drain_into }` for both hosts. **If #1215 is on `v2` when D3p-a opens, the inbox reader is a sibling of that type in the same module; if not, D3p-a ships the reader in the shape #1215 proposes and says so on the PR, so the two do not diverge.** `attention_poll` (`ainb-app/src/fleet/attention_poll.rs:56`, held on the sections at `sections.rs:614-621`) is the older shape and is not copied.

  The TUI screen, and the seam that lets it land without touching `ainb-core/src/app/*`.
  - Built-in screens are `Screen` impls (`ainb-core/src/app/screens/mod.rs:17-28`, `pub trait Screen`) held by `ScreenRegistry` (`ainb-core/src/app/registry.rs:13-31`, `pub fn register`). `register_builtins` (`screens/builtin.rs:824-842`) fills it, and `LayoutComponent::new` calls it at `components/layout.rs:311-313`. **A new screen does not have to be registered inside `register_builtins`: `register` is public and `layout.rs` is under `components/`, so the inbox screen's `Screen` impl lives in `ainb-core/src/components/inbox.rs` and is registered on the line after `:313`.** The standing rule that `ainb-core/src/app/*` stays untouched holds without an exception. The screen id joins `ainb-app/src/app/screens/mod.rs:14-55` (`ids::INBOX`) and the uniqueness test at `:81-118`.
  - The screen shim pattern: `DaemonsScreen` (`screens/builtin.rs:566-580`) renders `crate::components::daemons::render(frame, area, &state.hangar.daemons_state)` from a section and holds nothing. The inbox screen is that shape over `state.inbox`.
  - Keys: `GoToDaemons` (`keymap_defaults.rs:383`) is the row shape for reaching a screen from home; the inbox row is `b` from home if `keymap_defaults.rs` still has it free, else the next free key, with `ainb-core/tests/fixtures/keymap_rows.txt` and `docs/tui/keyboard-shortcuts.md` regenerated. Mark-read is a row in the inbox context. Neither writes outside ainb, so `key_only_completeness.rs` does not change.
  - The hangar plugin's own inbox sub-screen stays as it is: plugin-owned paint under `ids::HANGAR`, reached through the plugin. Two paints of one daemon list on one surface is drift; it is filed, not fixed here (see "FOLLOW-UPS").

  The desktop screen.
  - `ainb-tui/crates/ainb-desktop/src/lib.rs:19-29` lists the modules: `agent_status`, `bindings`, `clipboard`, `executor`, `host`, `intent`, `shell`, `sidecar`, `terminal`. #1215 deletes `agent_status` and adds `DesktopHost::start_agent_status`.
  - The webview subscribes through one list, `ui/src/subscription.ts:19-25` `SUBSCRIBED` (`sessions`, `workspace_load`, `shell`, `tmux`, `fleet`, `config`, `agent_status`), with `AHEAD_OF_READERS` at `:33` (`shell`, `tmux`, `config`). `subscription.test.ts` fails when a section is read but not subscribed. D3 adds `git_view`; D3-prime adds `inbox`, then `usage`.
  - The board is the landing surface (`ui/src/main.tsx:86-89`); the settings entry is inert until D3 (`:362-363`). The inbox is reached the way the board is: a top-level surface with a header count, and the header count is a scalar root selector per D15 (`ui/src/selectors.ts` is where the existing counts live).
  - What a new screen costs, end to end, is the list in the D3 goal at `:38`: a `ScreenId`, a section's fields with their `Versioned` slot, the `view!` in `ainb-app/src/wire/mod.rs` plus its arms, keymap rows, a reader and a subscription entry in the webview, `bindings/AppState.ts` regenerated for `.github/workflows/ci.yml`'s "TypeScript bindings freshness", the desktop jobs at `.github/workflows/desktop.yml:45` and `:114`, and the `Desktop.ts` freshness step at `:105`.

  The stats tab: the counters have a daemon-owned producer already, and the D2 amendment did not know it.
  - Burndown owns the TUI's `analytics` screen (`ainb-app/src/app/screens/builtin.rs:15-21` `PLUGIN_SCREENS`, `:16` maps `ids::ANALYTICS` to `burndown`), publishes no `ui.state`, only `ui.close_request` (`ainb-plugin-burndown/src/plugin.rs:340-350`), and lives off `sessions.usage_data`, `sessions.scan_progress` and `sessions.refresh_request` (`:153-165`), which the session-reader plugin publishes from its own scan of the provider logs (#391 and #393 record what that scan costs).
  - **The daemon already serves a bounded usage projection.** `fleet/usage_summary` is `ainb-hangar-proto/src/methods.rs:449-453` (params `FleetUsageSummaryParams { period }` at `fleet.rs:675-679`, result `FleetUsageSummaryResult` at `:774-800`: `state` (`scanning` / `ready` / `partial` / `unavailable`), `totals`, `daily` capped at 30, `providers`, `models`, `projects` each capped at 10, `detail` capped at 1,024 bytes, `:682-686`), gated by the capability `fleet.usage.read` (`fleet.rs:29`), dispatched at `rpc/mod.rs:1582` through `require_fleet_capability`. `fleet/usage_dashboard` (`methods.rs:454-459`) is the rich sibling behind `fleet.dashboard.read`. The producer is `ainb-hangar-daemon/src/fleet_usage.rs` (`:1-4`, "Provider logs and canonical model-rate parsing stay behind this module. The public RPC receives only aggregates, never paths, transcripts, or calls"), which scans through `ainb_plugin_session_reader::scanner` (`:20`), the same scanner burndown's data comes from, on a fifteen-minute refresh (`:27`).
  - So the D3 goal's "a new daemon read for counters no section carries" is half right: no section carries them, and the read exists. The stats tab is a section the host fills from `fleet/usage_summary`, not a verb this node writes and not a scan the desktop runs. The D14 argument is made in the amendment below, not assumed.

  The fallback cell: the desktop runs no plugin runtime, and the runtime is a crate the shell can own.
  - `ainb-desktop/src/executor.rs:29` is `NO_PLUGIN_RUNTIME`; `Effect::ForwardToPlugin` (`:121`) and `Effect::RunPluginAction` (`:134`) are answered undelivered, and `ainb-app/tests/host_side_effects.rs` proves no module in `ainb-app` owns a runtime.
  - The TUI's host owns one: `ainb-core/src/host.rs:73-83` holds `Option<ainb_plugin_runtime::RuntimeHandle>` with `set_plugin_runtime` for "a host that brings its own runtime up"; `ainb-app/src/plugins.rs:1-7` builds the tokio-backed `Runtime` and hands out a `Send + Clone` handle whose surface never awaits (`try_recv_render`, `snapshot_get`, `invoke_action`). The plugins are subprocesses speaking JSON-RPC; the desktop bundle stages the daemon sidecar already, and stages plugin binaries the same way.
  - The TUI's cell: `PluginScreen` (`ainb-core/src/app/screens/builtin.rs:54`) blits the plugin's own painted cells at `:426-439`, falling back to `build_placeholder_for_unloaded_plugin` (`:214`) for not registered, registered with a render error, and registered with no frame yet. The desktop's three placeholders are drawn from `PluginsHostView` (`wire/mod.rs:717`: presence, capture flags, scrubbed render errors), which is already framed; only the live cells need the host.
  - The seam that keeps a plugin rendering for a host that is not the terminal exists: `watched_plugin_screens` (`sections.rs:184-187`, "Plugin screens a host other than the terminal wants kept live ... A watch lapses unless renewed within `PLUGIN_SCREEN_WATCH_LEASE`"), and the intents `plugin.owned.watch_screen` and `plugin.owned.action` (`ainb-app/src/app/plugin_action.rs:15`, `:20`, builders `:28`, `:45`). Routing of keys on a plugin screen is host-neutral policy in `ainb-app/src/app/screens/builtin.rs`.
  - What may not cross: `plugin_ui_states` and `watched_plugin_screens` stay off the wire (`wire/mod.rs:713-716`, `sections.rs:170-187`, `state_serde.rs:141` deny-list, `plugin_actions.rs`). A painted cell buffer is not `ui.state`: it is what the terminal shows, and it crosses the way the terminal's bytes do, on a channel of its own, never on a section. That is the amendment's claim, argued below.

  The parity suite, which is this node's gate.
  - `ainb-app/tests/parity.rs:18-22` builds every committed fixture (`tests/parity/*.json`, twelve today: `config`, `daemons`, `git_view`, `home`, `log_history`, `new_session_pick_repo`, `onboarding`, `session_list`, `session_list_help`, `session_recovery`, `setup_menu`, `skill_manager`) with a floor of twelve, and `ainb-core/tests/parity_snapshots.rs:47-62` draws each through a ratatui `TestBackend` against its `.snap`. D3a (#1217) adds `tests/parity_frames.rs`, which dumps every section of every fixture to `tests/parity/frames/<name>.json` for the DOM half; D3b lands the DOM runner, the expected-facts list beside each fixture and the mutation check; D3 raises the floor to fourteen. This node adds `inbox` and `plugins_host` to both halves and raises the floor to sixteen, and adds `stats` to the DOM half with the reason it has no ratatui half (below).

─ WHAT D3-PRIME MAY NOT REOPEN ─

· Locked decisions D10 to D18 (`docs/plans/2026-09-11-multi-surface-decisions-spec.md:53-61`) stand. In particular:
  - **D14**, one status truth: nothing this node frames is a second projection of a status a section already carries. The inbox rows are notifications, not status; the usage numbers are counters, not status; neither adds a card, a tier or a wait kind beside `agent_status.view.cards[]`, and the stats tab draws no per-agent state at all.
  - **D15**, root selectors return scalars: the header shows an unread count, the inbox screen reads the rows. The stats tab's header figure, if any, is one scalar.
  - **D18**, the mutation envelope: the mark-read write carries a client-minted opaque 128-bit op id (`OpId::from_bytes`, `mutation.rs:181`) and the fence the registry assigns it, which is `Fk::None` (`mutation.rs:1190`); this node does not invent a fence the registry does not have, and does not ship a tenth `MutationEnvelope::default()`.
  - **D17**: `fleet.usage.read` is a capability, so a daemon without it answers the section `absent` with the refusal as the reason, the way `agent_status` does (`sections.rs:965`). No protocol integer moves.
· A plugin's `ui.state` does not ride a frame (`wire/mod.rs:713-716`). The fallback cell paints cells over a channel that is not a section; the amendment says so and says what proves it, and there is no silent exception.
· `ainb-app` owns no plugin runtime (`host_side_effects.rs`). The desktop's plugin host is owned by the Tauri shell, a process boundary outside `ainb-app`, and the executor that delivers to it is the desktop's (`executor.rs`), not a module of `ainb-app`.
· `ainb-tui/crates/ainb-core/src/app/*` is untouched. The TUI inbox screen registers from `components/layout.rs` through the public `ScreenRegistry::register`, as recorded above, so no exception is asked for.
· The inbox source is the daemon's `hangar/inbox_list`, not notifyd's database (`spec:223`). Replacing notifyd's ingest with the daemon's attention table (`spec:396`) is a later migration and is not opened here.
· The section set is not renumbered: `inbox` keeps its place at `SectionId::Inbox`, and `usage` is appended after `agent_status` as section 21, the way section 20 was appended.
· A row that writes outside ainb runs only from its key (`keymap.rs:1581`, `key_only` derived from `KeyAction::writes_outside_ainb`); no row this node adds does.

─ THE SEAMS D3-PRIME HAS TO OPEN ─

```
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 1 inbox section frames nothing │──▶│ fields on InboxSection, filled   │
│   wire/mod.rs:159, :711        │   │ by a host-owned reader, bounded  │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 2 no client mints an op id for │──▶│ the executor mints one per       │
│   inbox_mark_read (plugin.rs)  │   │ effect and retries with the same │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 3 no inbox screen on either    │──▶│ TUI screen over state.inbox,     │
│   surface (62dedf28e)          │   │ desktop screen over the frame    │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 4 usage numbers reach no       │──▶│ section 21 usage, filled from    │
│   section (spec:213)           │   │ fleet/usage_summary, one producer│
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 5 desktop runs no plugin       │──▶│ shell-owned RuntimeHandle, cells │
│   runtime, executor.rs:29      │   │ on a Tauri channel, not a frame  │
└────────────────────────────────┘   └──────────────────────────────────┘
┌────────────────────────────────┐   ┌──────────────────────────────────┐
│ 6 a surface meeting fields on  │──▶│ answer D3's :217 for this case:  │
│   a section it knew as empty   │   │ an older reader ignores new keys │
└────────────────────────────────┘   └──────────────────────────────────┘
```

The second seam is the one the inbox cannot skip. The daemon's ledger guard only engages when the caller sent an op id (`rpc/mod.rs:1341-1342`); without one, a retry after a lost reply runs the sweep again, which is harmless for this method (it is idempotent) and is exactly the habit that is not harmless for the next one. So the op id is minted once per effect by the executor that sends it, the retry sends the same id and the same body, and a test proves the second send is answered from the ledger (`replayed`), not by the handler. The sixth seam is the cheap one: an older webview or TUI reading `inbox` sees keys it did not have, and serde's default for unknown keys is to ignore them; the PR says so and a test shows an `InboxView` of D3a's shape decoding a D3p-a frame.

─ SCOPE, STAGED AS PRs ─

Seven PRs, the inbox first as four of its own, then the amendment for the other two, then each of them. Each is mergeable alone, targets `v2`, and carries its own proof. The inbox needs no amendment: `spec:223` is its contract already. The stats tab and the fallback cell each rest on a claim the spec does not yet make, so their amendment goes before them, as D3·spec did.

**D3p-a, the inbox section and its reader.**
· Touches: `ainb-tui/crates/ainb-app/src/app/sections.rs`, `src/wire/mod.rs`, `src/wire/fields.rs` only if a new scrubber is needed, `src/fleet/inbox_reader.rs` (new, beside `agent_status_reader.rs` once #1215 lands), `src/fleet/mod.rs`, `src/app/effect.rs` for the mark-read effect, `src/app/intent.rs` or the renderer-intent list for `inbox.mark_read`, `tests/fixtures/section_key_paths.txt`, `bindings/AppState.ts`, `ainb-tui/crates/ainb-core/src/host.rs` and `ainb-tui/crates/ainb-desktop/src/host.rs` for starting and draining the reader, `ainb-desktop/src/executor.rs` and the terminal's executor for the write.
· Builds: `InboxSection { entries: Vec<InboxEntryRow>, unread: i64, recipient: String, absent: Option<String>, head_revision: i64, rows_cut: usize }`, bounded to `MAX_INBOX_ROWS` (200, since the daemon returns every row newest-first and a fold that trusts the count is #1194 again), with `rows_cut` saying what was dropped; `InboxView` framing each field with its scrubber or its allow-list reason (`summary` through `scrub_str`, `fields.rs:168`; ids, enums and timestamps allow-listed with the reason in the triage); the reader, one read in flight, a subscription-shaped lifecycle if #1215's is on `v2`, else a poll on the tick cadence `agent_status` uses, folded on the host tick; the mark-read effect carrying a minted `OpId`, sent as `InboxScopedParams { workspace_id: "default", recipient: Some("member:me"), mutation }`, its reply folded into `unread` through the same drain as a read. The reducer flips nothing optimistically: `unread` is the daemon's number or `absent`.
· Proof: `ainb-app` tests that a list past the bound frames inside `MAX_FRAME_BYTES` and says what it cut; that a credential-shaped `summary` assembled at runtime does not survive the frame while the redaction marker draws; that a `MutationEnvelope::default()` cannot reach the wire from the effect (the type the executor sends has no default constructor for the op id); a daemon-side test in `ainb-hangar-daemon/tests/rpc_inbox.rs` that two sends of the same op id and body run the sweep once and the second is `replayed`; the key-path fixture regenerated with `UPDATE_SECTION_KEY_PATHS=1` in the same commit as each field, after triage; a test that an `InboxView` of the previous shape decodes the new frame (seam 6).
· Gate: `cargo test -p ainb-app`, `cargo test -p ainb-hangar-daemon` for `rpc_inbox`, the excluded workspace's own `cargo test`, "TypeScript bindings freshness", "Contracts".

**D3p-b, the TUI inbox screen.**
· Touches: `ainb-tui/crates/ainb-core/src/components/inbox.rs` (new), `src/components/mod.rs`, `src/components/layout.rs` (the register line and the home hint), `ainb-tui/crates/ainb-app/src/app/screens/mod.rs` (`ids::INBOX`), `src/app/keymap_defaults.rs`, `src/app/keymap.rs` if a `UiAction` is needed, `ainb-core/tests/fixtures/keymap_rows.txt`, `docs/tui/keyboard-shortcuts.md`, `ainb-core/tests/tripwire_inbox_opens_and_renders.rs` (new, in the shape the deleted one had, over the section rather than notifyd).
· Builds: the screen over `state.inbox`: rows newest-first with kind, age and summary, unread rows marked, the unread badge on the home hint, `r` sending the mark-read intent, `Enter` a no-op until a deep link exists (the row carries `subject_id`, and where it goes is a follow-up, not a stub). Registered from `layout.rs` after `:313`, so `ainb-core/src/app/*` is not touched.
· Proof: the tripwire opens the screen and reads the rows the fixture carries; `keymap_parity.rs` green with the regenerated rows; `cargo test -p ainb-core`.
· Gate: as D3p-a plus the tripwires and "CLI reference freshness".

**D3p-c, the desktop inbox screen.**
· Touches: `ainb-tui/crates/ainb-desktop/ui/src/inbox.ts`, `inbox.tsx`, `inbox.test.ts`, `subscription.ts`, `selectors.ts`, `main.tsx`, `shell.css`; `ainb-desktop/src/intent.rs` only if `inbox.mark_read` needs a host-authored row.
· Builds: the inbox as a surface beside the board, rows from the frame, unread count as a scalar root selector in the header, a mark-read control that sends the `RendererIntent` and nothing else; the `absent` reason drawn when the daemon has no inbox; the `rows_cut` count drawn when the bound was hit.
· Proof: `ui/` tests for the projection (rows, unread mark, the count as a scalar, the absent state, the cut count); `subscription.test.ts` green with `inbox` subscribed and read; `tsc --noEmit --strict` clean.
· Gate: as D3p-a plus the frontend tests.

**D3p-d, the parity fixtures with the mutation check, the journey and the proof.**
· Touches: `ainb-tui/crates/ainb-app/tests/parity/inbox.json`, `inbox.snap`, `frames/inbox.json`, the expected-facts file in the shape D3b landed, `tests/parity/support.rs`, `tests/parity.rs` (floor fourteen to fifteen), the DOM runner's fixture list, `ainb-desktop/e2e/specs/inbox.e2e.js` (new, beside `journey.e2e.js` and `answer.e2e.js`), `ainb-tui/scripts/proof/scenarios/d3p-inbox.sh` (new, in the shape of `d2-board.sh`, `skip` when `xvfb-run` is missing, `:47-50`), `scripts/proof/run.sh` `ALL_NODES` (`:86-92`), `.github/workflows/desktop.yml`.
· Builds: one `inbox` fixture with three rows, one read, one unread over the bound of nothing (so the cut count is zero and drawn as such), drawn by both renderers against one expected-facts list; the mutation check re-proved on this fixture (one fact deleted from one renderer's output, the suite fails, the failing output on the PR); the wdio journey: the inbox opens, the rows drawn carry the daemon's row ids, mark-read sends one op id and the unread count reaches zero from the daemon's reply, not from the webview; the proof scenario writing `result.json` with `pass: true` and observed lines naming the sections applied, the rows drawn, the op id sent and the ledger row it left.
· Gate: the two halves green, the journey green on both runners, the scenario in `ALL_NODES` with a `skip` on a box with no headless X.

**D3p·spec, the amendment for the stats tab and the fallback cell (docs only).**
· Touches: `docs/plans/2026-09-04-desktop-shared-core-spec.md`.
· Says three things, one paragraph each. First, the stats tab reads `fleet/usage_summary`, and why that satisfies D14 rather than bending it: the counters have one producer (`fleet_usage.rs`), the section is a fold of that producer's reply and computes nothing, the desktop runs no scan of the provider logs, and the numbers are usage, not status, so no card, tier or wait kind is duplicated. The TUI keeps burndown's plugin paint on `analytics` and gains no second stats screen, and the fact that burndown's data comes from a second scan of the same logs (`session-reader`, #391) is recorded as the drift that already exists, with a follow-up to move burndown onto the daemon's verb, not fixed here. The numbers become section 21 `usage`, appended, bounded by the verb's own caps, `absent` when the capability is refused. Second, the fallback cell: the Tauri shell owns an `ainb_plugin_runtime::RuntimeHandle` (the crate the TUI uses, `ainb-app/src/plugins.rs`), brought up on the first `plugin.owned.watch_screen` and torn down with the window; painted `WireBuffer` cells cross to the webview on a Tauri `Channel` of their own, the D1c terminal pattern, never on a section, never persisted, never off-box, which is the class the terminal's bytes are in already and is why no key-path fixture claims them; the desktop's executor delivers `ForwardToPlugin` and `RunPluginAction` to that handle, `ainb-app` still owns nothing; the three placeholders are drawn from `PluginsHostView`, which is framed and scrubbed already. Third, the parity gate for these two: `plugins_host` is a fixture on both halves (the placeholder states exist on both surfaces); `stats` is a DOM-half fixture over the framed `usage` section, because the TUI has no built-in stats screen and giving it one beside burndown's would be the drift the section set stops.
· Gate: docs jobs only. Merged before D3p-e opens.

**D3p-e, the stats tab.**
· Touches: `ainb-tui/crates/ainb-app/src/app/sections.rs` (`UsageSection`), `src/wire/mod.rs` (`SectionId::Usage`, `UsageView`, the three arms), `src/fleet/usage_reader.rs` (new), `tests/fixtures/section_key_paths.txt`, `bindings/AppState.ts`, `ainb-core/src/host.rs` and `ainb-desktop/src/host.rs` (start and drain), `ainb-desktop/ui/src/stats.ts`, `stats.tsx`, `stats.test.ts`, `subscription.ts`, `main.tsx`, `shell.css`, `tests/parity/stats.json` and its DOM-half facts.
· Builds: the section as a fold of `FleetUsageSummaryResult` (state, generated_at, totals, daily, providers, models, projects, detail) with `detail` through `scrub_str` since it is the one free-text field and the daemon already bounds it at 1,024 bytes; the reader reading `fleet/usage_summary` for `trailing_30_days` on the cadence the daemon refreshes at (fifteen minutes, `fleet_usage.rs:27`, so reading faster returns the same bytes); `absent` carrying the capability refusal when `fleet.usage.read` is not served; the desktop stats tab over the section: totals, a thirty-day strip from `daily`, the three breakdowns, `scanning` and `partial` drawn as states rather than as zeros ("Clients must show tokens instead of synthesising a zero cost", `fleet.rs:704-705`).
· Proof: `ainb-app` tests that the section frames inside `MAX_FRAME_BYTES` at the verb's caps; that `detail` is scrubbed; that a refused capability lands as `absent` with the reason; `ui/` tests for the projection and the four states; the DOM-half fixture; the key paths regenerated per field.
· Gate: as D3p-a plus the frontend tests.

**D3p-f, the plugin fallback cell, with the programme row folded in.**
· Touches: `ainb-tui/crates/ainb-desktop/src/plugins.rs` (new: the shell-owned runtime, its lifecycle, the cell channel), `src/executor.rs` (`ForwardToPlugin` and `RunPluginAction` delivered when a runtime is up, undelivered with the same reason when it is not), `src/host.rs`, `src/main.rs`, `src/lib.rs`, `tests/plugins.rs` (new), `ui/src/plugin_cell.ts`, `plugin_cell.tsx`, `plugin_cell.test.ts`, `main.tsx`, `shell.css`, `tauri.conf.json` and the xtask stage step for the bundled plugin binaries, `tests/parity/plugins_host.json` and `.snap` and its facts, `tests/parity.rs` (floor fifteen to sixteen), `docs/plans/2026-09-12-desktop-programme.md` (`:135` flipped, `:60` graph fixed).
· Builds: the runtime brought up on the first `plugin.owned.watch_screen` for a screen in `PLUGIN_SCREENS`, renewed on the lease the section already keeps, stopped with the window; cells drained with `try_recv_render` on the shell's tick and sent on a `Channel` as the terminal's bytes are, painted in the webview into a cell grid (the existing xterm.js surface fed the buffer as escape sequences, if a `ui/` test shows it paints the TUI's placeholder text identically; else a plain grid, decided on the PR); the three placeholders from `PluginsHostView` when there is no runtime, no plugin, or no frame yet; keys and clicks on the cell sent as `plugin.owned.action` through the host-neutral routing.
· Proof: a desktop test that with no runtime the executor still answers undelivered with `NO_PLUGIN_RUNTIME`'s reason and `host_side_effects.rs` is untouched; a test that a watch lapses when not renewed and the runtime stops rendering that screen; a `ui/` test that a buffer paints the cells it was given and nothing else; the `plugins_host` fixture on both halves; the bundle smoke asserting the plugin binaries are staged.
· Gate: the success criteria below, all green.

─ WHAT D3-PRIME DEFERS, WITH ITS COST ─

· **The hangar `ui.state` desktop component.** The D2 amendment (`spec:211`) left it blocked behind the two facts this node resolves for the fallback cell, and it stays blocked: the cell paints cells, it does not read the plugin's JSON, so the component that would draw the hangar plugin's own view as native widgets still has no safe path across the wire. Cost to record: the amendment that says how a plugin view is proved free of secrets, which no node has written.
· **Retiring the hangar plugin's inbox sub-screen.** Two paints of one daemon list on the TUI once D3p-b lands. Cost: a plugin change plus the `ACTIONS` contract at `ui_view.rs:29`, owned by the plugin, not by a surface node.
· **Burndown onto `fleet/usage_summary`.** The second scan of the provider logs (#391, #393) is the drift the stats amendment records. Cost: a plugin rewrite of its data path, and the answer to whether a subprocess plugin may call the daemon at all.

─ WHICH EXISTING TESTS MUST STAY GREEN ─

Unchanged on every PR of this node, or changed only in their own commit with the reason in the message:
· `ainb-app/tests/host_side_effects.rs`, including `no_module_in_the_crate_owns_the_plugin_runtime`. The fallback cell adds no module to `ainb-app`.
· `ainb-app/tests/serialize_guard.rs` with `tests/fixtures/serialize_call_sites.txt`.
· `ainb-app/tests/state_serde.rs` with `tests/fixtures/section_key_paths.txt`, regenerated with `UPDATE_SECTION_KEY_PATHS=1` in the same commit as each new field, after triage against the deny-lists (`:141`, `:664`).
· `ainb-app/tests/bindings.rs` and the CI job "TypeScript bindings freshness"; `bindings/AppState.ts` regenerated in the same PR.
· `ainb-app/tests/parity.rs`, `tests/parity_frames.rs` (D3a) and `ainb-core/tests/parity_snapshots.rs`: this node adds fixtures, and no existing fixture's expected facts change without its own commit.
· `ainb-app/tests/git_view_bound.rs` and `tests/palette_rows.rs` (D3a), `key_only_commands.rs`, `key_only_completeness.rs`, `command_gate.rs`, `review_commands.rs`, `pointer_commands.rs`, `intent_dispatch.rs`, `plugin_actions.rs`, `renderer_free.rs`, `persistence.rs`, `effects.rs`.
· `ainb-hangar-daemon/tests/rpc_inbox.rs` and the mutation registry's two walks (the proto embed test and the daemon replay test, `mutation.rs:458-460`).
· `ainb-core/tests/keymap_parity.rs` with `tests/fixtures/keymap_rows.txt`, and `docs/tui/keyboard-shortcuts.md` regenerated by `ainb-tui/scripts/gen-keymap-docs.sh` on any branch that adds a bound row.
· `ainb-desktop/tests/`: `host_contract.rs`, `sidecar.rs`, `terminal.rs`, `shell.rs`, `intent.rs`, `workspace_load.rs`, `bindings.rs`, `palette_golden.rs` (D3a), `agent_status_surface.rs` (#1215).
· `ainb-desktop/ui/src/*.test.ts`, including `subscription.test.ts`.
· The jobs of `.github/workflows/desktop.yml`, the workspace jobs of `ci.yml`, and a full `bash ainb-tui/scripts/proof/run.sh` reporting every other node passing, one more after this node.

─ SUCCESS CRITERIA (ALL MUST BE TRUE) ─

1. **The inbox exists on both surfaces over one section.** `InboxSection` carries bounded rows the host read from `hangar/inbox_list`, the TUI screen and the desktop screen both draw from it and from nothing else, and a fixture `inbox` is rendered by both halves against one expected-facts list. The floor in `parity.rs` rises to fifteen with it, and the mutation check is re-proved on it: with one fact deleted from one renderer's output the suite fails, and the failing output is on the PR.
2. **Mark-read is a D18 mutation end to end.** The effect carries a client-minted op id, a retry sends the same op id and body, a daemon test proves the second send is answered `replayed` from the ledger with the sweep run once, and no `MutationEnvelope::default()` is added anywhere in this node (`git grep` count on the PR equals the count on `v2`).
3. **Every field this node adds to the wire is provably scrubbed and provably in sync**: `section_key_paths.txt` names each new leaf, regenerated in the commit that added it; `summary` and `detail` are scrubbed and a test proves a credential-shaped string assembled at runtime does not survive either while the marker draws; `bindings/AppState.ts` is fresh and `tsc --noEmit --strict` is clean.
4. **The wdio inbox journey is green in CI**, on `ubuntu-latest` under `xvfb-run` and on `macos-latest` with the leg `MACOS_E2E_LEG` declares: the inbox opens, the rows drawn carry the daemon's row ids (proved by a `ui/` test that none was minted in TypeScript), mark-read sends one op id, and the unread count reaches zero from the daemon's reply.
5. **The proof scenario `d3p-inbox` passes on a `v2` build**, in `ALL_NODES`, writing `result.json` with `pass: true`, its observed lines carrying the sections applied, the rows drawn, the op id sent and the ledger row it left, with a `skip` rather than a failure where there is no headless X server.
6. **The stats tab is a fold of the daemon's projection.** Section 21 `usage` is filled from `fleet/usage_summary` and computes nothing; a refused `fleet.usage.read` lands as `absent` with the reason; the four states draw as states; the DOM-half fixture `stats` is green; and the amendment that argues D14 is merged before the tab opens.
7. **The fallback cell paints a runtime the shell owns.** A plugin screen watched from the window renders live cells over a channel that is not a section, the three placeholders draw without a runtime, `ainb-app` still owns no runtime, and `plugins_host` is a fixture on both halves, raising the floor to sixteen.
8. **The programme row flips** with the run ids and the graph at `:60` draws D3-prime between D3 and D4-prime.
9. Final deliverable runs without errors, with the test output and the CI run ids on each PR.

The softer ones, measured and recorded rather than gated:
· How many inbox rows a live daemon returns for the default workspace, and whether the bound of 200 was ever hit.
· The bytes the `usage` section frames at the verb's caps.
· The time from the first `watch_screen` to the first painted cell, and the runtime's memory with one plugin up.
· The number of sections an inbox-and-stats drain applies, from `renderer_applied`, so D4-prime knows what the last surfaces cost.

─ CONSTRAINTS ─

· PRs target `v2`, never `main`. Staged as the seven above, each reviewable alone.
· Every file change is its own signed commit: `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`. Never `git add -A`, never `git add .`; stage by named path.
· Conventional commit messages written as a human developer. No attribution trailers, no `Co-Authored-By`, no "Generated with", and no mention of Claude, Anthropic, an AI or an assistant in any commit message, PR title, PR body or comment.
· No em dashes on any line you author, in code, docs, commit messages or PR bodies.
· Never name another product as inspiration.
· Tests first: the test that pins a behaviour is committed before the code that makes it pass.
· Before every push: `cargo fmt --all -- --check` from `ainb-tui/`, `cargo test --no-run` on every touched crate, and in `ainb-tui/crates/ainb-desktop/ui`, `npm test` and `npx tsc --noEmit`. Build with `-j 3` and `CARGO_INCREMENTAL=0`.
· Draft PRs only. Never merge, never flip a PR ready, never close an issue, never poll CI. If a decision is needed, ask it as a PR or issue comment with a recommendation and keep working on everything that does not depend on it.
· Never touch `ainb-tui/crates/ainb-core/src/app/*`. Merge `origin/v2` before touching a file another lane is on, and name shared files in the PR body. Files this node shares with D3: `wire/mod.rs`, `sections.rs`, `section_key_paths.txt`, `bindings/AppState.ts`, `subscription.ts`, `main.tsx`, `parity.rs`, `parity/support.rs`, `desktop.yml`, `run.sh`.
· No plugin JSON on a frame. The cell channel carries painted cells, and the amendment is merged before the cell opens.
· Tauri, SolidJS 1.9.15, TypeScript 5.6.3 and Node 22 stay pinned, every frontend dependency in a committed lockfile, CI installing with `npm ci`. This node adds no frontend dependency.
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
| D3 goal `:217` | what a surface does when it meets a section set it does not know | D3p-a | this node is the first to add fields to a section a shipped reader knows as empty; the answer for that case (unknown keys ignored, proved by a test) is recorded on the PR, and the general question is filed if D3 has not filed it |
| #1194 | the board vec is documented as bounded but nothing caps it on deserialize | D3p-a, as a pattern | the inbox fold caps on deserialize with `rows_cut`, so the lesson is applied rather than the issue closed |
| #1188 | one agent status reader for the terminal and the desktop | **not this node**, #1215 | the inbox reader copies #1215's shape; if #1215 has not merged when D3p-a opens, D3p-a ships the same shape and says so |
| #1049 | Claude sessions never learn `provider_session_id` | **not this node** | a daemon and hook identity fix; no inbox row depends on it |
| #391, #393 | the session-reader scan cost | **not this node** | recorded in the stats amendment as the drift that exists; burndown onto the daemon's verb is filed |

─ FOLLOW-UPS TO FILE, NOT TO SOLVE ─

File each as an issue with its evidence. Do not fix it in this node.
· The hangar plugin's inbox sub-screen (`ui_view.rs:42`, `plugin.rs:1769`) beside the built-in inbox screen: two paints of one list on the TUI.
· The nine `MutationEnvelope::default()` sends in `ainb-app` and the two in the hangar plugin, listed by path and line, as the envelope-less writes the programme carries; one issue, one list.
· Burndown reading the provider logs through `session-reader` while the daemon reads them through `fleet_usage.rs`: one corpus, two scanners.
· A deep link from an inbox row's `subject_id` to the issue, comment or task it names, on both surfaces.
· A workspace picker: the inbox and the stats tab read the daemon's default workspace, and a multi-workspace daemon shows one.
· The board's turn timeline (`spec:213`): the stats tab lands the counters; the per-card timeline still has no section and no verb.
· Whether a subprocess plugin may call the daemon's verbs itself, which is what moving burndown onto `fleet/usage_summary` needs.
· The desktop's runtime with more than one plugin up: memory, and whether a frame cache keyed by `(screen, viewport)` (D3 goal `:214`) is needed once two hosts render one plugin.

─ OPEN QUESTIONS THE LANE ANSWERS ON A PR BODY ─

· **Which workspace and recipient the inbox read names.** Recommended: the daemon's default workspace (`DEFAULT_WORKSPACE_ID`, `connection.rs:33`) and `member:me`, exactly what the hangar plugin sends today (`plugin.rs:337-339`), so the built-in screen and the plugin's show the same rows until the plugin's is retired. Answer on D3p-a.
· **Poll or subscribe for the inbox.** The daemon has no `hangar/inbox_subscribe`; `attention/subscribe` exists for a different list. Recommended: a read on the `agent_status` cadence, in #1215's reader shape so a subscription can replace it without moving the fold. A daemon subscription verb is filed, not written here. Answer on D3p-a.
· **Where the op id is minted.** Recommended: in the executor, once per effect, held on the retry, because the reducer must stay replayable and the retry lives where the socket does. The reducer never sees the id; the reply's `unread` is what it folds. Answer on D3p-a with the test that proves a replay is `replayed`.
· **Whether the stats tab is a section or a read the screen makes.** Recommended: section 21, because D15's contract is that renderers read sections and a screen-made read is a second channel the DOM half of parity cannot render from. Answer on D3p·spec.
· **How cells reach the webview.** Recommended: a Tauri `Channel` per watched screen carrying the painted buffer, the D1c pattern, with the webview painting into the existing xterm.js surface if a test shows the placeholders paint identically, else a plain cell grid. Answer on D3p-f with the measured time to first cell.
· **What "full parity suite" admits for a screen that exists on one surface.** Recommended: a DOM-half fixture with the reason recorded in the amendment, never a stubbed ratatui half. Answer on D3p·spec.

─ FINAL DELIVERABLE ─

Confirmation each criterion is satisfied. Every file created or modified. How to run, test and deploy. Proof (screenshot, test output, URL). Decisions made and anything to know. Known limitations and follow-ups.

Begin by outputting your plan. Then execute end-to-end without checking in until done or genuinely blocked.

─ PROGRESS LOG ─

Plan, staged as the seven PRs:
1. D3p-a: the inbox section, its reader, the mark-read effect with a minted op id.
2. D3p-b: the TUI inbox screen, registered from `layout.rs`.
3. D3p-c: the desktop inbox screen.
4. D3p-d: the `inbox` parity fixture with the mutation check re-proved, the wdio inbox journey, the `d3p-inbox` proof scenario.
5. D3p·spec: the amendment arguing the stats tab under D14 and the fallback cell's host and channel.
6. D3p-e: section 21 `usage` and the stats tab.
7. D3p-f: the shell-owned plugin host and the fallback cell, with the programme row and graph.

2026-09-19: goal written on `docs/d3-prime-goal` against `origin/v2` at `175a421ff`. Two facts found while verifying anchors overturn the D3 goal's framing and are recorded above: `InboxScopedParams` already embeds the D18 envelope and the daemon's ledger guard is generic at dispatch, so the missing half is a client that mints an op id; and `fleet/usage_summary` already exists as a bounded, capability-gated daemon read, so the stats tab is a fold of it and not a verb this node writes. The anchors the D3 goal gave for the inbox verbs (`methods.rs:1173`, `:1192`) had moved to `:1191` and `:1210`. Waiting on the orchestrator's approval of this goal before any code PR opens, and on D3 merging on `v2` as the gate to start.
