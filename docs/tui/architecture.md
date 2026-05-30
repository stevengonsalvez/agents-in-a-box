---
title: "TUI architecture"
---

# TUI architecture

`ainb` is a Cargo **workspace** rooted at `ainb-tui/`. The TUI application and CLI live in the `ainb-core` crate; the rest of the workspace is the v2 plugin platform (runtime, SDK, reference plugins, conformance + test tooling).

## Workspace crates

Members are declared in `ainb-tui/Cargo.toml` (`default-members = ["crates/ainb-core"]`):

| Crate | Role |
|-------|------|
| `ainb-core` | The TUI app + `ainb` CLI binary. All screens, event loop, session/git/tmux/docker integration. |
| `ainb-plugin-runtime` | Host-side plugin runtime that loads and supervises v2 plugins. |
| `ainb-plugin-sdk-rust` | Rust SDK for authoring v2 plugins. |
| `ainb-plugin-protocol` | Wire protocol / JSON-RPC types shared by host and plugins. |
| `ainb-plugin-types-sessions` | Shared session data types exposed to plugins. |
| `ainb-plugin-burndown` | In-tree v2 reference plugin: usage/burndown analytics. |
| `ainb-plugin-notifyd` | In-tree v2 reference plugin: notifications. |
| `ainb-plugin-session-reader` | In-tree v2 reference plugin: session data backend. |
| `ainb-plugin-cts-v2` | Conformance test suite for the v2 plugin ABI. |
| `ainb-plugin-testkit` | Test harness for plugin authors. |
| `xtask` | Workspace task runner (build/release helpers). |

The workspace pins `version = "1.2.0"`, Rust edition 2021, and forbids `unsafe_code`.

## ainb-core module tree

App code lives under `ainb-tui/crates/ainb-core/src/`:

```
crates/ainb-core/src/
├── main.rs              # Entry point, CLI parsing, TUI loop
├── lib.rs               # Crate exports
├── app/                 # App state machine + event handling (state.rs, events.rs)
├── components/          # TUI screens (session_list, git_view, logs_viewer, home_screen, …)
├── widgets/             # Reusable UI widgets
├── cli/                 # Non-interactive subcommand implementations
├── fleet/               # `ainb fleet` orchestration (standup/broadcast/sequence/needs/daemon)
├── providers/           # Multi-provider (claude/codex/gemini/copilot) abstractions
├── claude/              # Claude API client
├── docker/              # Container management
├── tmux/                # Tmux / PTY integration
├── git/                 # Git + worktree operations
├── config/              # Configuration loading
├── models/              # Data models
├── agents/              # Agent registry
├── agent_parsers/       # Parse agent output
├── interactive/         # Interactive-mode helpers
├── usage_cache/         # Persistent usage-analytics cache
├── audit.rs             # Audit helpers
├── credentials.rs       # Credential storage
├── editors.rs           # Editor integration
└── plugins.rs           # Plugin install/management CLI surface
```

## Plugin runtime

`ainb-plugin-runtime` is the host-side supervisor that loads v2 plugins as native subprocess binaries speaking JSON-RPC over stdio (the `ainb-plugin-protocol` types). Authors build against `ainb-plugin-sdk-rust` and validate with `ainb-plugin-testkit` and the `ainb-plugin-cts-v2` conformance suite. The three in-tree reference plugins — `burndown` (analytics), `notifyd` (notifications), and `session-reader` (data backend) — double as worked examples. Install and inspect plugins via `ainb plugin install|list|lint|watch|tail` (see the [CLI reference](cli.md)).

## Style guide

Components follow the shared palette in `.claude/skills/tui-screen/SKILL.md`: cornflower-blue borders (`Rgb(100,149,237)`), gold titles/CTAs (`Rgb(255,215,0)`), green active state (`Rgb(100,200,100)`), `BorderType::Rounded` on all panels, a `▶` selection indicator, and a gold-keys / muted-descriptions bottom help bar.

## Testing surface

Tests live under `crates/ainb-core/tests/`:

- **Unit + model tests** — `test_app_state.rs`, `test_events.rs`, `test_session_model.rs`, etc.
- **Behavioral** — `behavioral.rs` and the `behavioral/` suite.
- **E2E PTY** — `e2e_pty_tests.rs`, `interactive_mode_tests.rs` (real PTY drive).
- **Tripwires** — `tripwire_*.rs` tmux-driven end-to-end screen assertions (burndown, inbox, crash recovery, new-session, …).

## See also

- [Overview](overview.md)
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Docs hub](../README.md)
