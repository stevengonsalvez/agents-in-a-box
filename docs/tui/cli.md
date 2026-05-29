---
title: "ainb CLI Reference"
---

# ainb CLI Reference

`ainb` is both an interactive terminal UI and a scriptable CLI. Every session operation the TUI can perform is also exposed as a subcommand, so agents and automation can drive it end-to-end without opening the UI.

Run `ainb` with no arguments to launch the TUI, or use any subcommand below for non-interactive work.

> **Version documented:** `ainb 1.2.0`
> **Source of truth:** output of `ainb <cmd> --help`. If this doc drifts, trust `--help`.

---

## Table of Contents

- [Global flags](#global-flags)
- [Shell completions](#shell-completions)
- [Exit codes](#exit-codes)
- [File layout](#file-layout)
- [Environment variables](#environment-variables)
- [Command reference](#command-reference)
  - [`tui`](#ainb-tui) — launch the TUI (default)
  - [`run`](#ainb-run) — spawn a new AI coding session
  - [`list`](#ainb-list) — list sessions
  - [`status`](#ainb-status) — inspect a session
  - [`logs`](#ainb-logs) — stream or tail session output
  - [`attach`](#ainb-attach) — drop into a session's tmux
  - [`kill`](#ainb-kill) — terminate a session
  - [`auth`](#ainb-auth) — set up authentication
  - [`recover`](#ainb-recover) — recover orphaned sessions
  - [`config`](#ainb-config) — manage configuration
  - [`git`](#ainb-git) — worktree operations
  - [`favorites`](#ainb-favorites) — saved repositories
  - [`init`](#ainb-init) — first-time setup & factory reset
  - [`presets`](#ainb-presets) — session presets
  - [`claudecode`](#ainb-claudecode) — Claude Code provider-specific commands (statusline)
  - [`completion`](#ainb-completion) — shell completions
  - [`plugin`](#ainb-plugin) — manage ainb plugins
  - [`fleet`](#ainb-fleet) — orchestrate the claude session fleet
  - [`usage`](#ainb-usage) — local usage analytics and export
- [Scripting recipes](#scripting-recipes)

---

## Global flags

Every command accepts:

| Flag | Values | Default | Purpose |
|------|--------|---------|---------|
| `--format` | `text`, `json`, `csv` | `text` | Switch output format. `csv` is mainly for `ainb usage export`. |
| `-h`, `--help` | — | — | Print help for the command or subcommand. |
| `-V`, `--version` | — | — | Print `ainb` version (top level only). |

`ainb` follows standard clap conventions: invalid enum values (e.g. `--tool foobar`) are rejected at parse time with a `[possible values: …]` hint and exit status 2.

---

## Shell completions

```bash
# Generate completions for your shell and source them
ainb completion zsh > ~/.zsh/completions/_ainb
ainb completion bash > /usr/local/etc/bash_completion.d/ainb
ainb completion fish > ~/.config/fish/completions/ainb.fish
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

---

## Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Success |
| `1`  | Runtime error — missing session, failed tmux op, config parse error, etc. Error message on stderr. |
| `2`  | Usage error — unknown flag, invalid enum value, missing required argument. Handled by clap. |

---

## File layout

`ainb` stores state under `~/.agents-in-a-box/` and reads/writes a handful of Claude-specific files under `~/.claude/`:

```
~/.agents-in-a-box/
├── sessions.json                    # Session registry (read by list/status/kill/recover)
├── config/config.toml               # User-level config (written by `config set` / `config edit`)
├── worktrees/
│   ├── by-session/<session-id>      # Symlink → real worktree path
│   └── <branch-name>/               # Actual worktree directories
├── repo-cache/                      # Cloned remote repos (populated by `run --remote-repo`)
├── favorites.json                   # Favorite repositories (managed by `favorites`)
└── presets/<name>.toml              # Custom presets (managed by `presets`)

./.agents-box/
├── config.toml                      # Project-level config override (takes precedence)
└── preset.toml                      # Active preset (written by `presets apply`)

~/.claude/
├── claude.json                      # Claude CLI state (updated by `run`)
└── agents/<session-id>.json         # Agent session metadata (scanned by `recover`)
```

Configuration precedence (highest first):
1. `./.agents-box/config.toml` (project)
2. `~/.agents-in-a-box/config/config.toml` (user)
3. `/etc/agents-in-a-box/config.toml` (system)

---

## Environment variables

| Variable | Effect |
|----------|--------|
| `EDITOR` | Used by `ainb config edit` to open the user config. Falls back to `vi`. |
| `HOME` | All paths under `~/.agents-in-a-box/` and `~/.claude/` resolve through `$HOME`. Override to run `ainb` against an isolated sandbox (see `scripts/cli-smoke-test.sh`). |
| `TMUX_TMPDIR` | Redirects the tmux socket directory. Useful for isolation testing so sessions don't appear in the user's default `tmux ls`. |
| `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `GEMINI_API_KEY` / `GITHUB_COPILOT_TOKEN` | Honored by the respective AI providers when `--tool` selects them. |

---

## Command reference

### `ainb tui`

Launch the TUI. This is the default when `ainb` is invoked with no subcommand.

```bash
ainb            # equivalent to `ainb tui`
ainb tui
```

No flags beyond the global `--format` / `--help`.

---

### `ainb run`

Spawn a new AI coding session in an isolated worktree + tmux pane.

```bash
ainb run [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--repo <PATH>` | Local repository path to use. |
| `--remote-repo <OWNER/REPO \| URL>` | Clone this repo into `~/.agents-in-a-box/repo-cache/` first. |
| `--create-branch <NAME>` | Create a new branch with this name in the worktree. |
| `--worktree` | Use a git worktree for isolation (recommended). |
| `--tool <TOOL>` | AI tool. One of `claude` (default), `codex`, `gemini`, `copilot`. Invalid values rejected at parse. |
| `--model <MODEL>` | Model identifier. Default `sonnet`. Common values: `sonnet`, `opus`, `haiku`. |
| `-p, --prompt <TEXT>` | Initial prompt sent to the agent after startup. |
| `-a, --attach` | Drop into the tmux session after creation. |
| `-i, --interactive` | Run in interactive mode (spawn tmux and attach). |
| `--dangerously-skip-permissions` | Pass the provider's "skip permission prompts" flag. **Dangerous** — the agent will not confirm destructive operations. |
| `--name <NAME>` | Custom session/workspace name (otherwise auto-generated from the repo + branch). |

**Examples**

```bash
ainb run --repo .                                  # Use current directory
ainb run --repo . --worktree                       # Isolate in a new worktree
ainb run --repo . --create-branch feat/new         # Create a branch + worktree
ainb run --remote-repo owner/repo                  # Clone from GitHub first
ainb run --tool codex --repo .                     # Use Codex instead of Claude
ainb run --repo . -p "fix the failing tests"       # Seed an initial prompt
ainb run --repo . --attach                         # Drop into tmux after creating
```

The provider binary (`claude`, `codex`, `gemini`, `copilot`) must be on `$PATH`. If it isn't, `run` exits with `1` and a clear message (`codex: not found on PATH`).

---

### `ainb list`

List sessions from the registry.

```bash
ainb list [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--running` | Only sessions whose tmux is currently alive. |
| `--workspace <NAME>` | Filter by workspace name. |
| `--format json` | Emit the registry as JSON for piping to `jq`. |

**Example**

```bash
ainb list --format json | jq '.[] | {name: .workspace_name, tool: .agent_type, branch}'
```

### `ainb usage`

Read-only local usage analytics for Claude Code and Codex session histories. AINB reads `~/.claude/projects/**/*.jsonl` plus `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` or `$CODEX_HOME/sessions/...`. It does not modify session files. Cost values are estimates from built-in model pricing; unknown models still report tokens and calls.

```bash
ainb usage report --period week
ainb usage report --from 2026-04-01 --to 2026-04-10 --format json
ainb usage report --provider codex --include agents-in-a-box
ainb usage report --exclude scratch --format json
ainb usage status --format json
ainb usage today --format json
ainb usage month --format json
ainb usage export --format csv --output /tmp/ainb-usage.csv
ainb usage export --format csv --output /tmp/ainb-usage-export
ainb usage optimize --period 30days
ainb usage compare --period all --format json
ainb usage yield --period week
```

Usage filters:

| Flag | Values | Purpose |
|------|--------|---------|
| `--period` | `today`, `week`, `30days`, `month`, `all` | Select time window. |
| `--from`, `--to` | `YYYY-MM-DD` | Custom inclusive range. One bound may be omitted. |
| `--provider` | `all`, `claude`, `codex` | Provider filter. |
| `--include`, `--project` | repeatable text | Include projects whose name/path contains text. |
| `--exclude` | repeatable text | Exclude projects whose name/path contains text. Exclusion wins. |

Usage settings:

```bash
ainb usage plan show --format json
ainb usage plan set claude-pro --provider claude --reset-day 12
ainb usage plan set custom --monthly-usd 75 --provider all
ainb usage plan reset
ainb usage currency GBP --symbol GBP
ainb usage currency --reset
ainb usage model-alias --list
ainb usage model-alias cursor-auto claude-sonnet-4-5
ainb usage model-alias --remove cursor-auto
```

TUI: press `i` to open Stats, `Tab` to reach `Burndown` and `Optimize`, `1`-`5` for periods, `p` for All/Claude/Codex, `/` include filter, `x` exclude filter, `d` custom range, `c` clear filters, and `r` reload.

---

### `ainb status`

Inspect a single session.

```bash
ainb status <SESSION>
```

`<SESSION>` can be a full/partial session UUID, tmux name, or workspace-name prefix. Prints session state, tmux status, worktree path, branch, and provider.

---

### `ainb logs`

View or follow a session's log output.

```bash
ainb logs [OPTIONS] <SESSION>
```

| Flag | Description |
|------|-------------|
| `-l, --lines <N>` | Number of lines to show. Default `100`. |
| `-f, --follow` | Follow new output (like `tail -f`). |

**Example**

```bash
ainb logs my-project -f              # Follow live
ainb logs my-project --lines 500     # Print the last 500 lines and exit
```

---

### `ainb attach`

Attach to a session's tmux pane interactively.

```bash
ainb attach <SESSION>
```

This replaces the current process with `tmux attach`, so it only makes sense from an interactive terminal (not a script).

---

### `ainb kill`

Terminate a session: kills the tmux, removes the worktree symlink, updates the registry.

```bash
ainb kill [OPTIONS] <SESSION>
```

| Flag | Description |
|------|-------------|
| `-f, --force` | Skip the confirmation prompt. Required for non-interactive use. |

---

### `ainb auth`

Interactive authentication setup — select provider (Claude, API key, etc.) and store credentials. Equivalent of the TUI's "Setup" panel.

```bash
ainb auth
```

Credentials for supported providers are stored via the OS keyring (macOS Keychain / libsecret / Windows Credential Manager) rather than a plaintext config file.

---

### `ainb recover`

Recover sessions whose state drifted — e.g. `sessions.json` entry without a live tmux, or agent-side metadata in `~/.claude/agents/` without a registry entry.

```bash
ainb recover <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `list` | List orphaned sessions and broken worktree symlinks. Read-only. |
| `resume <SESSION>` | Re-register an orphaned session in `sessions.json`. Accepts a partial UUID, tmux name, or workspace prefix. |
| `cleanup [SESSION] [--force]` | Remove orphaned session entries and broken symlinks. Omit `<SESSION>` to clean all orphans. `--force` skips confirmation. |

**Examples**

```bash
ainb recover list                       # Read-only sanity check
ainb recover list --format json         # Feed into automation
ainb recover cleanup --force            # Clean every orphan without prompting
ainb recover resume 7a2b                # Resume by UUID prefix
```

---

### `ainb config`

Read and modify configuration.

```bash
ainb config <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `show` | Display the merged config (project + user + system). |
| `get <KEY>` | Get a value by dot-notation key (e.g. `authentication.default_model`). |
| `set <KEY> <VALUE>` | Set a value in the **user-level** config. Creates the file if missing. |
| `reset [--force]` | Reset user config to defaults. Prompts unless `--force`. |
| `path` | Show the resolved path of each config layer and whether it exists. |
| `edit` | Open the user config in `$EDITOR`. |

**Examples**

```bash
ainb config path                                     # Where's my config?
ainb config show --format json | jq .                # Dump merged config
ainb config get authentication.default_model        # → sonnet
ainb config set authentication.default_model opus   # Persist a change
ainb config reset --force                            # Nuke user overrides
```

The dot-notation supports any TOML path the schema exposes; invalid keys exit with `1` and a list of valid top-level sections.

---

### `ainb git`

Worktree inspection and cleanup.

```bash
ainb git <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `worktrees` | List all managed worktrees and which session owns each. |
| `cleanup [--dry-run] [--force]` | Remove worktrees not referenced by any session. `--dry-run` previews. |
| `status <SESSION>` | Show `git status` for a session's worktree. |

**Example**

```bash
ainb git worktrees --format json | jq '.[] | select(.session_id == null)'  # Orphaned worktrees
ainb git cleanup --dry-run                                                 # Preview cleanup
```

---

### `ainb favorites`

Bookmark frequently-used repositories so `ainb run` can pick them up by alias.

```bash
ainb favorites <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `list` | All favorites sorted by use count (most-used first). |
| `add --alias <A> [--description <D>] [--tags <A,B>] <SOURCE>` | Add a favorite. `<SOURCE>` is `owner/repo`, an HTTPS/SSH URL, or a local path. |
| `remove <ALIAS>` | Remove a favorite by alias. |
| `use <ALIAS>` | Bump `use_count` and `last_used` for ranking. |

**Example**

```bash
ainb favorites add --alias myapp --tags "work,rust" git@github.com:me/myapp.git
ainb favorites list --format json | jq '.[0].alias'
```

Favorites live in `~/.agents-in-a-box/favorites.json`.

---

### `ainb init`

First-time setup, prerequisite check, and factory reset.

```bash
ainb init [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--check` | Only verify prerequisites (`tmux`, `git`, `claude`, `docker`). Read-only. |
| `--status` | Report onboarding completion state. |
| `--reset` | **Destructive** — remove `~/.agents-in-a-box/` entirely. Requires `--force` for non-interactive use. |
| `-f, --force` | Skip interactive confirmation (required for non-interactive `--reset`). |

**Example**

```bash
ainb init --check                   # CI-friendly prereq check
ainb init --status --format json    # What's configured?
ainb init --reset --force           # Factory reset (dangerous)
```

---

### `ainb presets`

Session presets bundle a provider + model + defaults that `ainb run` can apply in one step.

```bash
ainb presets <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `list` | Built-in + custom presets. |
| `show <NAME>` | Full details for one preset. |
| `create <NAME> [--provider <P>] [--model <M>] [--description <D>]` | Create a custom preset. Name must not collide with a built-in. |
| `delete <NAME>` | Delete a custom preset. Built-ins cannot be deleted. |
| `apply <NAME>` | Write `./.agents-box/preset.toml` pointing to this preset. Subsequent `ainb run` invocations in this repo pick up the defaults. |

**Built-in presets** (ship with ainb):

| Name | Provider / Model | Use case |
|------|-----------------|----------|
| `rust-backend` | claude / sonnet | Rust backend development with testing + clippy |
| `typescript-frontend` | claude / sonnet | TypeScript frontend with React + testing |
| `fast-iteration` | claude / haiku | Maximum speed — skips permission prompts |

**Example**

```bash
ainb presets list
ainb presets show rust-backend
ainb presets create my-opus --provider claude --model opus --description "Heavy refactors"
ainb presets apply rust-backend     # Next `ainb run` in this repo uses sonnet + rust conventions
```

---

### `ainb completion`

Emit shell completion script to stdout.

```bash
ainb completion <SHELL>
```

Supported: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

---

### `ainb claudecode`

Provider-namespaced commands for Claude Code. Other providers grow their own namespace; today only the statusline hook lives here.

```bash
ainb claudecode <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `statusline` | Claude Code statusline hook: reads session JSON on stdin, caches rate-limit windows for the TUI, and emits a powerline status string on stdout. |

**`statusline` flags**

| Flag | Description |
|------|-------------|
| `--cache-only` | Side-channel mode: write the rate-limit cache only and emit nothing on stdout. |

Wire it into Claude Code's `settings.json` as the `statusLine` command so the TUI can surface live rate-limit windows.

---

### `ainb plugin`

Manage ainb plugins — install from marketplaces, update, remove, and inspect runtime events.

```bash
ainb plugin <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `install <PLUGIN> [-y]` | Install a plugin from a marketplace. `<PLUGIN>` is a plugin id, e.g. `burndown` or `ainb-plugins/burndown@0.1.0`. `-y, --yes` skips the capability-approval prompt. |
| `update <PLUGIN> [-y]` | Update an installed plugin to the latest matching version. `-y, --yes` skips prompts when new capabilities are requested. |
| `remove <PLUGIN> [-y]` | Remove an installed plugin. `-y, --yes` skips the data-directory deletion prompt. |
| `list` | List installed plugins. |
| `search <QUERY>` | Search registered marketplaces by plugin name. |
| `marketplace <SUBCOMMAND>` | Manage marketplace registries: `add <URL\|PATH>`, `remove <NAME>`, `list`. |
| `lint <PLUGIN>` | Validate a plugin manifest + binary (ABI 2.0 sanity checks). Accepts a plugin id, staging dir, or `manifest.toml` path. |
| `watch <PLUGIN> [--duration <SECS>]` | Live-tail lifecycle + snapshot events for a registered plugin. `--duration` defaults to 30s. |
| `tail <PLUGIN> [--level <LVL>] [--since <TS>] [--duration <SECS>]` | Stream the host's tracing layer filtered to one plugin id. `--level` is `trace\|debug\|info\|warn\|error` (default `debug`); `--since` is an RFC-3339 timestamp; `--duration` defaults to 30s. |

**Examples**

```bash
ainb plugin marketplace add https://github.com/stevengonsalvez/ainb-plugins
ainb plugin search burndown
ainb plugin install burndown -y
ainb plugin list --format json
ainb plugin watch burndown --duration 60
```

In-tree v2 reference plugins: `burndown` (analytics), `notifyd` (notifications), `session-reader` (data backend).

---

## Scripting recipes

**Agent-friendly: list running sessions as JSON, exit non-zero if none**

```bash
count=$(ainb list --running --format json | jq length)
[ "$count" -gt 0 ] || { echo "no running sessions"; exit 1; }
```

**Create a session, wait for it to register, seed a prompt, kill when done**

```bash
ainb run --repo . --worktree --name review-pr --tool claude --model sonnet \
         -p "Review the diff on the current branch"
until ainb status review-pr --format json | jq -e '.is_running' >/dev/null; do sleep 1; done
# ... do work ...
ainb kill review-pr --force
```

**Isolated sandbox for testing (zero impact on real user data)**

```bash
export HOME=$(mktemp -d)
export TMUX_TMPDIR=$HOME/tmux
mkdir -p "$TMUX_TMPDIR"
ainb init --check                 # Verifies prereqs against the sandbox
ainb config set authentication.default_model opus
ainb list                         # Empty — sandbox is pristine
```

See `scripts/cli-smoke-test.sh` in this repo for a full isolation harness.

**Cleanup everything orphaned**

```bash
ainb recover cleanup --force
ainb git cleanup --force
```

---

*For TUI-specific keybindings see the README. For architecture and module layout see `CLAUDE.md`.*
