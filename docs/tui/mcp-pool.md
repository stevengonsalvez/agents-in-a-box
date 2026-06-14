---
title: "Shared MCP pool"
description: "ainb runs one MCP server process per server name, shared across every host session over a unix socket — so a swarm of Claude/Codex/Copilot sessions stops spawning a node/bun process per session per MCP. Configure with [mcp_pool], import from .mcp.json, install for other agent CLIs."
---

Every Claude Code session normally spawns its **own** node/bun process for each configured MCP server. Run a swarm of sessions and that multiplies into dozens of processes hogging CPU and RAM ([anthropics/claude-code#45880](https://github.com/anthropics/claude-code/issues/45880) reports kernel panics from 510 node processes). The **shared MCP pool** fixes this: a standalone `ainb mcp daemon` spawns each MCP server **once** behind a unix socket, and every session attaches through a tiny `ainb mcp proxy` stdio shim. N sessions, one backend process.

![ainb shared MCP pool — a project with only a .mcp.json (context7 via npx); `ainb mcp import` makes it poolable; the daemon starts; two independent sessions attach and both get real context7 tools; `ainb mcp status` shows clients: 2 sharing one child_pid; the process group proof shows a single shared context7 server for both sessions](../assets/screenshots/mcp-pool.gif)

*Two sessions attach to a real context7 server and both receive its tools (`resolve-library-id`, `query-docs`). `ainb mcp status` reports `clients: 2` against one `child_pid` — a single shared process group. Without the pool, those two sessions would spawn two separate context7 servers.*

## Enable it

- It's **on by default** — `[mcp_pool].enabled = true`. There's nothing to turn on for a basic setup.
- Start a Claude session the normal way: `ainb run --repo . --worktree`.
- The daemon auto-starts on first use; you never launch it by hand.
- Toggle it in the TUI under **Configuration → MCP Pool**, or in `config.toml`:

```toml
[mcp_pool]
enabled = true
idle_grace_secs = 300
```

## Use it

- **Already have a `.mcp.json`?** Do nothing — its stdio servers are auto-imported into the pool at session create.
- **No config yet?** Add a `.mcp.json`, or define `[mcp_servers.<name>]` in `config.toml` — either works.
- **Persist a one-off `.mcp.json` into config:** `ainb mcp import` (or `ainb mcp import --user`).
- **Check it's sharing:** `ainb mcp status` → look for `"clients": N` against one `"child_pid"`.
- **Share with Codex/Copilot too:** `ainb mcp install --codex --copilot`.
- **Stop the pool:** `ainb mcp stop`.
- **Don't pool a stateful server** (browser/db bridge): set `shared = false` on its `[mcp_servers.<name>]` entry.

## How it works

```
 session A ──stdio── ainb mcp proxy ──┐
 session B ──stdio── ainb mcp proxy ──┼─ unix socket ─ ainb mcp daemon ─ 1× context7 (npx)
 session C ──stdio── ainb mcp proxy ──┘   (id-rewrite mux, init cache, refcount)
```

The daemon multiplexes all clients onto the one child: it rewrites each session's JSON-RPC request ids so they never collide, answers every session's `initialize` from a cached copy of the backend's `InitializeResult` (the child sees exactly one initialize), and routes progress notifications back to the session that asked. When the last session detaches, the child is reaped after `idle_grace_secs`. If anything fails — daemon down, server not on PATH — the session silently falls back to spawning its own MCP, so a session never fails to start because of the pool.

Host/tmux sessions only. Docker sessions keep their per-container MCP init.

## Configuration

Pool settings and per-server opt-out live in `config.toml` (`~/.agents-in-a-box/config/config.toml`, or per-repo `.ainb/config.toml`), or in the TUI under **Configuration → MCP Pool**:

```toml
[mcp_pool]
enabled = true          # default true
idle_grace_secs = 300   # reap a pooled server N seconds after its last session detaches

[mcp_servers.context7]
name = "context7"
description = "docs server"
enabled_by_default = true
shared = true           # set false for stateful servers (browser/db bridges) → per-session spawn
installation = { type = "PreInstalled" }
definition = { type = "Command", command = "npx", args = ["-y", "@upstash/context7-mcp"] }
```

## You don't have to hand-write that

Stdio servers already declared in a worktree's `.mcp.json` are **auto-imported** into the pool at session create — push them once, every session shares them. To persist definitions explicitly:

```bash
ainb mcp import          # .mcp.json + Claude user-scope → .ainb/config.toml
ainb mcp import --user   # …into the user-level config instead
```

## Codex & Copilot

The shim is just a stdio command, so any agent CLI can share the same backend processes. One command wires their global MCP configs (with a `.bak` backup):

```bash
ainb mcp install --codex --copilot
```

This writes shim entries into `~/.codex/config.toml` and `~/.copilot/mcp-config.json` pointing at the pool sockets. A Codex session, a Copilot session, and three Claude sessions then all funnel into the same context7 process.

## Commands

| Command | What it does |
|---------|--------------|
| `ainb mcp daemon` | Run the pool daemon in the foreground (auto-spawned detached by `ainb run`) |
| `ainb mcp status` | Per-server JSON: client count, shared child pid, state |
| `ainb mcp stop` | Stop the daemon and its pooled children |
| `ainb mcp import [--user]` | Import stdio servers from `.mcp.json` / Claude user scope into config |
| `ainb mcp install --codex --copilot` | Point other agent CLIs at the pool shim |
| `ainb mcp proxy <socket>` | The stdio↔socket shim (used inside generated `.mcp.json`; you won't call it directly) |

## Verify it yourself

The GIF above is recorded from a reproducible demo that uses **real** context7:

```bash
cargo build --release
AINB_BIN=ainb-tui/target/release/ainb scripts/mcp-pool-demo.sh
```

A heavier end-to-end check spins up three real ainb Claude sessions and asserts one backend process, three shim attachments, a working tool call from every session, kill-one-survives resilience, and post-grace reaping:

```bash
scripts/validate-mcp-pool.sh 3
```

## See also

- [CLI reference](cli.md)
- [Overview](overview.md)
