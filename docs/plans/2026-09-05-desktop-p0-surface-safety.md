# Desktop shared core, first slice: P0 keymap-as-data + Phase S surface safety

## Overview

Ship the two TUI/daemon-only phases that unblock the `ainb-app` extraction and let the TUI, a future desktop, and `ainb web` run together without clashing: keymap-as-data with generated docs, versioned state sections, the scroll seal, and the surface-safety fixes from the concurrency audit. No desktop crate yet.

Spec: `docs/plans/2026-09-04-desktop-shared-core-spec.md` (phases P0 and S). Research: `research/2026-09-04_14-10-02_desktop-app-shared-core.md`, `research/2026-09-04_16-25-00_tauri-agent-apps-prior-art.md`.

## Current State Analysis

- `crates/ainb-core/src/app/events.rs:1473` `handle_key_event` is one 860-line dispatcher plus 20 per-screen handler fns (`:2334-3507`): 66 `match key_event.code` blocks, 520 `KeyCode::` occurrences, 491 extracted binding rows (aliases like `up|k` counted once). 254 map to a plain `AppEvent`, 231 are guarded by a modal flag or sub-step, 10 mutate state inline, 8 are dead arms, 2 delegate to `components/new_session/configure.rs:2065 handle_key` and `pick_repo.rs:737 handle_key` (called at `events.rs:2365,2392`). Regenerate the row table live at implementation time; the counts here are scoping only.
- The render path mutates state every frame: `Screen::render` takes `&mut AppState` (`app/screens/mod.rs:85`) for all ~17 impls in `screens/builtin.rs` (`:635-1065`); `layout.rs:199` writes `menu_bar_area`, `layout.rs:184` and `builtin.rs:828-831` write `embed_pane_area`, and `state.rs:4032 statusline_status_cached(&mut self)` touches the status cache on every status-bar draw. Any version-on-mutation scheme is meaningless until these leave `AppState`, so UiState (Phase 3) lands before versioned sections (Phase 2).
- Three interceptors run before the dispatcher and never consult it: interactive embed pre-empts every key at `main.rs:607-623` (only `ctrl+q` escapes, `:591-598`); preview scroll mode steals `esc up k down j pageup pagedown` at `main.rs:675-700`; `:` opens the slash palette at `main.rs:648`. Plugin screens reserve only `ctrl+c` and `?`/`shift+H` (`screens/builtin.rs:183`).
- `AppEvent` (`events.rs:24-626`) has 410 variants; 102 are nav/scroll shaped; 12 have no binding at all.
- `AppState` (`state.rs:3162-3521`) has 106 fields, `#[derive(Debug)]` only, `Default` is the sole constructor (`:3821-3992`). Fields group into 19 sections; 11 fields are channels, handles, or SQLite connections; 3 are `Rect`-typed or geometry maps. `main.rs:704-760` mutates the ratatui `layout` for 8 scroll events instead of going through `process_event` (`events.rs:3922`).
- `docs/tui/keyboard-shortcuts.md` is hand-written prose; 10 documented bindings do not exist and 10 real ones are undocumented. `docs/tui/cli.md` already has a generator plus a CI freshness gate (`.github/workflows/ci.yml:138-168`, `scripts/gen-cli-reference.sh`) to copy.
- Concurrency audit (17 hazards): daemon singleton exists (`ainb-hangar-daemon/src/single_instance.rs:75`), double-answer guard exists (`hangar-store/src/repo/attention.rs:344`, `daemon/src/answer.rs:71,116,160`), notifyd has a `StartupLock` (`plugin-notifyd/src/listener.rs:105-135`). Real gaps: `config.toml` read-modify-write with two unlocked writers (`config/mod.rs:2029`, `plugin-burndown/src/config.rs:169`); three in-place `fs::write` JSON stores; headroom proxy guard is a process-local `tokio::sync::Mutex` with a fixed port 8787 (`headroom/mod.rs:32,100-155`); MCP pool probe-then-unlink-then-bind (`mcp_pool/proxy.rs:94`, `paths.rs::socket_alive_or_cleanup`); `usage_cache` opens WAL with no `busy_timeout` (`usage_cache/db.rs:36-55`); nothing pins tmux `window-size`; the daemon has no per-connection identity (`HelloParams { token }` only, `hangar-proto/src/auth.rs:44`; `Caller` is `Operator | Copilot { scope_key }`, `daemon/src/rpc/auth.rs:53`).
- `fs2` is a workspace dep already used by `ainb-core`, `ainb-fleet-core`, `ainb-skill-core` (`skill-core/src/sync.rs:466` is the `try_lock_exclusive` pattern to copy). `write_atomic` (temp + rename + mode carry) exists at `config/mod.rs:1307`, `pub(crate)`.

## Desired End State

- One `Keymap` table in `crates/ainb-core/src/app/keymap.rs` resolves every key on every screen and modal, the three `main.rs` interceptors included; `~/.agents-in-a-box/keymap.toml` overrides it; `docs/tui/keyboard-shortcuts.md` is generated from it and CI fails on drift.
- `AppState` fields live in 19 `Versioned<Section>` structs; any `&mut` access bumps the section version; `App::changed_since()` reports bumped sections.
- Scroll offsets, Rects, and mouse hit-testing live in a ratatui-host `UiState`; the 9 scroll `AppEvent` variants and 12 dead variants are gone; `main.rs` mutates nothing behind the reducer.
- Two `ainb` processes, or `ainb` plus `ainb web`, can save config, spawn the headroom proxy, start the MCP pool, and answer the same attention item without data loss; the daemon knows which surfaces are connected.
- All 101 tripwires green after every phase; new behavioural tests for the keymap, versions, and each lock.

### Key Discoveries:
- Key-to-intent and intent-to-state are already separate layers (`handle_key_event` returns `Option<AppEvent>`, `process_event` is the single reducer). Phase 1 replaces only the first layer.
- Context must be richer than `ScreenId`: 12 modal guards in the dispatcher (`confirm-dialog`, `mcp-overlay`, three rename modes, context menu, help swallow, quick-commit, six skill-manager overlays) plus sub-steps (`NewSessionStep`, `OnboardingStep`, `FocusedPane`). The extracted table already carries these as `context[guard]` strings.
- List selection indices (`selected_session_index` and the 93 per-screen cursor variants) are the arguments commands act on (`sessions.attach` reads the selection), so they stay in core as flow state. Only scroll offsets, pane focus, Rects, hover, and `needs_redraw` move to `UiState`. The spec's transient-state table is amended to say so.
- `PickRepo` branch outcomes (`events.rs`, six variants) mutate `AppState`, read the clipboard, write session defaults, and reassign `current_screen` with no `AppEvent`. Phase 1 keeps them behind one `AppEvent::PickRepoOutcome(outcome)` so the table stays total.
- `Versioned<T>` cannot derive `Serialize` yet: 19 component types derive `Debug` only and two hold SQLite connections (`InboxState`, `FleetPanelState`). Phase 2 adds the wrapper and version bumps only; serde arrives with P1.
- `AttentionAnswered { attention_id, by, .. }` already broadcasts (`hangar-proto/src/events.rs:188`); the TUI hangar plugin sends `answered_by = "tui"` (`plugin-hangar/src/plugin.rs:2227`) and web sends `"web"` (`ainb-web/src/routes.rs:372`). Convention change is a string, not a protocol change.

## What We're NOT Doing

