---
title: "agents-in-a-box — documentation"
---

Canonical source of truth for the monorepo. Everything published to the website
under `/docs/*` is rendered from these files.

If you're looking for a specific topic, start here:

| Section | What's in it |
|---|---|
| [Product](#product) | What agents-in-a-box is, value, high-level architecture |
| [TUI](#tui) | The `ainb` terminal app + CLI |
| [Toolkit](#toolkit) | Portable skills, agents, workflows (ainb-toolkit external repo) |
| [Plugins](#plugins) | v2 subprocess plugin system |
| [Knowledge](#knowledge) | `reflect` / `recall` GraphRAG + QMD |
| [Contributing](#contributing) | Build, test, ship |
| [Reference](#reference) | Architecture, glossary, deep dives |

---

## Product

- [What is agents-in-a-box?](product/what-is-ainb.md)
- [Value proposition](product/value.md)
- [Whole-system architecture](product/architecture.md)

## TUI

The `ainb` terminal app and its CLI.

- [Overview](tui/overview.md)
- [Install](tui/install.md)
- [First session quickstart](tui/quickstart.md)
- [Attaching to sessions](tui/attach.md) — full-screen and in-pane tmux attach
- [CLI reference](tui/cli.md) — every subcommand, every flag
- [Keyboard shortcuts](tui/keyboard-shortcuts.md)
- [Architecture](tui/architecture.md)
- [FAQ](tui/faq.md)

## Toolkit

Portable AI-coding agent toolkit. Skills, agents, workflows — deployed to 9 AI tools. The canonical source is the standalone [`stevengonsalvez/ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit) repo; ainb consumes it as a pinned external source.

- [Overview](toolkit/overview.md)
- [Skills (86)](toolkit/skills.md)
- [Agents (37)](toolkit/agents.md)
- [Bootstrap engine](toolkit/bootstrap.md)

## Plugins

> **Read this first if you're confused: [plugins/README.md](plugins/README.md)** — the word "plugin" means two different things in this repo. The index disambiguates.

The v2 subprocess plugin system for the ainb TUI:

- [What is an ainb plugin?](plugins/overview.md)
- [User guide](plugins/user-guide.md) — install, configure, troubleshoot
- [Authoring guide](plugins/authoring.md) — write your own
- [Wire spec v2](plugins/spec-v2.md) — the JSON-RPC contract
- [Changelog](plugins/changelog.md)

## Knowledge

The two-tier learning capture and retrieval system.

- [How reflection works](knowledge/overview.md)
- [`reflect` CLI reference](knowledge/reflect-cli.md)

## Contributing

- [Building from source](contributing/building.md)
- [CI / CD](contributing/ci-cd.md)
- [Release process](contributing/release-process.md)

## Reference

- [Architecture deep-dive](reference/architecture.md)
- [Glossary](reference/glossary.md)

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
- Cross-link other docs with relative paths (`[…](../plugins/spec-v2.md)`).
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
