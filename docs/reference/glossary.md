---
title: "Glossary"
---

Definitions for the terms that recur across the agents-in-a-box docs. Where a term has a precise contract, the authoritative source is linked.

## Agents, sessions, worktrees

**Agent** — an AI coding harness instance (Claude Code, Codex CLI, GitHub Copilot, Gemini). agents-in-a-box is harness-agnostic: skills, agents, and plugins are deployed to each harness's config directory.

**Session** — a single conversation/run of an agent against a project. The TUI lists, attaches to, and recovers sessions; session logs are walked per-provider by the `session-reader` plugin.

**Worktree** — a git worktree giving a task its own isolated checkout and branch (architecture: worktree-per-task). Lets multiple agents work in parallel without colliding on the same working tree.

## Plugins

**Plugin (v2, subprocess)** — an ainb plugin that conforms to the [v2 contract](../plugins/spec-v2.md): a native executable spawned by the host as a child process, exchanging **JSON-RPC 2.0** over framed stdio (stdin = requests from host, stdout = responses + reverse-calls, stderr = host log output). Governed by capability grants in its manifest.

**Claude Code plugin** — a *different* concept: a Claude Code harness extension (e.g. the `reflect` plugin) that bundles skills, hooks, and adapters and is installed with `claude plugin install`. It does not implement the ainb v2 subprocess contract; the two senses of "plugin" are unrelated.

## Toolkit units

**Skill** — a structured workflow invoked by slash command (e.g. `/commit`, `/plan`), shared across harnesses from `skills/` in the [`stevengonsalvez/ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit) repo.

**Agent (toolkit)** — a specialised sub-agent definition (e.g. `code-reviewer`, `distinguished-engineer`) from `agents/` in the [`stevengonsalvez/ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit) repo, delegated to for focused work.

**Workflow** — a multi-phase orchestration that chains skills (plan → implement → validate) under a structured delivery process.

## Knowledge base

**QMD** — the vector / semantic search engine of the knowledge base; answers "what matches?" via embedding similarity (BM25 + vector hybrid).

**GraphRAG** — the graph search engine; answers "what's connected?" by traversing the entity-relationship graph. Built with `nano-graphrag` using a passthrough LLM that consumes pre-extracted entity sidecars (no external LLM calls during indexing).

**Sidecar** — an `.entities.yaml` file sitting next to a knowledge document (`doc.md` + `doc.entities.yaml`). It carries the pre-extracted entities and relationships that feed GraphRAG indexing directly.

**Community report** — a GraphRAG-generated summary of a cluster (community) of related entities, used to answer higher-level questions about a connected region of the graph.

## Terminal & tmux

**tmux** — terminal multiplexer used to run persistent, detachable agent and dev-server sessions. Sessions survive disconnects and are reattached with `tmux attach -t <session>`.

**PTY** — pseudo-terminal; the kernel device pair that lets the TUI and tmux drive a child process as if it had a real terminal (handles resize, raw input, ANSI output).

**Pane** — a single rectangular split inside a tmux window running one shell/process.

**Window** — a full-screen tab within a tmux session; a window contains one or more panes.

## Plugin contract & runtime

**ABI** — Application Binary Interface; the host/plugin runtime contract version. A v2 plugin declares `abi_version = 2` in its manifest and must match the host's `ABI_VERSION` (currently `2`). See [spec-v2 §5.4](../plugins/spec-v2.md).

**Wire version** — the schema version of a snapshot/event type. Plugin types crates (e.g. `ainb-plugin-types-sessions`) carry a `pub const WIRE_VERSION: u32` (currently `3`); subscribers must check `event.version == WIRE_VERSION` and reject mismatches gracefully rather than panicking.

**CTS (Conformance Test Suite)** — the executable form of the plugin spec. `ainb-plugin-cts-v2` covers **14 axes** (manifest round-trip, framing, method dispatch, capability gating, render determinism, snapshot pub/sub, action timeout, log filtering, fs path guard, graceful shutdown, crash recovery, quarantine, CLI dispatch capture, chunked publish ordering). A plugin author runs it via `cargo test` for a per-axis pass/fail report.

**Canary** — a minimal plugin written to exercise exactly one CTS axis, living at `crates/ainb-plugin-cts-v2/tests/canaries/<axis>/`.

**`cts-v2`** — the `ainb-plugin-cts-v2` crate: the host-side conformance test runner (14 axes) plus its canary plugins.

**`testkit`** — the `ainb-plugin-testkit` crate: an in-process test harness for plugin authors. A plugin author hands their `Plugin` impl to testkit to exercise it without spawning a real subprocess.

**`notifyd`** — the `ainb-plugin-notifyd` crate: the ainb-owned notification daemon. Listens on a Unix domain socket at `~/.agents-in-a-box/notify.sock` and is one of the in-tree v2 reference plugins (alongside `burndown` for analytics and `session-reader` for the session data backend).

## See also

- [Architecture](./architecture.md)
- [Plugin contract — v2](../plugins/spec-v2.md)
- [Knowledge base overview](../knowledge/overview.md)
- [Docs hub](../README.md)