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

## See also

- [Overview](overview.md)
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Docs hub](../README.md)