- No `ainb-app` crate yet (P1), no serde or specta on `AppState` (P1/D1), no desktop crate (D1).
- Not moving list selection out of core.
- Not moving sessions.json into the daemon (P6).
- Not touching `apps/ainb-fleet-macos`.
- Not consolidating plugin subprocesses (audit: safe as is).
- Not adding an input lease for tmux multi-attach (rejected by both design passes).
- Not changing which keys the interactive embed forwards to the PTY; only where that rule is declared.

## Implementation Approach

Phase 1 turns the dispatcher into table lookup with a parity test that runs old and new resolvers over a fixture matrix, so no binding changes by accident. Phase 3 (wave 2) pulls the renderer-only state out into `UiState` and changes `Screen::render` to take `&AppState` plus `&mut UiState`, so drawing can no longer mutate core state and `main.rs` stops mutating layout. Phase 2 (wave 3) then regroups fields into sections one section per commit, using a `Versioned<T>` wrapper whose `DerefMut` bumps the version (over-bumping is harmless; under-bumping is a bug); because the draw path is read-only by then, a bump means a real change. Phase S runs in parallel: every lock reuses `fs2::FileExt::lock_exclusive` (inside `spawn_blocking` when the caller is async), every JSON write reuses `write_atomic`, and the daemon gains a minimal in-memory `ConnectionRegistry`.

---

## Phase 1: Keymap-as-data with generated docs
<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-core/src/app/keymap.rs, ainb-tui/crates/ainb-core/src/app/keymap_defaults.rs, ainb-tui/crates/ainb-core/src/app/keymap_toml.rs, ainb-tui/crates/ainb-core/src/app/events.rs, ainb-tui/crates/ainb-core/src/app/mod.rs, ainb-tui/crates/ainb-core/src/main.rs, ainb-tui/crates/ainb-core/src/cli/keymap.rs, ainb-tui/crates/ainb-core/src/cli/mod.rs, ainb-tui/crates/ainb-core/tests/keymap_parity.rs, ainb-tui/crates/ainb-core/tests/keymap_toml.rs, ainb-tui/crates/ainb-core/tests/fixtures/keymap_rows.txt, ainb-tui/scripts/gen-keymap-docs.sh, docs/tui/keyboard-shortcuts.md, .github/workflows/ci.yml] -->

### Overview
Replace the 66 host-dispatched match blocks with one table, model the three `main.rs` interceptors as top-priority contexts, add TOML overrides, generate the shortcuts doc, gate drift in CI. Scope is every key the host dispatches; the two component-owned handlers (`new_session/configure.rs:2065-2217`, `pick_repo.rs:737-918`, ~330 lines) stay as they are and New Session gets no TOML overrides in this phase (deferred to P2 when those `*State` structs move).

Chord grammar, answering the spec's open question: modifiers are `ctrl`, `alt`, `shift` only in the TUI (terminals never deliver `cmd`/`super`; `alt` covers Option/Meta on macOS and Linux alike); `cmd+` chords are reserved for the desktop renderer and rejected by `Chord::parse` in the TUI with a clear error. Shifted letters are the bare uppercase letter (`G`); `shift` is written only for non-letters (`shift+tab`). Multi-key sequences (`g g`) are parsed but no default uses them; the resolver treats a pending prefix with a 500ms timeout.

### Changes Required:

#### 1. Chord, context, binding types
**File**: `ainb-tui/crates/ainb-core/src/app/keymap.rs` (new)
**Changes**: types, normalisation from crossterm, lookup with context priority.

```rust
/// A normalised key chord: modifiers sorted, key lowercased unless shifted letter.
/// Wire form is the same string the TOML file uses: "ctrl+k", "shift+G", "g g", "f2", "esc".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord(String);

impl Chord {
    pub fn parse(s: &str) -> Result<Self, ChordParseError>;
    pub fn from_key_event(ev: &crossterm::event::KeyEvent) -> Self; // only crossterm touchpoint
    pub fn as_str(&self) -> &str;
}

/// Where a binding applies. Order of variants is priority order when several are active.
/// Not Copy: `ScreenId` is `String` (screens/mod.rs:14); contexts key on the `&'static str`
/// constants in `screens::ids` instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyContext {
    EmbedInteractive,        // main.rs:607-623, everything but ctrl+q goes to the PTY
    PreviewScroll,           // main.rs:675-700
    ConfirmDialog, McpOverlay, SessionRename, OtherTmuxRename, SshRename,
    SessionContextMenu, HelpVisible, QuickCommit,
    SkillManagerOverlay(SkillOverlay),
    ConfigPopup, AuthProviderPopup,
    Screen(&'static str, SubContext),   // SubContext: None | NewSessionStep | OnboardingStep | FocusedPane | Review | Searching ...
    TextInput,               // computed from the existing is_text_input_context predicate, events.rs:1327-1440;
                             // only non-printable base keys (enter, esc, tab, arrows, ctrl+*) resolve here,
                             // every printable char without ctrl/alt is KeyAction::Text
    Global,
}

pub enum KeyAction {
    App(AppEvent),                 // AppEvent: Clone already (events.rs:24)
    Ui(UiAction),                  // Phase 3 fills this; Phase 1 has ScrollPreview/ScrollLogs variants forwarded to the old code path
    Passthrough,                   // EmbedInteractive: write to PTY
    OpenSlashPalette,              // ':' at main.rs:648
}

