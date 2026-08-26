---
title: "agents-in-a-box — documentation"
---

Canonical source of truth for the monorepo. Everything published to the website
under `/docs/*` is rendered from these files.

If you're looking for a specific topic, start here:

| Section | What's in it |
|---|---|
| [Product](#product) | What agents-in-a-box is, value, high-level architecture |
| [TUI](#tui) | The `ainb` terminal app |
| [CLI](#cli) | Every subcommand, every flag |
| [Fleet](#fleet) | Running many agents at once: attention, the chat bridge, ATC, cost |
| [Hangar](#hangar) | The managed-agents control plane |
| [Skill manager](#skill-manager) | Install, sync and promote units across tool homes |
| [Toolkit](#toolkit) | Portable skills, agents, workflows (external repo) |
| [Plugins](#plugins) | v2 subprocess plugin system |
| [Observability](#observability) | Usage/cost, live processes, causality, telemetry |
| [Knowledge](#knowledge) | `reflect` / `recall` GraphRAG + QMD |
| [Contributing](#contributing) | Build, test, ship |
| [Reference](#reference) | Architecture, glossary, repositories |
| [Internal](#internal) | Plans, specs and research, not published to the site |

---

## Product

- [What is agents-in-a-box?](product/what-is-ainb.md)
- [Value proposition](product/value.md)
- [Whole-system architecture](product/architecture.md)

## TUI

The `ainb` terminal app.

- [Overview](tui/overview.md)
- [Install](tui/install.md)
- [First session quickstart](tui/quickstart.md) — start here
- [Starting a new session](tui/start-session.md)
- [Attaching to sessions](tui/attach.md) — full-screen and in-pane tmux attach
- [Code review (diff)](tui/code-review.md)
- [Shared MCP pool](tui/mcp-pool.mdx)
- [Token optimisation — Headroom & RTK](tui/token-optimization.mdx)
- [Daemons overlay](tui/daemons.mdx)
- [Inbox & notifications](tui/inbox-notifications.md)
- [Browser dashboard (`ainb web`)](tui/web.md)
- [Keyboard shortcuts](tui/keyboard-shortcuts.md)
- [Architecture](tui/architecture.md)
- [FAQ](tui/faq.md)

## CLI

- [Full CLI reference](tui/cli.md) — generated from `--help`, drift-checked in CI

## Fleet

Running more than one agent at a time.

- [Chat bridge](fleet-bridge.md) — drive the fleet from Telegram, Slack or Discord
- [ATC](atc-plumbing.md) — the always-on watcher and its session-lifecycle plumbing
- [Fleet cost rollups](tui/fleet-cost.md) — spend across every session, with budget alerts

## Hangar

The managed-agents control plane: boards, tasks, squads, autopilots.

- [Architecture & features](hangar/architecture.md)

> The rest of `hangar/` is the build record — the original proposal, phase
> plans, verification goals and the Multica research that produced them. It is
> kept for provenance and is not published to the site. See
> [hangar/README.md](hangar/README.md).

## Skill manager

Install, sync and promote skills, agents and commands across your tool homes.

- [Guide](skill-manager/guide.mdx) — start here
- [Discovery & import](skill-manager/discovery.md) — adopt what is already on disk
- [Catalog browse](skill-manager/browse.md)
- [Sync](skill-manager/sync.md) — reconcile home and repo
- [Drift check](skill-manager/check.md)
- [Usage tracking](skill-manager/usage.md)
- [Promote](skill-manager/promote.md) — turn a local unit into a git-backed source
- [Sandbox testing](skill-manager/sandbox-testing.md)

## Toolkit

Portable AI-coding agent toolkit. Skills, agents and workflows deployed to 9 AI
tools. The canonical source is the standalone
[`stevengonsalvez/ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit)
repo; ainb consumes it as a pinned external source.

- [Overview](toolkit/overview.md)
- [Skills (94)](toolkit/skills.md)
- [Agents (16)](toolkit/agents.md)
- [Bootstrap engine](toolkit/bootstrap.md)

Claude Code plugins that ship alongside it:

- [Overview](toolkit/plugins/overview.md) — how these differ from ainb plugins
- [`reflect`](toolkit/plugins/reflect.md) — learning capture and recall
- [`ainb-fleet`](toolkit/plugins/ainb-fleet.md) — the fleet orchestration skills
- [`ainb-hooks`](toolkit/plugins/ainb-hooks.md) — lifecycle events into Hangar and the Inbox

## Plugins

> **Read this first if the word is confusing: [plugins/README.md](plugins/README.md).**
> "Plugin" means two different things in this repo, and that page disambiguates.

The v2 subprocess plugin system for the ainb TUI:

- [What is an ainb plugin?](plugins/overview.md)
- [User guide](plugins/user-guide.md) — install, configure, troubleshoot
- [Authoring guide](plugins/authoring.md) — write your own
- [Wire spec v2](plugins/spec-v2.md) — the JSON-RPC contract
- [Changelog](plugins/changelog.md)

In-tree plugins:

- [burndown](plugins/burndown.md) — the analytics screen and `ainb usage`
- [session-reader](plugins/session-reader.md) — the silent backend behind burndown's numbers
- [witr](plugins/witr.md) — process-causality tracing
- [learnings](plugins/learnings.md) — browse and search the knowledge base
- [abtop](plugins/abtop.md) — live agent-process monitor

## Observability

See what your agents are doing, spending and running.

- [Overview](observability/overview.md) — which tool answers which question
- [Usage analytics (burndown)](plugins/burndown.md) — spend by day, project and model
- [Fleet cost rollups](tui/fleet-cost.md) — spend across the whole fleet
- [abtop](plugins/abtop.md) — live agent-process monitor
- [witr](plugins/witr.md) — process-causality tracing
- [OpenTelemetry to Grafana Cloud](reference/otel-grafana.md) — ship metrics, logs and traces off-box

## Knowledge

The two-tier learning capture and retrieval system.

- [How reflection works](knowledge/overview.md)
- [`reflect` CLI reference](knowledge/reflect-cli.md)
- [Hooks & platform](knowledge/hooks-and-platform.md) — Claude, Codex and Copilot wiring

Reflect memory, in depth:

- [Problem & fit](knowledge/reflect-memory/problem-and-fit.md)
- [The construct](knowledge/reflect-memory/construct.md)
- [Recall reference](knowledge/reflect-memory/recall.md)
- [Why build, not adopt](knowledge/reflect-memory/comparison.md)
- [Memory browser (`reflect serve`)](knowledge/reflect-memory/serve.md)

## Contributing

- [Building from source](contributing/building.md)
- [CI / CD](contributing/ci-cd.md)
- [Verifying on a loaded box](contributing/verifying-on-a-loaded-box.md)
- [Release process](contributing/release-process.md)

## Reference

- [Architecture deep-dive](reference/architecture.md)
- [Glossary](reference/glossary.md)
- [Repositories](reference/repositories.md) — which repo holds what

## Internal

Written for whoever is building the thing, not for a reader of the site, so
these are excluded from the site build. They stay in the repo for provenance.

| Directory | What it holds |
|---|---|
| `plans/` | Dated implementation plans for in-flight work |
| `contracts/` | Implementer specs, e.g. the macOS fleet daemon contract |
| `explorations/` | Comparative research notes |
| `solutions/` | Troubleshooting and postmortem notes |
| `hangar/` | The Hangar build record, except `hangar/architecture.md` |

Current plans:

- [Buzz port part 1 — daemon chat bus + ACP adapter](plans/2026-07-31-buzz-port-01-chat-bus-acp.md)
- [Buzz port part 2 — fleet chat + copilot](plans/2026-07-31-buzz-port-02-fleet-chat-copilot.md) · [spec](plans/2026-07-31-buzz-port-02-fleet-chat-copilot-spec.md)
- Research: [porting block/buzz into ainb (discussion #570)](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570)
- Explainer: [buzz to ainb port research](https://explainers.stevengonsalvez.com/buzz-acp-port/)

---

## How this tree maps to the legacy layout

The pre-restructure layout had docs scattered across three places. Here's the mapping:

| New location | Source (legacy) |
|---|---|
| `docs/tui/cli.md` | `ainb-tui/docs/CLI.md` |
| `docs/tui/faq.md` | `ainb-tui/docs/FAQ.md` |
| `docs/toolkit/overview.md` | `toolkit/README.md` (TOC + intro — the toolkit itself has since moved to the standalone `stevengonsalvez/ainb-toolkit` repo) |
| `docs/plugins/overview.md` | new — disambiguates |
| `docs/plugins/user-guide.md` | `docs/plugins.md` |
| `docs/plugins/authoring.md` | `docs/plugin-authoring.md` |
| `docs/plugins/spec-v2.md` | `docs/plugin-spec/v2.md` |
| `docs/plugins/changelog.md` | `docs/plugin-spec/CHANGELOG.md` |
| `docs/knowledge/overview.md` | `docs/how-reflection-works.md` |

The migration is complete: every page in this tree is now authoritative and the
legacy paths have been removed. The table above is retained only to document
provenance.

---

## Editing rules

- Markdown is the source format. Astro Starlight renders to HTML at build time.
- Code blocks: always tag the language for syntax highlighting.
- Cross-link other docs with relative paths, resolved from the linking file (`[…](plugins/spec-v2.md)` from here, `[…](../plugins/spec-v2.md)` from inside `tui/`).
- Heading style: `#` once per file (title), then `##` and below for sections.
- Frontmatter (Starlight-style) only on files that override the title or sidebar position:

  ```yaml
  ---
  title: "Plugin authoring guide"
  sidebar:
    order: 2
  ---
  ```

- Use admonitions sparingly: `> [!note]`, `> [!warning]`, `> [!tip]`.
