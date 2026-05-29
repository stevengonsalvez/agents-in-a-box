---
title: "Glossary"
---

# Glossary

Definitions for the terms that recur across the agents-in-a-box docs. Where a term has a precise contract, the authoritative source is linked.

## Agents, sessions, worktrees

**Agent** — an AI coding harness instance (Claude Code, Codex CLI, GitHub Copilot, Gemini). agents-in-a-box is harness-agnostic: skills, agents, and plugins are deployed to each harness's config directory.

**Session** — a single conversation/run of an agent against a project. The TUI lists, attaches to, and recovers sessions; session logs are walked per-provider by the `session-reader` plugin.

**Worktree** — a git worktree giving a task its own isolated checkout and branch (architecture: worktree-per-task). Lets multiple agents work in parallel without colliding on the same working tree.

## Plugins

**Plugin (v2, subprocess)** — an ainb plugin that conforms to the [v2 contract](../plugins/spec-v2.md): a native executable spawned by the host as a child process, exchanging **JSON-RPC 2.0** over framed stdio (stdin = requests from host, stdout = responses + reverse-calls, stderr = host log output). Governed by capability grants in its manifest.

**Claude Code plugin** — a *different* concept: a Claude Code harness extension (e.g. the `reflect` plugin) that bundles skills, hooks, and adapters and is installed with `claude plugin install`. It does not implement the ainb v2 subprocess contract; the two senses of "plugin" are unrelated.

## Toolkit units

**Skill** — a structured workflow invoked by slash command (e.g. `/commit`, `/plan`), shared across harnesses from `toolkit/packages/skills/`.

**Agent (toolkit)** — a specialised sub-agent definition (e.g. `code-reviewer`, `distinguished-engineer`) from `toolkit/packages/agents/`, delegated to for focused work.

**Workflow** — a multi-phase orchestration that chains skills (plan → implement → validate) under a structured delivery process.

## See also

- [Architecture](./architecture.md)
- [Plugin contract — v2](../plugins/spec-v2.md)
- [Knowledge base overview](../knowledge/overview.md)
- [Docs hub](../README.md)