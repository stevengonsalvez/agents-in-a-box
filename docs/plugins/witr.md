---
title: "witr plugin"
description: "Wraps the external witr binary to surface process-causality tracing as an ainb screen, CLI, and slash command."
---

`witr` surfaces process-causality tracing inside ainb by wrapping the external [`witr`](https://github.com/pranshuparmar/witr) binary (>= 0.3.2 on `PATH`). It is the canonical **subprocess-wrapping** reference — and it exemplifies *two* integration patterns at once: a host-embedded **foreign TTY** for its interactive screen, and a `spawn_subprocess` **exec-and-parse** flow for its CLI and slash command.

![witr's interactive process browser embedded in ainb](../assets/screenshots/witr-browser.png)

*Press `w` in ainb to open witr's interactive browser full-screen — a sortable all-process list with a live ancestry pane.*

## How it works

![witr plugin — how it works](../assets/diagrams/witr.svg)

There is no single render path here — witr is two surfaces fused onto one external binary, and the diagram shows them as two lanes.

**The screen is not a `WireBuffer`.** witr's value is its own interactive all-process browser (sortable process list + ancestry pane), which has no machine-readable render — it lives only in `witr -i`. So pressing `w` (or the sidebar tile) does a host-level handoff, not a `plugin/render` call. The host (`ainb-core`) emits `AppEvent::GoToWitr`, which queues `AsyncAction::AttachWitr`: ainb runs `tmux new-session -A -d -s ainb-witr "witr -i"`, suspends its own TUI, and attaches full-screen to witr's native interactive browser — the same suspend/attach/resume mechanism ainb uses to embed agent sessions. When the user quits witr, ainb resumes. This wiring lives entirely in `ainb-core`; the plugin's `render` is never invoked for the screen.

**The CLI and slash run `witr --json` and parse it.** `ainb witr <target>` is routed by the host to the plugin's `plugin/cli_dispatch` (namespace `witr`); `/witr <target>` reaches `handle_slash`. Both call the plugin's single `scan()` authority, which execs `witr --json <target>` under the `spawn_subprocess` capability (argv array, no shell; bounded by a 5s timeout) and decodes stdout into a typed `WitrSnapshot`. The decode tolerates two real witr quirks: witr exits non-zero to signal that *warnings* are present even when it printed a complete, valid JSON snapshot — so a successful parse wins regardless of exit code — and Go's `encoding/json` marshals empty slices/maps as `null`, which is mapped back to empty collections. From the snapshot, the CLI emits text / `--tree` / `--warnings` / canonical `--format json`, while the slash opens a detail overlay.

**Fresh scans publish on the event bus.** When a scan runs from a live host channel (the TUI refresh path), the parsed snapshot is published to the `witr.snapshot` topic via `host/snapshot/publish`, msgpack-encoded and byte-deterministic. The one-shot `ainb witr` CLI invocation passes no host channel — there are no bus subscribers during a single CLI run — so the publish is skipped there.

## Capabilities

These are the exact non-default capabilities declared in `manifest.toml`. Every other capability (`read_sessions`, `read_claude_logs`, `read_codex_logs`, `write_plugin_data`, `network`) is left at its deny default.

| Capability | Declared value | Why it is needed |
|---|---|---|
| `spawn_subprocess` | `["witr"]` | Exec `witr --version` (detect on init) and `witr --json <target>` (every scan). The list form is per-binary audit metadata. |
| `event_bus` | `true` | Publish each fresh scan on the `witr.snapshot` topic via `host/snapshot/publish`. |

## Using it

- **Screen** — open it by pressing `w` from the home surface or selecting the witr sidebar tile. Instead of a plugin-rendered screen, ainb hands the terminal to witr's native `witr -i` interactive browser full-screen (sortable all-process list + ancestry pane); quit witr to return to ainb. If witr is missing or below 0.3.2, the plugin renders an install/upgrade empty state instead.
- **CLI** (`cli_namespaces = ["witr"]`) — `ainb witr` execs the wrapped binary and renders the parsed snapshot:
  - `ainb witr nginx` — trace a process by name (positional; witr fuzzy-matches)
  - `ainb witr --pid 1234`, `ainb witr --port 5432`, `ainb witr --file /var/run/x.lock`, `ainb witr --container redis` — typed targets
  - `ainb witr nginx --tree` — ancestry chain as an indented tree
  - `ainb witr --port 5432 --warnings` — just the warnings list
  - `ainb witr --pid 1234 --format json` — canonical JSON re-emit of the snapshot
  - `ainb witr nginx --short` — raw `witr <target>` passthrough (no JSON re-parse; cannot be combined with `--format json`)
- **Slash command** (`/witr`) — `/witr <target>` scans the target and opens a focused detail overlay over the current screen.
- **Published topic** — `witr.snapshot` (msgpack-encoded `WitrSnapshot`), emitted on each fresh scan from a live host channel. The plugin subscribes to nothing.

## Source

`crates/ainb-plugin-witr` — detects the external `witr` binary, owns the `ainb witr` CLI + `/witr` slash via `witr --json` exec-and-parse, publishes `witr.snapshot`, and renders the missing/outdated empty state; the foreign-TTY screen handoff (`w` → `witr -i`) is wired in `ainb-core`. Diagram generated via /fireworks-tech-graph.
