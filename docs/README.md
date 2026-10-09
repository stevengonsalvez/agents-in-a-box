---
title: "agents-in-a-box: documentation"
---

Canonical source of truth for the monorepo. Everything published to the website under `/docs/*` is rendered from these files.

## 1. Start here

- [Overview](/product/what-is-ainb): what ainb is, who it is for, component breakdown
- [Install](/tui/install): Homebrew, curl script, prebuilt binaries, cargo build
- [Quickstart](/tui/quickstart): first session in 60 seconds
- [Concepts](/product/concepts): workspaces, worktree isolation, tmux persistence, attention
- [Keyboard shortcuts](/tui/keyboard-shortcuts): complete keybindings table

## 2. Using ainb

- [Starting a new session](/tui/start-session): repo picker and new-session wizard
- [Attaching to sessions](/tui/attach): full-screen and in-pane live tmux attach
- [Code review diff](/tui/code-review): Warp-style diff viewer with collapsible hunks
- [Fleet and chat bridge](/fleet-bridge): drive the fleet from Telegram, Slack or Discord
- [ATC background watcher](/atc-plumbing): always-on watcher and session lifecycle
- [Shared MCP pool](/tui/mcp-pool): shared MCP server pool across sessions
- [Token optimisation](/tui/token-optimization): Headroom proxy and RTK context compression
- [Inbox and notifications](/tui/inbox-notifications): ainb-hooks notification feed
- [Browser dashboard](/tui/web): read-only web view (`ainb web`)

## 3. Configure and Extend

- [Value proposition](/product/value): detailed comparison and workflow impact
- [Plugins overview](/plugins/overview): subprocess plugin system v2
- [Plugins user guide](/plugins/user-guide): installing and configuring plugins
- [In-tree plugins](/plugins/overview):
  - [burndown](/plugins/burndown): cost and spend tracking
  - [session-reader](/plugins/session-reader): JSONL event streaming
  - [witr](/plugins/witr): process causality tree
  - [learnings](/plugins/learnings): knowledge graph (guide moved to the [reflect docs site](https://stevengonsalvez.github.io/ainb-reflect-memory/guides/tui-learnings-plugin/))
  - [abtop](/plugins/abtop): fleet process monitor
- [Skill manager guide](/skill-manager/guide): discovery, sync, drift check, and promotion
- [Toolkit overview](/toolkit/overview): portable skills and agents across 9 tools
- [Reflect memory overview](/knowledge/overview): GraphRAG and QMD knowledge capture
- [Memory browser](/knowledge/reflect-memory/serve): `reflect serve` web interface (moved to the [reflect docs site](https://stevengonsalvez.github.io/ainb-reflect-memory/guides/memory-browser/))

## 4. Reference

- [CLI reference](/tui/cli): generated from `--help`, verified in CI
- [Plugin authoring guide](/plugins/authoring): build native binary plugins
- [Plugin wire spec v2](/plugins/spec-v2): JSON-RPC protocol specification
- [Fleet cost rollups](/tui/fleet-cost): spend across sessions and budget alerts
- [Observability overview](/observability/overview): telemetry and causality
- [OpenTelemetry to Grafana](/reference/otel-grafana): Prometheus and dashboard setup
- [Architecture deep-dive](/reference/architecture): whole-system architecture
- [TUI architecture](/tui/architecture): ratatui host and event loop
- [Repositories map](/reference/repositories): monorepo crate map
- [Hangar control plane](/hangar/architecture): task boards and autopilots
- [Reflect memory internals](/knowledge/reflect-memory/problem-and-fit) (these pages moved to the [reflect docs site](https://stevengonsalvez.github.io/ainb-reflect-memory/)):
  - [The construct](/knowledge/reflect-memory/construct)
  - [Recall reference: 57 ports](/knowledge/reflect-memory/recall)
  - [Comparison](/knowledge/reflect-memory/comparison)

## 5. Help

- [Troubleshooting and FAQ](/tui/faq): common questions and fixes
- [Daemons overlay](/tui/daemons): diagnostic overlay for background services
- [Contributing guide](/contributing/building): build, test, CI/CD, and release
- [Glossary](/reference/glossary): terms and definitions