pub struct Binding { pub ctx: KeyContext, pub chord: Chord, pub action: KeyAction, pub doc: &'static str }

pub struct Keymap { by_ctx: HashMap<KeyContext, HashMap<Chord, KeyAction>> }

impl Keymap {
    pub fn defaults() -> Self;                                   // from keymap_defaults.rs
    pub fn with_overrides(self, o: &KeymapOverrides) -> Self;
    /// Resolve against the contexts currently active, highest priority first.
    pub fn resolve(&self, active: &[KeyContext], chord: &Chord) -> Option<&KeyAction>;
    pub fn bindings(&self) -> impl Iterator<Item = &Binding>;   // for docs + palette
}

/// Computed from AppState each keypress; mirrors the guard order of the old dispatcher.
pub fn active_contexts(state: &AppState, host: &HostFlags) -> Vec<KeyContext>;
```

`HostFlags { embed_interactive: bool, preview_scroll_mode: bool }` is passed in by `main.rs` so the table can own the interceptor rules without `keymap.rs` importing the embed client.

#### 2. Default table
**File**: `ainb-tui/crates/ainb-core/src/app/keymap_defaults.rs` (new)
**Changes**: one `pub fn defaults() -> Vec<Binding>` built from the extracted 491 rows. The row table (`context | chord | event | line`, 22 section headers) is committed in the same PR as `crates/ainb-core/tests/fixtures/keymap_rows.txt`; the parity test reads it, so the source of truth is in git, not a scratch file (`rg 'KeyCode::'` cannot regenerate it: 520 hits with no contexts or guards). Text-input contexts are one `KeyContext::TextInput`, active whenever the existing `is_text_input_context` predicate (`events.rs:1327-1440`: plugin `captures_text`, fleet panel `is_capturing_text()`, config popup, skill-manager browse, git-view commit mode, onboarding auth pane, the rename buffers) is true; the resolver returns `KeyAction::Text(c)` for any printable char without ctrl/alt and consults the table only for non-printable keys. The reducer appends to the active buffer as the old arms did. Chord normalisation: 49 arms match `KeyCode::Char('G')` ignoring modifiers, and crossterm sets `SHIFT` only under the enhanced keyboard protocol, so a shifted letter canonicalises to the single chord `G` whether or not the `SHIFT` bit is set; `shift` is kept only for non-letters (`shift+tab`). The parity matrix runs every letter chord in both modifier forms. Only `KeyEventKind::Press` is dispatched; `Repeat` is treated as `Press`, `Release` is ignored (today's dispatcher has the same effective behaviour on Unix). Aliases `up|k` become two rows. The 10 inline-mutation arms become new `AppEvent` variants (`PickRepoOutcome(PickRepoOutcome)`, and one per remaining inline arm) whose reducer branches are the moved code. The 8 dead arms are dropped. Events carrying payloads (`AttachSessionByIndex(n)` from digits `1..9`) are rows with `KeyAction::App(AppEvent::AttachSessionByIndex(n))` per digit.

#### 3. TOML overrides
**File**: `ainb-tui/crates/ainb-core/src/app/keymap_toml.rs` (new)
**Changes**: `~/.agents-in-a-box/keymap.toml`, shape `[context] event_name = "chord"` where `event_name` is the unit-variant name (`attach`, `quit`, `go_to_git_view`; snake_case of the `AppEvent` variant). Payload-carrying variants are not overridable in this phase. Invalid file: log with line number, apply defaults, surface via `add_warning_notification`. Reject any non-`cmd`/`ctrl+q` chord in `EmbedInteractive` context (terminal owns those keys).

```toml
# ~/.agents-in-a-box/keymap.toml
[session_list]
attach = "o"
quick_shell = "$"          # printable chars are written as the char, never as shift+digit (layout-dependent)
[global]
open_slash_palette = "ctrl+space"
```

#### 4. Dispatcher rewrite
**File**: `ainb-tui/crates/ainb-core/src/app/events.rs`
**Changes**: `handle_key_event` becomes `Chord::from_key_event` → `active_contexts` → `keymap.resolve` → return `KeyAction`. The 20 handler fns are deleted once their rows are in the table; the parity test guards the migration. Keep the old dispatcher as `legacy_handle_key_event` behind `#[cfg(test)]` until the parity test has passed on the full matrix, then delete it in the same PR. Add `AppEvent::PickRepoOutcome` and the other inline-arm variants with reducer branches in `process_event`. Delete the 12 unbound variants (`HomeScreenNavigateUp/Down/Left/Right`, `FileFinderNavigateUp/Down`, `EnterScrollMode`, `ExitScrollMode`, `ConfigNextCategory`, `ConfigPrevCategory`, `NextWorkspace`, `PreviousWorkspace`).

#### 5. Interceptors consult the table
**File**: `ainb-tui/crates/ainb-core/src/main.rs`
**Changes**: `main.rs:591-623` (embed) and `:648` (`:` palette) and `:675-700` (scroll mode) call the same `keymap.resolve` with `HostFlags`; `Passthrough` writes to the PTY, `OpenSlashPalette` opens it, `Ui(ScrollPreview*)` runs the existing scroll code (moved into `UiState` in Phase 3). No behaviour change; the rule now lives in one table.

#### 6. Docs generator + CLI
**File**: `ainb-tui/crates/ainb-core/src/cli/keymap.rs` (new), `ainb-tui/crates/ainb-core/src/cli/mod.rs`
**Changes**: `ainb keymap list [--format md|json]` prints the merged table grouped by context with the `doc` string. `--format md` emits the exact content of `docs/tui/keyboard-shortcuts.md`.

**File**: `ainb-tui/scripts/gen-keymap-docs.sh` (new), `docs/tui/keyboard-shortcuts.md`, `.github/workflows/ci.yml`
**Changes**: script runs `ainb keymap list --format md > docs/tui/keyboard-shortcuts.md`; CI step "Assert docs/tui/keyboard-shortcuts.md is up to date" copied from the `cli.md` step at `ci.yml:168`, same path filter block at `:99`. First regeneration removes the 10 phantom bindings and adds the 10 undocumented ones; the PR description lists them.

#### 7. Tests
**File**: `ainb-tui/crates/ainb-core/tests/keymap_parity.rs` (new)
**Changes**: build a fixture matrix of `AppState` variants (one per `KeyContext`, ~40 states) × every chord in the table (letters in both `SHIFT`-bit forms) plus 30 unbound chords; assert `legacy_handle_key_event(ev, state)` and `Keymap::defaults().resolve(active_contexts(state), chord)` agree (compare `Debug` of the `AppEvent`). Hermetic fixtures: `AppState::default()` loads the real user config (`state.rs:3822`) and `InboxState::default()` opens the real notifyd SQLite (`components/inbox.rs:118-137`), so add `AppState::for_test()` that takes an `AppConfig::default()` and an in-memory `InboxState`, and pin `HOME` to a temp dir per the tripwire traps skill. Also: no duplicate `(ctx, chord)`; every `Binding.doc` non-empty; `Chord::parse(chord.as_str())` round-trips; the fixture file row count equals the table's row count.

**File**: `ainb-tui/crates/ainb-core/tests/keymap_toml.rs` (new)
**Changes**: override replaces a chord; unknown event name warns and is ignored; `ctrl+c` in `[embed_interactive]` is rejected; malformed TOML falls back to defaults with a warning.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-core --test keymap_parity` passes with the legacy dispatcher still present, then again after its deletion (the test then compares against a golden JSON dump of the pre-migration table committed in the same PR).
- [ ] `cargo test -p ainb-core --test keymap_toml` passes.
- [ ] Local gate: `cargo test -p ainb-core --tests` including all 101 `tripwire_*` tests (CI runs only two tripwire binaries, `.github/workflows/ci.yml:326,484,494`; this phase adds a named CI step running `tripwire_core_*` and the new-session tripwires, the ones that exercise key dispatch).
- [ ] `ainb keymap list --format md | diff - docs/tui/keyboard-shortcuts.md` is empty.
- [ ] `cargo clippy -p ainb-core -- -D warnings`; `unsafe_code` still forbidden.
- [ ] `rg -c 'KeyCode::' crates/ainb-core/src/app/events.rs` is 0 (the two component handlers in `components/new_session/` are out of scope and keep theirs).

#### Manual Verification:
- [ ] With `~/.agents-in-a-box/keymap.toml` mapping `[session_list] attach = "o"`, pressing `o` on a session attaches and `enter` no longer does.
- [ ] Inside an interactive embed, `ctrl+c` reaches the agent and `ctrl+q` still detaches.
- [ ] `?` help overlay content matches the generated doc.

### Checkpoints:
- **`[CHECKPOINT:human-verify]`**: keymap.toml override and embed passthrough
  - What was built: table-driven key dispatch, TOML overrides, generated docs
  - How to verify: 1) write the override above, start `ainb`, press `o` on a session, expect attach; 2) attach interactively, press `ctrl+c`, expect the agent to receive it; press `ctrl+q`, expect detach; 3) run `ainb keymap list` and spot-check three bindings you use daily.
  - Resume: Type "approved" or describe issues

---

## Phase S-A: File locks, atomic writes, tmux window-size
<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-core/src/config/mod.rs, ainb-tui/crates/ainb-core/src/config/lock.rs, ainb-tui/crates/ainb-core/src/cli/config_cmd.rs, ainb-tui/crates/ainb-core/src/config/favorites_store.rs, ainb-tui/crates/ainb-core/src/config/ssh_display_names.rs, ainb-tui/crates/ainb-core/src/config/onboarding.rs, ainb-tui/crates/ainb-plugin-burndown/src/config.rs, ainb-tui/crates/ainb-plugin-burndown/Cargo.toml, ainb-tui/crates/ainb-core/src/headroom/mod.rs, ainb-tui/crates/ainb-core/src/mcp_pool/proxy.rs, ainb-tui/crates/ainb-core/src/mcp_pool/client.rs, ainb-tui/crates/ainb-core/src/mcp_pool/paths.rs, ainb-tui/crates/ainb-core/src/usage_cache/db.rs, ainb-tui/crates/ainb-core/src/tmux/session.rs, ainb-tui/crates/ainb-plugin-notifyd/src/pid.rs, ainb-tui/crates/ainb-core/tests/config_concurrent_save.rs] -->

### Overview
Close the audit's data-loss and orphan hazards with the crate's existing primitives. One commit per numbered item.

### Changes Required:

#### 1. Shared config lock helper
**File**: `ainb-tui/crates/ainb-core/src/config/lock.rs` (new), `ainb-tui/crates/ainb-core/src/config/mod.rs`
**Changes**:

```rust
/// Cross-process lock for read-modify-write of files under the config dir.
/// Blocking exclusive flock on `<file>.lock`; released on drop. Same primitive as
/// ainb-skill-core/src/sync.rs:466 and ainb-fleet-core session_registry.rs:161.
pub(crate) struct ConfigLock(std::fs::File);
pub(crate) fn lock_for(path: &Path) -> std::io::Result<ConfigLock> {
    // `config.toml` -> `config.toml.lock`. NOT with_extension (that yields `config.lock`);
    // burndown must open the byte-identical name.
    let lock_path = std::path::PathBuf::from(format!("{}.lock", path.display()));
    let f = std::fs::OpenOptions::new().create(true).write(true).open(&lock_path)?;
    fs2::FileExt::lock_exclusive(&f)?;
    Ok(ConfigLock(f))
}
```

Blocking `lock_exclusive` everywhere (not `try_lock`): contention waits, config writes take milliseconds; the `try_lock_exclusive` pattern in skill-core is cited only for the `fs2` usage shape. `config.toml` has three writers, not one: `AppConfig::save` (`mod.rs:2029`), the external-keys helpers at `mod.rs:1343,1362,2083`, and `ainb config set` (`cli/config_cmd.rs:232,236`). Lock inside each writer, and let a caller hold one outer `ConfigLock` across a pair: the settings screen does `save` then `save_external_keys` back to back (`events.rs:3814` then `:3848`), which must be one critical section; give the writers an `_with_lock(&ConfigLock)` variant the pair calls. In `save`: keep `create_dir_all(&config_dir)` first (the lock file needs the directory), then take `lock_for(&config_path)` before `read_existing` and hold it through `write_atomic`. `lock_for` is not re-entrant: `save` must never be called while a `ConfigLock` for the same path is held on the same thread (grep callers of `save()` inside `save`-triggered callbacks such as `tunables::refresh_snapshot`; none exist today, add a debug assertion via a thread-local flag). Make `write_atomic` `pub` (from `pub(crate)`) so sibling modules reuse it. If the config dir is on a filesystem that refuses flock (some network mounts), `lock_for` returns the error and `save` proceeds unlocked with a warning, matching `session_registry.rs:168` best-effort semantics.

#### 2. Burndown's writer takes the same lock
**File**: `ainb-tui/crates/ainb-plugin-burndown/src/config.rs`, `ainb-tui/crates/ainb-plugin-burndown/Cargo.toml`
**Changes**: add `fs2 = { workspace = true }`; in `BurndownConfig::save` (`:169`) open `<config_path>.lock` built as `format!("{}.lock", path.display())`, i.e. `~/.agents-in-a-box/config/config.toml.lock`, the byte-identical name `lock_for` produces (both crates resolve the same config path: `config/mod.rs:2154-2158` and `plugin-burndown/src/plugin.rs:244-246`). Same 6 lines duplicated with a comment pointing at `ainb-core/src/config/lock.rs`; the plugin binary must not link `ainb-core`. Acquire before the read at `:177`, hold through the rename. Test in `config_concurrent_save.rs` asserts both writers created exactly one lock file with that name.

#### 3. In-place JSON writes become atomic
**File**: `favorites_store.rs:176`, `ssh_display_names.rs:59`, `onboarding.rs:104`
**Changes**: replace `fs::write(path, content)` with `crate::config::write_atomic(path, &content)` under `lock_for(path)`.

#### 4. Headroom proxy cross-process guard
**File**: `ainb-tui/crates/ainb-core/src/headroom/mod.rs:32,100-155`
**Changes**: keep `SPAWN_LOCK` for in-process serialisation; inside it run the guarded region in `tokio::task::spawn_blocking` (the fn is `async`, and `fs2` flock blocks the thread; the health poll is up to 5s at `:98-99`): take `lock_for(&pid_file())` (flock on `proxy.pid.lock`), probe, spawn if absent, poll until healthy, write `proxy.pid`, drop the lock. On failed bind, do not write the pid and do not kill the incumbent.

#### 5. MCP pool: lock across probe and bind
**File**: `mcp_pool/proxy.rs:94`, `mcp_pool/client.rs:107`, `mcp_pool/paths.rs`
**Changes**: `socket_alive_or_cleanup` gains a caller-held `lock_for(&socket_path)` requirement: `run_server_proxy` (async) acquires the lock inside `spawn_blocking`, then probes, unlinks if dead, binds (the bind itself is synchronous `UnixListener::bind`, done inside the same blocking task and the listener handed back), then drops the lock. The client's probe-then-spawn at `client.rs:107` takes the same lock around probe + spawn decision so two hosts cannot both spawn.

#### 6. usage_cache busy_timeout
**File**: `usage_cache/db.rs:36-55`
**Changes**: `conn.busy_timeout(std::time::Duration::from_secs(5))?;` after the WAL pragma, matching notifyd (`5s`) and hangar (`10s`).

#### 7. Pin tmux window-size
**File**: `tmux/session.rs:213` `configure_session`
**Changes**: add `tmux set-option -t <sanitized_name> window-size latest` next to the existing `history-limit` call. Never set `aggressive-resize`.

#### 8. notifyd: O_EXCL supersedes the spec's flock
**File**: `plugin-notifyd/src/pid.rs:25-34`
**Changes**: the spec asked for a flock; the audit's hazard is already closed by `StartupLock` (`listener.rs:105-138`), which takes `notify.lock` with `create_new` (O_EXCL, atomic) before any socket mutation. Its stale-recovery path (dead pid: remove and retry `create_new` once) is race-safe because the retry is itself O_EXCL, so two recoverers cannot both win; the loser bails. Accepted as is. Only change: replace the pid-file doc comment claiming it locks with a pointer to `StartupLock`.

#### 9. Test
**File**: `ainb-tui/crates/ainb-core/tests/config_concurrent_save.rs` (new)
**Changes**: copy the shape of `ainb-skill-core/tests/concurrent_sync_test.rs`: two OS processes (`std::process::Command` re-invoking the test binary with an env flag, as that test does) each run `AppConfig::save` 50 times with different keys against one temp `config.toml`; assert both keys survive every iteration. Second test: `lock_for` blocks a second holder until drop.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-core --test config_concurrent_save` passes 20 consecutive runs (`for i in $(seq 20); do cargo test -p ainb-core --test config_concurrent_save -q || exit 1; done`).
- [ ] `cargo test -p ainb-core` and `cargo test -p ainb-plugin-burndown` pass.
- [ ] `tmux show-options -t <session> window-size` prints `latest` for a session created by a tripwire launched with a tmux conf that sets `set -g window-size manual` (tmux 3.4 defaults to `latest`, so the assertion only means something under a conflicting user conf; add to `tripwire_new_session*.rs` using the harness's `-f` conf hook).
- [ ] `cargo clippy --workspace -- -D warnings`.

#### Manual Verification:
- [ ] Start `ainb` twice in two terminals, change a config setting in each, restart both: both settings persist.
- [ ] With the headroom proxy already running from terminal A, start `ainb` in terminal B: no second spawn attempt logged, `proxy.pid` unchanged.

---

## Phase S-B: Daemon connection registry
<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-hangar-proto/src/auth.rs, ainb-tui/crates/ainb-hangar-proto/src/methods.rs, ainb-tui/crates/ainb-hangar-proto/src/events.rs, ainb-tui/crates/ainb-hangar-proto/src/connections.rs, ainb-tui/crates/ainb-hangar-daemon/Cargo.toml, ainb-tui/Cargo.toml, ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs, ainb-tui/crates/ainb-hangar-daemon/src/rpc/auth.rs, ainb-tui/crates/ainb-hangar-daemon/src/rpc/connections.rs, ainb-tui/crates/ainb-hangar-daemon/src/answer.rs, ainb-tui/crates/ainb-hangar-client/src/lib.rs, ainb-tui/crates/ainb-plugin-hangar/src/plugin.rs, ainb-tui/crates/ainb-plugin-notifyd/src/resolver.rs, ainb-tui/crates/ainb-web/src/daemon.rs, ainb-tui/crates/ainb-hangar-daemon/tests/connections.rs] -->

### Overview
Minimal in-memory registry of connected surfaces, exposed by one read RPC and one push event, so any surface can show "also open in: tui, web".

### Changes Required:

#### 1. Proto
**File**: `ainb-hangar-proto/src/auth.rs:44`, new `connections.rs`, `methods.rs`, `events.rs`
**Changes**:

```rust
// auth.rs
pub struct HelloParams {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceInfo>,          // absent = "unknown", old clients keep working
}
// connections.rs
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SurfaceInfo { pub kind: SurfaceKind, pub pid: u32 }          // no host: clients have no hostname crate
pub enum SurfaceKind { Tui, Web, Desktop, Cli, Copilot, Unknown }
pub struct ConnectionRow { pub conn_id: u64, pub surface: SurfaceInfo, pub host: String, pub connected_at: DateTime<Utc>, pub tmux_clients: Vec<String> }
// `host` is stamped by the daemon from its own hostname (gethostname crate, daemon only;
// a remote ssh-forwarded client is still "the machine the daemon runs on" until D4 adds a client host field).
pub struct ConnectionsListResult { pub connections: Vec<ConnectionRow> }
// methods.rs
pub const HANGAR_CONNECTIONS_LIST: &str = "hangar/connections_list";   // add to ALL_METHODS (:1654)
// events.rs: HangarEvent::ConnectionsChanged { connections: Vec<ConnectionRow> }
```

#### 2. Daemon
**File**: `ainb-hangar-daemon/src/rpc/connections.rs` (new), `rpc/mod.rs:256-320`, `rpc/auth.rs`
**Changes**: `ConnectionRegistry(Arc<Mutex<HashMap<u64, ConnectionRow>>>)` owned by `serve()`; insert after a successful `auth/hello` (surface from params, `Unknown` if absent), remove when the connection task ends (same place the semaphore permit drops), broadcast `ConnectionsChanged` on both via the existing `EventBroker`. `hangar/connections_list` handler returns the rows. Every 5s the registry runs `tmux list-clients -F '#{session_name} #{client_tty} #{client_width}x#{client_height}'` once and fills `tmux_clients` per row by matching the client's known attached session names (attached sessions reported by clients in a later phase; in this phase the field carries every client of every session, grouped by session name, so the TUI card can show the count).

#### 3. Clients (four hello sites, not one)
**Files**: `ainb-hangar-client/src/lib.rs`, `ainb-plugin-hangar/src/plugin.rs:1035`, `ainb-plugin-notifyd/src/resolver.rs:140`, `ainb-web/src/daemon.rs:161`
**Changes**: the TUI hangar plugin, notifyd, and web each build their own `auth/hello` frame and never call the client's `hello()`. Each of the four sites sends `surface: Some(SurfaceInfo { kind, pid })`: plugin-hangar `Tui`, notifyd `Cli` (it is a daemon, label it `Notifyd` if a fifth kind is cheaper than confusion), web `Web`, `ainb-hangar-client` callers `Cli` by default with a setter. `connections_list()` and `ConnectionsChanged` decoding land in `ainb-hangar-client` only. Add `gethostname = "0.4"` to `[workspace.dependencies]` and to `ainb-hangar-daemon` only.

#### 3b. Daemon stamps `answered_by`
**File**: `ainb-hangar-daemon/src/answer.rs`
**Changes**: the handler always overwrites `AnswerParams.answered_by` with `"<kind>@<host>"` from the calling connection's registry row (client-supplied values are spoofable, `routes.rs:372`; `Unknown` connections get `unknown@<host>`). Surfaces stop needing a hostname; Phase S-C shrinks to the fold rule.

#### 4. Test
**File**: `ainb-hangar-daemon/tests/connections.rs` (new)
**Changes**: two clients hello with different kinds, `connections_list` shows both, closing one removes it and emits `ConnectionsChanged`; a client with no `surface` field lists as `Unknown`.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-hangar-daemon --test connections`.
- [ ] `cargo test -p ainb-hangar-proto` (the `ALL_METHODS` registry test passes with the new method).
- [ ] `cargo test -p ainb-plugin-cts-v2` unaffected (proto crate here is hangar-proto, not plugin-protocol).
- [ ] Existing daemon tests: `cargo test -p ainb-hangar-daemon`.

#### Manual Verification:
- [ ] Run `ainb` and `ainb web`; `ainb hangar rpc hangar/connections_list` (or the equivalent debug verb) shows `tui` and `web` rows with pids.

---

## Phase S-C: Card retirement on `AttentionAnswered`
<!-- wave: 2 | depends_on: [Phase S-B] | files: [ainb-tui/crates/ainb-plugin-hangar/src/screen/control_center.rs, ainb-tui/crates/ainb-web/frontend/app.js, ainb-tui/crates/ainb-fleet-tools/src/fleet.rs] -->

### Overview
Make the existing first-answer-wins guard visible: the TUI retires a card the instant another surface answers it; web relies on its poller.

### Changes Required:

#### 1. TUI folds `AttentionAnswered` as authoritative
**File**: `plugin-hangar/src/screen/control_center.rs`
**Changes**: the hangar plugin already subscribes (`attention/subscribe`, `rpc/mod.rs:584`). On `AttentionAnswered { attention_id, by }`: remove the card, close any open answer form for that id, show a 3s toast "answered by <by>" (`by` is now `<kind>@<host>` from S-B 3b). On `AlreadyAnswered { by }` RPC error: same toast, same retirement.

#### 2. Web scope
**File**: `ainb-web/frontend/app.js:367+` (SSE client)
**Changes**: web has no `AttentionAnswered` path; its SSE stream emits only `snapshot` from a 2s poller (`routes.rs:404-440`). Scope: the next snapshot clears the card within 2s; on an `already answered` error from `/api/answer`, close the form and toast immediately. No new SSE event in this phase.

#### 3. Copilot tool server
**File**: `fleet-tools/src/fleet.rs:436`
**Changes**: send `answered_by = "copilot"`; the daemon stamps the host.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-plugin-hangar` (add a unit test: reducer receives `AttentionAnswered` for an open form, form closes, card gone).
- [ ] `cargo test -p ainb-web` and the web e2e in `crates/ainb-web/e2e` (answer-conflict case: second answer gets the toast).

#### Manual Verification:
- [ ] Raise an ASK, open its answer form in the TUI, answer it from `ainb web`: the TUI form closes with the toast naming `web@<host>`; the reverse direction clears the web card within 2s.

---

## Phase 2: Versioned state sections
<!-- wave: 3 | depends_on: [Phase 3] | files: [ainb-tui/crates/ainb-core/src/app/versioned.rs, ainb-tui/crates/ainb-core/src/app/sections.rs, ainb-tui/crates/ainb-core/src/app/state.rs, ainb-tui/crates/ainb-core/src/app/events.rs, ainb-tui/crates/ainb-core/src/app/keymap.rs, ainb-tui/crates/ainb-core/src/app/keymap_defaults.rs, ainb-tui/crates/ainb-core/src/app/ui_state.rs, ainb-tui/crates/ainb-core/src/app/screens, ainb-tui/crates/ainb-core/src/app/state_tests.rs, ainb-tui/crates/ainb-core/src/main.rs, ainb-tui/crates/ainb-core/src/components, ainb-tui/crates/ainb-core/src/fleet/bridge/slack.rs, ainb-tui/crates/ainb-core/tests, ainb-tui/crates/ainb-plugin-hangar/src/plugin.rs] -->

### Overview
Regroup the 106 `AppState` fields into 19 section structs wrapped in `Versioned<T>`; every `&mut` deref bumps the version; `App::changed_since` reports bumped sections. One section per commit, tripwires green after each. The spec said 17 sections; the field audit produced 19 (session labels and ssh sessions split out of sessions, workspace-load split out of shell); the spec's count is superseded by this table.

### Changes Required:

#### 1. Wrapper
**File**: `ainb-tui/crates/ainb-core/src/app/versioned.rs` (new)

```rust
/// Version-stamped section of AppState. Any `&mut` access bumps `v`.
/// Over-bumping is harmless (an extra send); under-bumping is a bug, hence DerefMut.
#[derive(Debug, Default)]
pub struct Versioned<T> { v: u64, data: T }
impl<T> Versioned<T> {
    pub fn version(&self) -> u64 { self.v }
    pub fn get(&self) -> &T { &self.data }
    pub fn get_mut(&mut self) -> &mut T { self.v += 1; &mut self.data }   // ponytail: coarse bump, per-field later if traffic matters
}
impl<T> std::ops::Deref for Versioned<T> { type Target = T; fn deref(&self) -> &T { &self.data } }
impl<T> std::ops::DerefMut for Versioned<T> { fn deref_mut(&mut self) -> &mut T { self.get_mut() } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId { Sessions, SessionLabels, Tmux, Ssh, GitView, WorkspaceLoad, NewSession, Logs, ClaudeChat, Fleet, Hangar, McpPool, Inbox, PluginsHost, Config, Skills, Recovery, Onboarding, Shell }
pub type SectionVersions = [u64; 19];
```

#### 2. Section structs
**File**: `ainb-tui/crates/ainb-core/src/app/sections.rs` (new)
**Changes**: 19 structs, fields exactly as the audit grouped them (line numbers in `state.rs:3162-3521`):

| section | fields |
|---|---|
| `SessionsSection` | workspaces, selected_workspace_index, selected_session_index, shell_selected, selected_sessions, expand_all_workspaces, session_filter, attached_session_id, favorite_workspace_paths |
| `SessionLabelsSection` | session_label_store, session_label_rename_mode, session_label_rename_buffer, session_label_rename_target, session_context_menu, sessions_pane_state |
| `TmuxSection` | embed, embed_session, tmux_sessions, preview_update_task, other_tmux_sessions, other_tmux_expanded, selected_other_tmux_index, selected_other_tmux_sessions, other_tmux_rename_mode, other_tmux_rename_buffer |
| `SshSection` | ssh_sessions, ssh_sessions_expanded, selected_ssh_session_index, ssh_session_rename_mode, ssh_session_rename_buffer |
| `GitViewSection` | git_view_state, quick_commit_message, quick_commit_cursor, is_current_dir_git_repo |
| `WorkspaceLoadSection` | is_loading_workspaces, workspace_load_error, workspace_load_started, workspace_load_receiver, last_snapshot_time, last_preview_update, last_status_check |
| `NewSessionSection` | new_session_state, branch_refresh_receiver, branch_refresh_seq, repo_check_receiver, repo_check_seq, repo_init_receiver, repo_init_seq |
| `LogsSection` | logs, live_logs, log_last_updated, last_log_check, last_logs_session_id, log_streaming_coordinator, log_sender, log_history_state |
| `ClaudeChatSection` | claude_chat_visible, claude_chat_state, claude_manager |
| `FleetSection` | fleet_panel_state, attention_baseline, live_window_watcher, last_headroom_watchdog, last_token_refresh_check |
| `HangarSection` | pending_daemon_config_edits, hangar_daemon_config_loaded, daemons_state |
| `McpPoolSection` | mcp_overlay |
| `InboxSection` | inbox_state |
| `PluginsHostSection` | pending_plugin_renders, plugin_captures_text, plugin_render_errors, plugin_runtime (geometry maps already in UiState after Phase 3) |
| `ConfigSection` | app_config, config_screen_state, config_popup_state, changelog_state (statusline cache already in UiState after Phase 3) |
| `SkillsSection` | skills_state, skills_load_receiver, skill_manager_state, drift_load_receiver |
| `RecoverySection` | session_recovery_state |
| `OnboardingSection` | onboarding_state, setup_menu_state, auth_setup_state, auth_provider_popup_state |
| `ShellSection` | current_screen, previous_screen, should_quit, help_visible, focused_pane, ui_needs_refresh, home_screen_state, home_screen_v2_state, notifications, confirmation_dialog, pending_event, pending_async_action, async_operation_cancelled, last_panel_close_version (menu_bar_area already in UiState after Phase 3) |

`AppState` becomes `pub sessions: Versioned<SessionsSection>, ...` (19 fields). `Default` (`state.rs:3821-3992`) moves per section into `impl Default for XSection`.

#### 3. Mechanical access rewrite
**Files**: `state.rs`, `events.rs`, `main.rs`, `components/*`, `fleet/bridge/slack.rs`, `plugin-hangar/src/plugin.rs`, tests
**Changes**: `state.workspaces` → `state.sessions.workspaces` and so on. `Deref`/`DerefMut` keep the rewrite to a path prefix. Do it one section per commit with `ast-grep -p 'state.$FIELD' --rewrite 'state.<section>.$FIELD'` restricted to that section's field names, then `cargo build`, then tripwires. The draw path already takes `&AppState` after Phase 3, so drawing cannot bump. Split-borrow hazard: code holding `&mut state.a` and `&state.b` at once compiles today on distinct fields but fails once both sit behind one section's `DerefMut` (which borrows the whole section and defeats two-phase borrows). Two confirmed sites: `state.rs:4123` (`claude_chat_state` + `claude_manager`, both `ClaudeChatSection`) and `events.rs:3814` (`config_screen_state` + `app_config`, both `ConfigSection`). Fix pattern: `let sec = state.claude_chat.get_mut(); let (a, b) = (&mut sec.claude_chat_state, &sec.claude_manager);` (one bump, two field borrows on the inner struct); the per-section `cargo build` surfaces the rest.

#### 4. `changed_since`
**File**: `state.rs`
**Changes**:

```rust
impl AppState {
    pub fn versions(&self) -> SectionVersions;
    pub fn changed_since(&self, seen: &SectionVersions) -> Vec<SectionId>;
}
```

`main.rs` uses `changed_since` to set `needs_redraw` instead of the ad-hoc `ui_needs_refresh` writes where those are pure "state changed" signals (keep `ui_needs_refresh` for host-only repaint reasons).

#### 5. Tests
**File**: `state_tests.rs`, `tests/versioned_sections.rs` (new)
**Changes**: `process_event(AppEvent::X)` bumps exactly the expected sections for 10 representative events (attach, filter, open git view, inbox mark read, config save, notification add); a draw over `&AppState` bumps nothing; `changed_since` returns only bumped ids.

### Success Criteria:

#### Automated Verification:
- [ ] After each section commit: `cargo build -p ainb-core && cargo test -p ainb-core --tests` green.
- [ ] `cargo test -p ainb-core --test versioned_sections`.
- [ ] `cargo test -p ainb-plugin-hangar` (it touches `AppState` through the host).
- [ ] `rg -n 'fn (draw|render)\w*\(.*&mut AppState' crates/ainb-core/src/components crates/ainb-core/src/app/screens crates/ainb-core/src/main.rs` returns 0 lines (already true after Phase 3; this guards regression).
- [ ] Idle-loop check: run the TUI headless for 10s with no input (tripwire harness), sample `versions()` at start and end; only `WorkspaceLoadSection` (timestamps) and `Shell` (notifications TTL) may differ.

#### Manual Verification:
- [ ] TUI idle CPU unchanged (`abtop` or `top`: no repaint storm from over-bumping; `needs_redraw` only true after real changes).

---

## Phase 3: Scroll seal and UiState
<!-- wave: 2 | depends_on: [Phase 1] | files: [ainb-tui/crates/ainb-core/src/app/ui_state.rs, ainb-tui/crates/ainb-core/src/app/keymap.rs, ainb-tui/crates/ainb-core/src/app/keymap_defaults.rs, ainb-tui/crates/ainb-core/src/app/events.rs, ainb-tui/crates/ainb-core/src/app/state.rs, ainb-tui/crates/ainb-core/src/app/screens/mod.rs, ainb-tui/crates/ainb-core/src/app/screens/builtin.rs, ainb-tui/crates/ainb-core/src/app/registry.rs, ainb-tui/crates/ainb-core/src/main.rs, ainb-tui/crates/ainb-core/src/components/tmux_preview.rs, ainb-tui/crates/ainb-core/src/components/layout.rs, ainb-tui/crates/ainb-core/src/components/session_list.rs, ainb-tui/crates/ainb-core/src/components, ainb-tui/crates/ainb-plugin-hangar/src/plugin.rs, ainb-tui/crates/ainb-core/tests/ui_state.rs] -->

### Overview
Move renderer-only state (scroll offsets, auto-scroll, Rects, plugin geometry, the status-bar TTL cache, mouse hit-testing) into a ratatui-host `UiState`; make the whole draw path take `&AppState`; delete the 9 scroll `AppEvent` variants; `main.rs` mutates nothing behind the reducer. Runs before Phase 2 so that versioning only ever sees real mutations.

#### 0. Draw path becomes read-only on core state
**Files**: `app/screens/mod.rs:85`, `app/screens/builtin.rs:635-1065` (~17 `impl Screen`), `app/registry.rs`, `components/layout.rs:81,88,628`, every `components/*.rs` render fn that takes `&mut AppState`
**Changes**: `Screen::render(&mut self, frame, area, state: &AppState, ui: &mut UiState)`; `layout.rs:81 render` and `:628 render_status_bar` take `(&AppState, &mut UiState)`; `build_live_status_spans` (`layout.rs:887`) stays pure. `statusline_status_cached` (`state.rs:4032`) moves to `UiState::statusline_status(&mut self, state: &AppState)`. Find the full list with `rg -n 'fn (render|draw)\w*\(.*&mut AppState' crates/ainb-core/src/components crates/ainb-core/src/app` and convert every hit. `ainb-plugin-hangar` renders through the host and needs no change unless it calls a `&mut` draw helper (check `plugin.rs` for `render(` callers).

### Changes Required:

#### 1. UiState
**File**: `ainb-tui/crates/ainb-core/src/app/ui_state.rs` (new)

```rust
/// Renderer-local state for the ratatui host. Never crosses to another surface.
#[derive(Debug, Default)]
pub struct UiState {
    pub preview_scroll: ScrollState,      // from tmux_preview.rs scroll mode
    pub logs_scroll: ScrollState, pub logs_auto_scroll: bool,
    pub menu_bar_area: Option<Rect>,      // from ShellSection
    pub embed_pane_area: Option<Rect>,    // from TmuxSection
    pub plugin_render_areas: HashMap<String, Rect>, pub plugin_render_origins: HashMap<String,(u16,u16)>, pub plugin_last_render_viewport: HashMap<String,(u16,u16)>,
    pub needs_redraw: bool,
}
pub enum UiAction { ScrollLogsUp, ScrollLogsDown, ScrollLogsToTop, ScrollLogsToBottom, ToggleAutoScroll, ScrollPreviewUp, ScrollPreviewDown, ExitScrollMode }
impl UiState { pub fn apply(&mut self, a: UiAction, layout: &mut Layout); }
/// Mouse hit-test: Rects in, an AppEvent or UiAction out. Geometry from events.rs:883-921.
pub fn hit_test(ui: &UiState, state: &AppState, m: &MouseEvent) -> Option<KeyAction>;
```

`handle_mouse_event` (`events.rs:952-1228`, 277 lines) is not a pure hit test today: it mutates state directly (selection changes, pane focus, menu clicks). Phase 3 splits it: the Rect lookup becomes `hit_test`, and each mutation becomes an `AppEvent::Mouse*` variant (or an existing variant such as `SelectSession(idx)`) handled in `process_event`. Budget the reducer rewrite as its own commit; the split is the same key-vs-intent seam Phase 1 applied to keys.

`SessionsPaneState` (`state.rs:596-604`) mixes persisted `preferred_width` / `collapsed` with Rect geometry: only the geometry moves to `UiState`; the two persisted fields stay in `SessionLabelsSection`. `components/session_list.rs:126,173` writes a `Rect` and is in this phase's file list.

#### 2. Remove from core
**Files**: `events.rs`, `state.rs`
**Changes**: delete the 9 variants (`ScrollLogsUp/Down/ToTop/ToBottom`, `ToggleAutoScroll`, `ScrollPreviewUp/Down`, `EnterScrollMode`, `ExitScrollMode`) and their `process_event` branches; remove `menu_bar_area` (`state.rs:3230`), `embed_pane_area` (`:3227`), the three plugin geometry maps (`:3370,3379,3389`), and `statusline_status_cache` (`:3434`) from `AppState`; delete the `Rect` refs at `state.rs:26,601,602,605,632,641` by moving `SessionsPaneState` geometry (`state.rs:596`) into `UiState`.

#### 3. Host wiring
**Files**: `main.rs:675-760`, `keymap_defaults.rs`, `components/tmux_preview.rs`, `components/layout.rs`, `screens/builtin.rs:645`
**Changes**: the interceptor block calls `ui.apply(action, &mut layout)` for `KeyAction::Ui`; `PreviewScroll` rows in the default table map to `UiAction`; plugin mouse coordinate translation (`builtin.rs:645`) reads origins from `UiState`.

#### 4. Test
**File**: `tests/ui_state.rs` (new)
**Changes**: `UiAction` sequence over a fixed layout yields expected offsets; `hit_test` on a fixture Rect map returns the right `AppEvent`; `AppState` has no field of type `Rect` (`rg -c 'Rect' src/app/state.rs src/app/sections.rs` is 0).

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-core --test ui_state`.
- [ ] `rg -c 'ratatui::layout::Rect|Rect>' crates/ainb-core/src/app/state.rs` is 0.
- [ ] `rg -n 'fn (render|draw)\w*\(.*&mut AppState' crates/ainb-core/src/components crates/ainb-core/src/app` returns 0 lines.
- [ ] `rg -n 'layout\.' crates/ainb-core/src/main.rs` shows only calls through `ui.apply` or draw.
- [ ] All tripwires green, especially `tripwire_*preview*`, `tripwire_*logs*`, `tripwire_*mouse*` if present (list with `ls crates/ainb-core/tests/tripwire_*`).

#### Manual Verification:
- [ ] Scroll the tmux preview with `j`/`k`/PageUp, exit with `esc`; scroll logs with the same keys and toggle auto-scroll; click a session row and a menu bar item.

### Checkpoints:
- **`[CHECKPOINT:human-verify]`**: scroll and mouse behaviour after the seal
  - What was built: renderer-local `UiState`, scroll variants removed from the reducer
  - How to verify: 1) open a session with long output, enter preview scroll mode, page up and down, `esc`; 2) open logs, scroll, toggle auto-scroll; 3) click three sidebar items and one menu bar entry; expect identical behaviour to before.
  - Resume: Type "approved" or describe issues

---

## Phase S-D: Concurrency tests across surfaces
<!-- wave: 4 | depends_on: [Phase S-A, Phase S-B, Phase S-C, Phase 2] | files: [ainb-tui/crates/ainb-hangar-daemon/tests/answer_race.rs, ainb-tui/crates/ainb-core/tests/tripwire_multi_attach_resize.rs, ainb-tui/scripts/surface-combo-smoke.sh, .github/workflows/ci.yml] -->

### Overview
Prove the policy: two answers race, three clients attach and one resizes during a picker answer, and every surface combination starts clean.

### Changes Required:

#### 1. Answer race
**File**: `ainb-hangar-daemon/tests/answer_race.rs` (new)
**Changes**: raise one attention row, fire two `attention/answer` calls concurrently from two clients (`SurfaceKind::Tui`, `SurfaceKind::Web`); assert exactly one `Delivered` and one `AlreadyAnswered { by }` where `by` names the winner in `<kind>@<host>` form; assert one `AttentionAnswered` event was broadcast.

#### 2. Multi-attach resize during answer
**File**: `ainb-core/tests/tripwire_multi_attach_resize.rs` (new; follow `.claude/skills/tmux-ui-tripwire` traps)
**Changes**: create a session with a picker prompt on screen (reuse the picker fixture from `answer.rs` tests), attach three tmux clients at 80x24, 120x40, 200x50 (three PTYs from the tripwire harness, each running `tmux attach -t`), start `attention/answer` for the picker, resize one client mid-flight with `tmux refresh-client -t <client> -C 100x30` (never `resize-window`, which flips `window-size` to `manual` and would contradict the next assertion), assert the answer result is `Confirmed`, and `tmux show-options -t <session> window-size` is still `latest`. If three PTYs prove flaky in CI, the minimal variant is two clients (the picker client and one resizer), which still exercises the reflow-during-verify path. This is the interaction the design pass could not rule out (`answer.rs:558-687`).

#### 3. Surface combo smoke
**File**: `ainb-tui/scripts/surface-combo-smoke.sh` (new), `.github/workflows/ci.yml`
**Changes**: for each of {tui}, {web}, {tui, web}, {tui, tui}: start the daemon, start the surfaces (TUI headless in tmux per tripwire harness, web on a random port), call `hangar/connections_list`, assert the expected kinds, save a config key from each surface, assert all keys persist, stop everything, assert no orphan `proxy.pid` and exactly one `daemon.lock` holder. Wire as a CI job after the tripwire job.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p ainb-hangar-daemon --test answer_race` 20 consecutive runs green.
- [ ] `cargo test -p ainb-core --test tripwire_multi_attach_resize`.
- [ ] `scripts/surface-combo-smoke.sh` exits 0 locally and in CI.

#### Manual Verification:
- [ ] none; this phase is the automation.

---

## Testing Strategy

### Unit Tests
- Keymap: parity matrix, duplicate detection, TOML override rules, embed passthrough rejection.
- Versioned: bump on `&mut`, no bump on `&`, `changed_since` exactness.
- UiState: scroll arithmetic, hit-test mapping.
- Locks: `lock_for` blocks second holder; `write_atomic` leaves no temp on failure.

### Integration Tests
- Concurrent config save across two processes; answer race across two clients; connection registry add/remove; multi-attach resize during a picker answer; surface combination smoke.
- All 101 existing tripwires after every phase, run locally (`cargo test -p ainb-core --tests`); CI runs two tripwire binaries today, and each phase adds its relevant ones as named CI steps.

### Manual Testing Steps
1. Override a binding in `keymap.toml`, confirm in the TUI, confirm `ainb keymap list` shows it.
2. Run `ainb` twice, save different settings, restart, both persist.
3. Answer one ASK from web while the TUI form is open; TUI retires the card with the toast.
4. Scroll preview and logs, click sidebar, after Phase 3.

## Performance Considerations
- `Versioned::deref_mut` bumps on every `&mut` deref; the 16ms tick calls `App::tick` which mutates a few sections each tick (timestamps in `WorkspaceLoadSection`). Keep timestamps that change every tick out of hot sections or accept those sections always "changed"; measured in Phase 2 manual check. Per-field bumps are the upgrade path.
- `lock_for` is a blocking flock; config saves are rare and small.
- `tmux list-clients` every 5s in the daemon is one subprocess; skip when no connections.

## Migration Notes
- `docs/tui/keyboard-shortcuts.md` regeneration removes 10 phantom bindings; call them out in the changelog.
- `keymap.toml` is optional; absence is the default.
- `HelloParams.surface` is optional; old clients hello as `Unknown`.
- `answered_by` strings change shape; nothing parses them today (`snapshots.rs:770` carries them verbatim).

## References
- Spec: `docs/plans/2026-09-04-desktop-shared-core-spec.md`
- Research: `research/2026-09-04_14-10-02_desktop-app-shared-core.md`, `research/2026-09-04_16-25-00_tauri-agent-apps-prior-art.md`
- Lock pattern: `ainb-tui/crates/ainb-skill-core/src/sync.rs:466`, `ainb-tui/crates/ainb-skill-core/tests/concurrent_sync_test.rs`
- Atomic write: `ainb-tui/crates/ainb-core/src/config/mod.rs:1307`
- Docs drift gate to copy: `.github/workflows/ci.yml:138-168`, `ainb-tui/scripts/gen-cli-reference.sh`
- Plugin key translation to mirror: `ainb-tui/crates/ainb-core/src/app/screens/mod.rs:96-98`, `screens/builtin.rs:85,183`
- Tripwire traps: `.claude/skills/tmux-ui-tripwire/SKILL.md`
