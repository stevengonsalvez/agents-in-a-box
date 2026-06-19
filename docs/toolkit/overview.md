---
title: "Toolkit overview"
---

:::note[The toolkit is now its own repo]
The portable toolkit moved out of this monorepo into the standalone
**[stevengonsalvez/ainb-toolkit](https://github.com/stevengonsalvez/ainb-toolkit)**
repo, where it is **flattened** at the repo root: skills at `skills/`, agents at
`agents/`, workflows at `workflows/`, utilities at `utilities/`, plus
`bootstrap.js`, `external-dependencies.yaml`, and `catalog.yaml`. `ainb`
consumes it as a pinned external source. The paths shown below use the legacy
`toolkit/packages/` form for continuity — in ainb-toolkit they live without the
`toolkit/packages/` prefix. See [Repositories](/reference/repositories/) for how
the two repos fit together.
:::

One canonical source tree, many AI tools. Author a skill, agent, or workflow once in the [ainb-toolkit](https://github.com/stevengonsalvez/ainb-toolkit) repo and `bootstrap.js` (or `ainb skill install`) deploys it into each supported tool's home directory.

## What lives in `toolkit/packages/`

| Path | Contents |
|---|---|
| `packages/skills/` | 91 agent-invokable `SKILL.md` bundles |
| `packages/agents/` | 37 agent definitions across 6 categories (`design/`, `engineering/`, `meta/`, `orchestrators/`, `swarm/`, `universal/`) |
| `packages/utilities/` | Shared `config/`, `hooks/`, `output-styles/`, `reflections/`, and `utils/` |
| `packages/workflows/single-agent/` | Guided plan -> implement -> validate flows |
| `packages/knowledge/` | Knowledge/docs templates |

The `reflect` Claude Code plugin and its companion knowledge-base CLI were extracted into their own repo, [stevengonsalvez/ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory) — the CLI flattened at that repo's root, the plugin under `plugin/`. They are no longer in this monorepo (or under `toolkit/packages/`). The reflect/recall CLI is installed via `uv tool install` (`uv tool install --upgrade 'git+https://github.com/stevengonsalvez/ainb-reflect-memory.git[graph]'`) by `bootstrap.js`.

## Supported tools

`bootstrap.js` defines 13 `TOOL_CONFIG` entries. Eleven are user-facing deploy targets; two (`claude` and `packages`) are internal aliases used by the rules/full-copy code paths.

| Tool key | Home dir | Notes |
|---|---|---|
| `claude-code-4.5` | `~/.claude/` | Primary. Plugins + npx-skills + agent-skills |
| `codex` | `~/.codex/` | `AGENTS.md` copied to project root |
| `copilot` | `~/.copilot/` | GitHub Copilot CLI |
| `gemini` | `~/.gemini/` | Gemini CLI |
| `hermes-agent` | `~/.hermes/` | Reads `~/.claude/skills/` via `external_dirs`; no direct external installs |
| `nanoclaw` | `~/.claude/` (shared) | Successor to OpenClaw, shares the Claude dir |
| `amazonq` | `PROJECT/.amazonq/rules/` | Project-scoped |
| `cursor` | `PROJECT/.cursor/rules/` | Project-scoped rule files |
| `cline` | `PROJECT/.clinerules/` | Project-scoped rule files |
| `roo` | `PROJECT/.roo/rules/` | Project-scoped rule files |
| `clawdhub` | `workspace/skills/` | Clawdhub skill format |

## How bootstrap deploys

```bash
cd toolkit && npm install
node bootstrap.js --tool=claude-code-4.5   # one tool
node bootstrap.js --tool=claude-code-4.5 --verify   # read-only parity check
```

Each target gets its own home-dir population; repeat per tool. See [Bootstrap engine](bootstrap.md) for the deploy mechanics.

## External dependencies

Externally-authored skills and plugins are declared in `toolkit/external-dependencies.yaml` and installed by the generated `setup-external.sh`. Sections include `agent-skills` (git clones), `npx-skills`, `claude-plugins`, `nanoclaw-skills`, `mcp-servers`, `mcporter-servers`, and `marketplaces`. Each entry's `applies-to` field controls which tools install it.

## Spec-Driven Development

Spec Kit integration drives a spec -> plan -> tasks flow via slash commands:

```bash
node bootstrap.js --sdd --targetFolder=<project>
```

Artifacts land under `specs/<feature-branch>/`.

## See also

- [Bootstrap engine](bootstrap.md)
- [Skills](skills.md)
- [Agents](agents.md)
- [Docs hub](../README.md)
