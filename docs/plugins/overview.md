---
title: "ainb v2 plugins — overview"
---

What a v2 subprocess plugin is, conceptually. New here? Read [README.md](./README.md) first — it disambiguates from Claude Code plugins.

For the user CLI flow, jump to [user-guide.md](./user-guide.md). To write one, [authoring.md](./authoring.md). For the wire contract, [spec-v2.md](./spec-v2.md).

## What is a plugin?

A plugin is a self-contained capsule that adds a screen, CLI subcommand, sidebar entry, or statusline segment to ainb without recompiling the host. A v2 plugin is a **native executable** the host spawns as a child process; the two talk JSON-RPC 2.0 over framed stdio.

Plugins only see the host capabilities they declare in their manifest, and they cannot reach the network, filesystem, or subprocess launcher unless you grant those capabilities.

## Architecture at a glance

![ainb v2 plugin architecture — host, JSON-RPC stdio, plugin subprocesses, capability gate, event bus](../assets/diagrams/plugin-architecture.svg)

The host (`ainb-core`) spawns each plugin as a child process and drives it over **JSON-RPC 2.0 / Content-Length-framed stdio**. Host→plugin methods: `plugin/init` (with the granted capabilities), `plugin/render` (host sends a `Viewport`, plugin returns a `WireBuffer`), `plugin/handle_key`, `plugin/handle_event`, `plugin/cli_dispatch`, `plugin/shutdown`. Plugin→host (reverse) calls: `host/snapshot/publish` + `host/snapshot/subscribe` (the **event bus**) and `host/action/invoke`. The `ainb-plugin-runtime` enforces capabilities — an ungranted host-fn call comes back as JSON-RPC `-32001` (`CAPABILITY_DENIED`).

### Two ways a plugin screen renders

- **In-process `WireBuffer`** — the host owns the terminal; the plugin paints a sparse cell grid the host blits each frame. Integrated and themeable. This is how **`burndown`** draws the Analytics dashboard.
- **Host-embedded foreign TTY** — for an interactive program that has no machine-readable render (only its own TUI), the host *suspends* and hands the whole terminal to the external binary, resuming when it exits — the same mechanism ainb uses to attach to agent sessions. This is how **`witr`** opens its all-process browser (`witr -i`): there's no JSON for witr's live process list, so pressing `w` runs `tmux new-session -A -d -s ainb-witr "witr -i"` and attaches full-screen.

## What a plugin can own

- **A TUI screen** — implement `Plugin::render` and paint a `WireBuffer` per frame; the host blits it onto the terminal.
- **A CLI subcommand tree** — claim a `cli_namespaces` entry in your manifest; the host dispatches `ainb <ns> ...` invocations through `Plugin::cli_dispatch`.
- **Snapshot topics (publish/subscribe)** — push data on a topic via `HostClient::snapshot_publish`; subscribers (other plugins or the host) get `plugin/handle_event` deliveries.
- **A statusline segment** — own a slice of the persistent status bar.
- **Its own state** — write under `~/.agents-in-a-box/plugins/<name>/` (gated by `write_plugin_data`).
- **Host actions** — invoke host-owned operations via `host/action/invoke` (gated by capability).

## Reference plugins

Four plugins ship in-tree as the canonical examples:

- **[`burndown`](./burndown.md)** — screen-owner reference. Owns the Analytics screen (renders a ratatui dashboard into a `WireBuffer` each frame) and the `ainb usage` CLI tree; subscribes to `sessions.usage_data` from `session-reader`.
- **[`session-reader`](./session-reader.md)** — pure-publisher reference. No screen. Scans `~/.claude/projects/**`, `~/.codex/sessions/**` (and more) and chunk-publishes usage snapshots on `sessions.usage_data` for `burndown` to render.
- **[`witr`](./witr.md)** — subprocess-wrapper reference. The `ainb witr <target>` CLI + `/witr` slash run `witr --json <target>` and parse the ancestry JSON; its **screen** is a host-embedded foreign TTY (`w` hands the terminal to `witr -i` — see [the two render paths](#two-ways-a-plugin-screen-renders)). Declares `spawn_subprocess` + `event_bus`.
- **[`notifyd`](./notifyd.md)** — note: *not* a v2 subprocess plugin. It's an in-tree daemon (binary `ainb-notifyd`) compiled into `ainb-core` that owns the Inbox screen and captures Claude Code / Codex hook events into SQLite; documented here as a sibling for completeness.

Each links to its own page with a `/fireworks-tech-graph` diagram of how it works.

![Burndown plugin — full analytics dashboard](../assets/screenshots/burndown.png)

*The burndown plugin rendering the full analytics dashboard against real `~/.claude/projects` data.*

## Where plugins live on disk

The host discovers plugins from a flat staging directory:

```text
dist/plugins/
├── burndown/
│   ├── burndown            (native executable, ad-hoc signed on macOS)
│   └── manifest.toml
├── session-reader/
│   ├── session-reader
│   └── manifest.toml
└── witr/
    ├── witr
    └── manifest.toml
```

(`notifyd` is **not** here — it's a daemon compiled into `ainb-core`, not a staged subprocess plugin; see [its page](./notifyd.md).)

That layout is what `just stage-plugins` produces from in-tree crates, and what the host walks on startup. The `AINB_PLUGIN_ROOT` env var overrides it (defaults to `<workspace-root>/dist/plugins`).

Plugin-writable state lives under `~/.agents-in-a-box/plugins/<name>/` (override with `AINB_HOME`), gated by the `write_plugin_data` capability.

## Capability model (summary)

Every plugin declares the host capabilities it needs in its `manifest.toml`:

```toml
[capabilities]
read_sessions       = true     # ~/.agents-in-a-box/sessions/**
read_claude_logs    = true     # ~/.claude/projects/**/*.jsonl
read_codex_logs     = false    # ~/.codex/sessions/**/*.jsonl
write_plugin_data   = true     # ~/.agents-in-a-box/plugins/<name>/ writable
event_bus           = true     # publish/subscribe across plugins
spawn_subprocess    = false    # exec child processes
network             = []       # bool or hostname allow-list
```

Default for every flag is **deny** (`false` / `[]`). The runtime rejects host-fn calls against a capability the manifest doesn't grant with JSON-RPC error code `-32001` (`CAPABILITY_DENIED`).

Full semantics: [spec-v2.md §1](./spec-v2.md#1-manifest) and [spec-v2.md §9](./spec-v2.md#9-capability-gates).

## Versus the deprecated v1 wasm contract

v1 used `wasm32-wasip1` cdylibs in a wasmi host runtime with linker-omitted host-fn imports for capability gating. v2 dropped wasm entirely — a normal `cargo build` binary, native code, OS-process boundary. The wasm sandbox added implementation cost without buying any safety property the OS process boundary doesn't already provide for ainb's threat model.

## Next steps

- **End user?** [user-guide.md](./user-guide.md) — every `ainb plugin` command.
- **Building a plugin?** [authoring.md](./authoring.md) — Rust SDK, scaffolding, debugging.
- **Implementing a host?** [spec-v2.md](./spec-v2.md) — the wire contract.
