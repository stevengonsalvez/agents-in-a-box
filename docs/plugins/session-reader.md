---
title: "session-reader plugin"
description: "Pure-publisher reference plugin — scans per-provider session logs and chunk-publishes a UsageData snapshot on the event bus."
---

A pure data-plane plugin: it owns **no screen and no CLI namespace**. It walks the per-provider session-log directories, aggregates a `UsageData` snapshot, and chunk-publishes it on the `sessions.usage_data` topic for downstream consumers (`burndown`, and the host) to render. It is the canonical **publisher** example — the inverse of a screen-owning plugin, exercising only the event bus, log-read, and plugin-data capabilities.

## How it works

![session-reader plugin — how it works](../assets/diagrams/session-reader.svg)

The plugin never paints anything — its `Plugin::render` returns an empty `0×0` `WireBuffer` and the host's render path tolerates that. On `plugin/init` it does *not* publish; instead it subscribes (`host/snapshot/subscribe`) to two trigger topics and then sits idle. Publishing on startup would race the consumer's own subscription and lose chunks, so the design is strictly pull: a consumer asks, the plugin answers.

When a `plugin/handle_event` for `sessions.refresh_request` arrives, the plugin scans the four provider roots (`~/.claude/projects/**`, `~/.codex/sessions/**`, `~/.gemini/sessions`, `~/.config/github-copilot/sessions`, plus Cursor) on a blocking task, parses each file, and aggregates a `UsageData` snapshot. Parses are cached in a SQLite file under the plugin's own data dir — files whose `(mtime, size)` are unchanged short-circuit, so a warm rescan is cheap. While the cold scan runs it streams per-file progress on `sessions.scan_progress` so `burndown` can render a "Scanning sessions…" line.

The aggregated `UsageData` is then split by `chunk_usage_data` into one or more msgpack envelopes, each kept under ~2 MiB so the post-base64 JSON-RPC body stays well under the host framer's 16 MiB cap. Chunk 0 carries the bounded aggregates; follow-on chunks carry slices of the unbounded tail vecs (`calls`, `sessions`, `shell_commands`); the last chunk is flagged `is_final`. Each chunk is pushed via `host/snapshot/publish` on `sessions.usage_data`.

The host's event bus stores the latest publish per topic and fans `plugin/handle_event` deliveries to subscribers (`burndown`); the host itself can also pull the stored snapshot via `host/snapshot/get`. The `sessions.flush_cache_request` topic is the recovery escape hatch — it wipes the parse cache and forces a cold rescan (driven by `burndown`'s `F` key).

## Capabilities

These are the exact `[capabilities]` from `manifest.toml`. Every flag defaults to deny; the plugin grants only what its data plane needs.

| Capability | Value | Why it's needed |
|---|---|---|
| `read_claude_logs` | `true` | Read `~/.claude/projects/**/*.jsonl` session histories. |
| `read_codex_logs` | `true` | Read `~/.codex/sessions/**` rollout logs (also covers the Gemini/Copilot/Cursor roots the scanner walks). |
| `event_bus` | `true` | Subscribe to trigger topics and publish the snapshot + progress on the bus. |
| `write_plugin_data` | `true` | Persist the SQLite parse cache under the plugin's own data dir for warm rescans. |
| `network` | `[]` | No network access. |
| `spawn_subprocess` | `false` | No child processes. |

## Using it

There is **nothing to open and nothing to type** — by design. The plugin provides no screen, no slash command, and no `ainb <ns> ...` CLI namespace. You interact with it only indirectly, through the topics it publishes and subscribes:

- **Publishes** (`[provides] snapshots`): `sessions.usage_data` — the chunked `UsageData` snapshot consumed by `burndown` (and the host). It also publishes `sessions.scan_progress` while a cold scan is in flight.
- **Subscribes** (`[subscribes] snapshots`): `sessions.refresh_request` (any publisher forces a re-scan + re-publish) and `sessions.flush_cache_request` (wipe the parse cache, then cold re-scan — backing `burndown`'s `F` key).
- **Lifecycle**: declared `spawn = "eager"` so the first snapshot is ready before `burndown` asks; idle-reaped after 600s.

To see its output, open the **Analytics** screen owned by `burndown` (or run `ainb usage`) — that screen renders the snapshot this plugin produces.

## Source

`crates/ainb-plugin-session-reader` — the data backend for ainb's usage analytics: scans provider session logs, caches parses, and chunk-publishes the `UsageData` snapshot. Diagram generated via /fireworks-tech-graph.
