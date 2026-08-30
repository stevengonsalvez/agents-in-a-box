---
title: "TUI architecture"
---

`ainb` is a Cargo **workspace** rooted at `ainb-tui/`. The TUI application and CLI live in the `ainb-core` crate; the rest of the workspace is the v2 plugin platform (runtime, SDK, reference plugins, conformance + test tooling).

## Workspace crates

34 members, declared in `ainb-tui/Cargo.toml`. `default-members` is
`["crates/ainb-core", "crates/ainb-hangar-daemon"]`, so a bare `cargo build`
produces the TUI and the Hangar daemon.

### The host

| Crate | Role |
|---|---|
| `ainb-core` | The TUI app and the `ainb` binary: screens, event loop, session, git, tmux and provider integration. |
| `ainb-cli` | The skill-manager CLI (`ainb skill`, `source`, `search`), routed before the async runtime starts. |
| `ainb-web` | Read-only SSE web dashboard over the fleet. |
| `xtask` | Workspace automation: builds canary plugins and release helpers. |

### Plugin platform

| Crate | Role |
|---|---|
| `ainb-plugin-protocol` | Wire types: JSON-RPC 2.0 envelopes, Content-Length framing, error codes. |
| `ainb-plugin-runtime` | Host side: subprocess spawn, lifecycle FSM, snapshot and action bus, capability gate. |
| `ainb-plugin-sdk-rust` | Rust SDK for writing a plugin. |
| `ainb-plugin-testkit` | In-process harness so a plugin author can test without a subprocess. |
| `ainb-plugin-cts-v2` | Conformance suite: 18 numbered axes plus 3 canaries, 21 in total. |
| `ainb-plugin-types-sessions` | Shared session schema exposed across the wire. |

### In-tree plugins

Six, staged into `dist/plugins/<id>/` by `scripts/build-plugins.sh`.

| Crate | Plugin id | Role |
|---|---|---|
| `ainb-plugin-burndown` | `burndown` | Usage and cost analytics; owns the Stats screen and `ainb usage`. |
| `ainb-plugin-session-reader` | `session-reader` | Silent publisher: walks provider logs, feeds burndown. |
| `ainb-plugin-witr` | `witr` | Process-causality tracing; wraps the external `witr` binary. |
| `ainb-plugin-abtop` | `abtop` | Live agent-process monitor; wraps the external `abtop` binary. |
| `ainb-plugin-learnings` | `learnings` | Browse, search and graph the knowledge base. |
| `ainb-plugin-hangar` | `hangar-tui` | The Hangar screen; a client of the Hangar daemon. |

> `ainb-plugin-notifyd` is **not** a plugin despite the crate name. It builds
> the `ainb-notifyd` daemon, which listens on a Unix socket and writes to
> SQLite; the Inbox screen reads that store. It has no plugin manifest and
> speaks no plugin ABI.

### Hangar

Seven crates behind the managed-agents control plane.

| Crate | Role |
|---|---|
| `ainb-hangar-core` | IO-free domain types: actors, ids, task status, clock. |
| `ainb-hangar-proto` | The daemon's JSON-RPC surface; `ALL_METHODS` is the registry. |
| `ainb-hangar-store` | SQLite pool, embedded migrations, repository wrappers. |
| `ainb-hangar-client` | The one client onto the daemon's socket. |
| `ainb-hangar-daemon` | The control plane itself: scheduler, task FSM, agent runner. |
| `ainb-hangar-sandbox` | Filesystem-confined spawn wrapper (Seatbelt on macOS). |
| `ainb-hangar-secrets` | Read/write bridge onto the OS keychain. |

### Fleet, skills and support

| Crate | Role |
|---|---|
| `ainb-fleet-core` | Session discovery, needs classification, verified tmux delivery. |
| `ainb-fleet-tools` | The fleet copilot's MCP tool server. |
| `ainb-acp` | Agent Client Protocol client, transcript reducer, store writer. |
| `ainb-skill-core` | Manifest-driven install, sync and removal of skills and agents. |
| `ainb-adapters-source` | Where units come from: git, local, marketplace. |
| `ainb-adapters-tool` | Where units go: the nine tool homes. |
| `ainb-diff` | Confirm-diff rendering for the skill installer. |
| `ainb-fetch` | Shared HTTP fetch with caching. |
| `ainb-usage` | Usage and cost primitives shared by burndown and fleet cost. |
| `ainb-model-rates` | Per-model pricing tables. |

Rust edition 2021, `unsafe_code` forbidden. Regenerate the counts above with
`cargo metadata --no-deps`.

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
