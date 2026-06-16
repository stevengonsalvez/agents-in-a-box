---
title: "Bootstrap engine"
---

`toolkit/bootstrap.js` deploys packages from `toolkit/packages/` into each supported tool's home directory. It is driven by a `TOOL_CONFIG` map (13 entries) plus an `external-dependencies.yaml` manifest.

## Per-tool home directories

Each `TOOL_CONFIG` entry sets a `targetSubdir` and a `ruleDir`. The eleven user-facing targets:

| Tool key | `targetSubdir` |
|---|---|
| `claude-code-4.5` | `.claude` |
| `codex` | `.codex` |
| `copilot` | `.copilot` |
| `gemini` | `.gemini` |
| `hermes-agent` | `.hermes` |
| `nanoclaw` | `.claude` (shared with Claude) |
| `amazonq` | `.amazonq/rules` |
| `cursor` | `.cursor/rules` |
| `cline` | `.clinerules` |
| `roo` | `.roo/rules` |
| `clawdhub` | `skills` |

Two additional internal entries, `claude` and `packages`, back the rules-glob and copy-entire-folder code paths and are not selected directly as deploy targets.

## Package mappings

For tools with `usePackagesStructure: true`, `packageMappings` controls which `packages/` subtrees land where. For `claude-code-4.5`:

| Source under `packages/` | Target under `~/.claude/` |
|---|---|
| `skills` | `skills` |
| `agents` | `agents` |
| `utilities/utils` | `utils` |
| `utilities/hooks` | `hooks` |
| `utilities/output-styles` | `output-styles` |
| `utilities/reflections` | `reflections` |

`codex` and `copilot` map only `skills` and `utilities/reflections` (no agents).

## Template substitutions

Each tool config declares `templateSubstitutions` keyed by glob (`**/*.md`, `**/*.sh`, `**/*.py`, `**/*.js`, `**/*.ts`, `**/*.json`, `**/*.yaml`, `**/*.yml`, `**/*.toml`). Two tokens are rewritten per tool so deployed files reference the correct home dir:

| Token | `claude-code-4.5` | `codex` | `copilot` |
|---|---|---|---|
| `TOOL_DIR` | `.claude` | `.codex` | `.copilot` |
| `HOME_TOOL_DIR` | `~/.claude` | `~/.codex` | `~/.copilot` |

This is why skills must use `{{TOOL_DIR}}` / `{{HOME_TOOL_DIR}}` placeholders rather than hardcoded `.claude` paths.

## `--verify` mode

```bash
node bootstrap.js --tool=<X> --verify
```

Read-only. Checks every applicable manifest entry has its `SKILL.md` at the expected path, and reports per-tool parity and orphans. Makes no changes.

## Adding a new target tool

1. Add a `TOOL_CONFIG` entry with `ruleDir`, `targetSubdir`, `packageMappings`, and `templateSubstitutions`.
2. If the tool consumes the manifest, add it to the relevant `applies-to` lists in `external-dependencies.yaml` (and to `TOOL_CANONICAL_NAMES` if its manifest name differs from its key).
3. Run `node bootstrap.js --tool=<new> --verify` to confirm parity.

## Machine-global external packages (CLIs)

On any home-scoped install (e.g. `claude-code-4.5`), bootstrap also installs the
machine-global CLIs declared under `external-packages:` in
`external-dependencies.yaml`. Each is idempotent — bootstrap detects an existing
install via `<cli> --version` and skips reinstall.

| CLI | installer | backs |
|---|---|---|
| `reflect` | `uv tool install …reflect-kb[graph]` | reflect plugin + retrieval KB |
| `ainb` | `brew tap stevengonsalvez/agents-in-a-box && brew install ainb` | ainb-fleet / ainb-hooks plugins (terminal dev-env manager) |

`ainb` falls back to the curl installer (`ainb-tui/install.sh`) when Homebrew is
absent. The same CLIs are refreshed by `toolkit/scripts/update-externals.sh packages`.
External *plugins* (Claude marketplace) are tracked under `claude-plugins:` and
installed via the generated `setup-external.sh` / `update-externals.sh plugins`.

## See also

- [Toolkit overview](overview.md)
- [Docs hub](../README.md)
