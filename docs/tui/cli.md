---
title: "ainb CLI Reference"
description: "Full multi-hierarchy reference for every ainb subcommand — generated from the binary's --help."
---

`ainb` is both an interactive terminal UI and a scriptable, headless CLI. Every
operation the TUI performs is also exposed as a subcommand with `--format json`
output, so humans drive it from the dashboard and agents drive it from shell
scripts. Run `ainb` with no arguments to launch the TUI; use any subcommand
below for non-interactive work.

> **Generated — do not edit by hand.** This page is produced from the live
> binary by [`ainb-tui/scripts/gen-cli-reference.sh`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/ainb-tui/scripts/gen-cli-reference.sh),
> which walks `ainb <cmd> --help` for every command. CI fails if it drifts, so
> the output of `ainb --help` stays the source of truth. To update: run the
> script and commit the result.

## Global flags

| Flag | Description |
|------|-------------|
| `--format <text\|json\|csv\|markdown>` | Output format for any command (default `text`). `json` is the machine-readable form for scripting/agents. |
| `-h, --help` | Print help for the command (recursive — works at every level). |
| `-V, --version` | Print the build identity (commit + date), or `-V` for the bare semver. |

## Reading this reference

Each command shows its description followed by the verbatim `ainb <cmd> --help`
output — including its arguments, flags, and an `EXAMPLES:` block. Groups
(`config`, `git`, `usage`, `fleet`, `hangar`, …) nest their subcommands as
sub-sections; recursive help (`ainb <group> <sub> --help`) works for every
node. The page's right-hand "On this page" panel is the full command tree.

## Command reference

## `ainb tui`

Launch the TUI (default if no command given)

```console
$ ainb tui --help
Launch the TUI (default if no command given)

Usage: ainb tui [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb diff-review`

Review a repository's uncommitted changes in the Code Review surface

```console
$ ainb diff-review --help
Review a repository's uncommitted changes in the Code Review surface

Usage: ainb diff-review [OPTIONS] [path]

Arguments:
  [path]  Repository path (default: current directory) [default: .]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb diff-review                 Review uncommitted changes in the current repo
  ainb diff-review ~/code/proj     Review a specific repo
  ainb diff-review --format json   Emit the structured diff as JSON (headless)
```

## `ainb run`

Spawn a new AI coding session

```console
$ ainb run --help
Spawn a new AI coding session

Usage: ainb run [OPTIONS]

Options:
      --format <format>                Output format [default: text] [possible values: text, json, csv, markdown]
      --remote-repo <REMOTE_REPO>      Remote repository (e.g., username/repo or full URL)
      --repo <REPO>                    Local repository path
      --create-branch <CREATE_BRANCH>  Create a new branch with this name
      --worktree                       Use git worktree for isolation
      --tool <TOOL>                    AI tool to use [default: claude] [possible values: claude, codex, gemini, copilot]
      --model <MODEL>                  Provider model ID to pass through unchanged
  -p, --prompt <PROMPT>                Initial prompt to send
  -a, --attach                         Attach to session after creation
      --dangerously-skip-permissions   Skip permission prompts (dangerous!)
      --name <NAME>                    Custom session name
  -i, --interactive                    Run in interactive mode (spawn tmux and attach)
      --parent <PARENT>                Parent session id — links this session to an orchestrator (e.g. ATC) so its completions route to the parent's durable inbox (event-driven plumbing). Exported into the session as `AINB_PARENT_SESSION`
  -h, --help                           Print help

EXAMPLES:
  ainb run --repo .                                 Use current directory
  ainb run --repo . --worktree                      Isolate in a new worktree
  ainb run --repo . --create-branch feat/new        Create a branch + worktree
  ainb run --remote-repo owner/repo                 Clone GitHub repo first
  ainb run --tool codex --repo .                    Use Codex instead of Claude
  ainb run --repo . -p "fix the failing tests"    Send an initial prompt
  ainb run --repo . --attach                        Drop into tmux after creating
```

## `ainb list`

List all sessions (running + idle)

```console
$ ainb list --help
List all sessions (running + idle)

Usage: ainb list [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --running                Show only running sessions
      --workspace <WORKSPACE>  Show only sessions for a specific workspace
  -h, --help                   Print help

EXAMPLES:
  ainb list                        List all sessions
  ainb list --running              Only running sessions
  ainb list --workspace my-proj    Sessions for one workspace
  ainb list --format json          Machine-readable output
```

## `ainb logs`

View session output/logs

```console
$ ainb logs --help
View session output/logs

Usage: ainb logs [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID or name

Options:
  -f, --follow           Follow log output (like tail -f)
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -l, --lines <LINES>    Number of lines to show [default: 100]
  -h, --help             Print help

EXAMPLES:
  ainb logs my-project             Last 100 lines for a session
  ainb logs my-project -f          Follow live (like tail -f)
  ainb logs my-project -l 500      Last 500 lines
```

## `ainb attach`

Attach to a session (drops into tmux)

```console
$ ainb attach --help
Attach to a session (drops into tmux)

Usage: ainb attach [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID or name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb attach my-project           Drop into the session's tmux
```

## `ainb status`

Show a session's status/health

```console
$ ainb status --help
Show a session's status/health

Usage: ainb status [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID or name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb status my-project           Show status/health
  ainb status my-project --format json
```

## `ainb kill`

Terminate a session

```console
$ ainb kill --help
Terminate a session

Usage: ainb kill [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID or name

Options:
  -f, --force            Force kill without confirmation
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb kill my-project             Kill (with confirmation)
  ainb kill my-project --force     Kill without prompting
```

## `ainb auth`

Set up authentication

```console
$ ainb auth --help
Set up authentication

Usage: ainb auth [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb auth                        Interactive authentication setup
```

## `ainb recover`

Recover orphaned or crashed sessions

```console
$ ainb recover --help
Recover orphaned or crashed sessions

Usage: ainb recover [OPTIONS] <COMMAND>

Commands:
  list     List orphaned sessions and broken worktree symlinks
  resume   Resume an orphaned session by re-registering it in the session store
  cleanup  Clean up orphaned sessions and broken worktrees
  help     Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb recover list                Find orphaned sessions + broken worktrees
  ainb recover resume <id>         Re-register an orphaned session
  ainb recover cleanup             Remove orphans + broken worktrees
```

### `ainb recover list`

List orphaned sessions and broken worktree symlinks

```console
$ ainb recover list --help
List orphaned sessions and broken worktree symlinks

Usage: ainb recover list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb recover resume`

Resume an orphaned session by re-registering it in the session store

```console
$ ainb recover resume --help
Resume an orphaned session by re-registering it in the session store

Usage: ainb recover resume [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID, tmux name, or workspace name prefix

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb recover cleanup`

Clean up orphaned sessions and broken worktrees

```console
$ ainb recover cleanup --help
Clean up orphaned sessions and broken worktrees

Usage: ainb recover cleanup [OPTIONS] [SESSION]

Arguments:
  [SESSION]  Specific session to clean up (all if omitted)

Options:
  -f, --force            Skip confirmation prompt
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb config`

Manage configuration

```console
$ ainb config --help
Manage configuration

Usage: ainb config [OPTIONS] <COMMAND>

Commands:
  show   Display current configuration (merged from all sources)
  get    Get a specific config value using dot-notation (e.g., `authentication.default_model`)
  set    Set a config value in user-level config
  reset  Reset user configuration to defaults
  path   Show config file locations
  edit   Open user config in $EDITOR
  help   Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb config show                                Merged config from all sources
  ainb config get authentication.default_model    Read one value (dot notation)
  ainb config set ui_preferences.show_git_status true
  ainb config path                                Where config files live
  ainb config edit                                Open user config in $EDITOR
  ainb config reset                               Restore defaults
```

### `ainb config show`

Display current configuration (merged from all sources)

```console
$ ainb config show --help
Display current configuration (merged from all sources)

Usage: ainb config show [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb config get`

Get a specific config value using dot-notation (e.g., `authentication.default_model`)

```console
$ ainb config get --help
Get a specific config value using dot-notation (e.g., `authentication.default_model`)

Usage: ainb config get [OPTIONS] <KEY>

Arguments:
  <KEY>  Config key in dot-notation

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb config set`

Set a config value in user-level config

```console
$ ainb config set --help
Set a config value in user-level config

Usage: ainb config set [OPTIONS] <KEY> <VALUE>

Arguments:
  <KEY>    Config key in dot-notation
  <VALUE>  Value to set

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb config reset`

Reset user configuration to defaults

```console
$ ainb config reset --help
Reset user configuration to defaults

Usage: ainb config reset [OPTIONS]

Options:
  -f, --force            Skip confirmation prompt
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb config path`

Show config file locations

```console
$ ainb config path --help
Show config file locations

Usage: ainb config path [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb config edit`

Open user config in $EDITOR

```console
$ ainb config edit --help
Open user config in $EDITOR

Usage: ainb config edit [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb git`

Manage git worktrees + inspect session changes

```console
$ ainb git --help
Manage git worktrees + inspect session changes

Usage: ainb git [OPTIONS] <COMMAND>

Commands:
  worktrees  List all managed worktrees and their session association
  cleanup    Remove orphaned worktrees (not referenced by any session)
  status     Show git status for a specific session's worktree
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb git worktrees               List managed worktrees + session links
  ainb git status my-project       Git status for a session's worktree
  ainb git cleanup                 Remove orphaned worktrees
```

### `ainb git worktrees`

List all managed worktrees and their session association

```console
$ ainb git worktrees --help
List all managed worktrees and their session association

Usage: ainb git worktrees [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb git cleanup`

Remove orphaned worktrees (not referenced by any session)

```console
$ ainb git cleanup --help
Remove orphaned worktrees (not referenced by any session)

Usage: ainb git cleanup [OPTIONS]

Options:
  -f, --force            Skip confirmation prompt
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --dry-run          Preview what would be removed without making changes
  -h, --help             Print help
```

### `ainb git status`

Show git status for a specific session's worktree

```console
$ ainb git status --help
Show git status for a specific session's worktree

Usage: ainb git status [OPTIONS] <SESSION>

Arguments:
  <SESSION>  Session ID (full/partial UUID) or workspace name prefix

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb favorites`

Manage favorite repositories

```console
$ ainb favorites --help
Manage favorite repositories

Usage: ainb favorites [OPTIONS] <COMMAND>

Commands:
  list    List all favorites sorted by usage (most used first)
  add     Add a new favorite repository
  remove  Remove a favorite by alias
  use     Record usage of a favorite (bumps use_count and last_used)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb favorites list                       Favorites ranked by usage
  ainb favorites add --alias <alias> <src>  Add a favorite (alias + path/URL)
  ainb favorites use <alias>                Record a use (bumps ranking)
  ainb favorites remove <alias>             Delete a favorite
```

### `ainb favorites list`

List all favorites sorted by usage (most used first)

```console
$ ainb favorites list --help
List all favorites sorted by usage (most used first)

Usage: ainb favorites list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb favorites add`

Add a new favorite repository

```console
$ ainb favorites add --help
Add a new favorite repository

Usage: ainb favorites add [OPTIONS] --alias <ALIAS> <SOURCE>

Arguments:
  <SOURCE>  Repository source: owner/repo, https URL, ssh URL, or local path

Options:
      --alias <ALIAS>              Friendly alias for this favorite (used to look it up later)
      --format <format>            Output format [default: text] [possible values: text, json, csv, markdown]
      --description <DESCRIPTION>  Optional human-readable description
      --tags <TAGS>                Comma-separated list of tags
  -h, --help                       Print help
```

### `ainb favorites remove`

Remove a favorite by alias

```console
$ ainb favorites remove --help
Remove a favorite by alias

Usage: ainb favorites remove [OPTIONS] <ALIAS>

Arguments:
  <ALIAS>  Alias of the favorite to remove

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb favorites use`

Record usage of a favorite (bumps use_count and last_used)

```console
$ ainb favorites use --help
Record usage of a favorite (bumps use_count and last_used)

Usage: ainb favorites use [OPTIONS] <ALIAS>

Arguments:
  <ALIAS>  Alias of the favorite to record usage for

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb init`

First-time setup and prerequisite checking

```console
$ ainb init --help
First-time setup and prerequisite checking

Usage: ainb init [OPTIONS]

Options:
      --check            Only check prerequisites, don't modify any files
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --status           Show current onboarding completion status
      --reset            Factory reset: remove ~/.agents-in-a-box entirely
  -f, --force            Skip interactive confirmation (required for non-interactive --reset)
  -y, --yes              Auto-install missing dependencies ainb can install safely (npm/uv/cargo/ ainb/claude-plugin). brew/curl still need explicit per-item consent
      --script           Generate an idempotent install script for what's missing (writes ~/.agents-in-a-box/installer/install-<agent>.sh) instead of installing
      --agent <AGENT>    Target agent for --script: claude | codex | copilot (default: claude)
  -h, --help             Print help

EXAMPLES:
  ainb init                        First-time setup (interactive)
  ainb init --check                Only check prerequisites, change nothing
  ainb init --status               Show onboarding completion status
  ainb init --reset --force        Factory reset ~/.agents-in-a-box (non-interactive)
```

## `ainb doctor`

Health-check the manifest, lockfile, deployed files, and configured sources. Exits non-zero when any problem is found

```console
$ ainb doctor --help
Health-check the manifest, lockfile, deployed files, and configured sources. Exits non-zero when any problem is found

Usage: ainb doctor [OPTIONS]

Options:
      --offline  Skip the source-reachability check (avoid hitting the network / re-running fetchers)
  -h, --help     Print help

EXAMPLES:
  ainb doctor                      Health-check skill manifest/lockfile/deployed files
  ainb doctor --offline            Skip source-reachability network checks
```

## `ainb reflect`

Reflect plugin lifecycle: bootstrap installer + dependency check

```console
$ ainb reflect --help
Reflect plugin lifecycle: bootstrap installer + dependency check

Usage: ainb reflect [OPTIONS] <COMMAND>

Commands:
  bootstrap  One-step install: auto-install reflect-kb[graph]; print missing system tools
  check      Classified dependency check (reflect-focused; same engine as `ainb doctor`)
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb reflect bootstrap           One-step install of reflect-kb[graph]
  ainb reflect bootstrap --yes     Non-interactive install
  ainb reflect check               Classified dependency report
```

### `ainb reflect bootstrap`

One-step install: auto-install reflect-kb[graph]; print missing system tools

```console
$ ainb reflect bootstrap --help
One-step install: auto-install reflect-kb[graph]; print missing system tools

Usage: ainb reflect bootstrap [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -y, --yes              Install the reflect-owned layer without prompting
      --print-only       Detect + print every command; install nothing
  -h, --help             Print help
```

### `ainb reflect check`

Classified dependency check (reflect-focused; same engine as `ainb doctor`)

```console
$ ainb reflect check --help
Classified dependency check (reflect-focused; same engine as `ainb doctor`)

Usage: ainb reflect check [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb presets`

Manage session presets

```console
$ ainb presets --help
Manage session presets

Usage: ainb presets [OPTIONS] <COMMAND>

Commands:
  list    List all available presets (built-in + custom)
  show    Show full details for a specific preset
  create  Create a new custom preset
  delete  Delete a custom preset
  apply   Apply a preset to the current repository (writes .agents-box/preset.toml)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb presets list                Built-in + custom presets
  ainb presets show <name>         Full preset details
  ainb presets apply <name>        Write .agents-box/preset.toml in this repo
```

### `ainb presets list`

List all available presets (built-in + custom)

```console
$ ainb presets list --help
List all available presets (built-in + custom)

Usage: ainb presets list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb presets show`

Show full details for a specific preset

```console
$ ainb presets show --help
Show full details for a specific preset

Usage: ainb presets show [OPTIONS] <NAME>

Arguments:
  <NAME>  Preset name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb presets create`

Create a new custom preset

```console
$ ainb presets create --help
Create a new custom preset

Usage: ainb presets create [OPTIONS] <NAME>

Arguments:
  <NAME>  Preset name (must be unique and not collide with built-ins)

Options:
      --format <format>            Output format [default: text] [possible values: text, json, csv, markdown]
      --provider <PROVIDER>        Agent provider (e.g., claude, codex, gemini)
      --model <MODEL>              Model identifier (e.g., sonnet, opus, haiku)
      --description <DESCRIPTION>  Human-readable description
  -h, --help                       Print help
```

### `ainb presets delete`

Delete a custom preset

```console
$ ainb presets delete --help
Delete a custom preset

Usage: ainb presets delete [OPTIONS] <NAME>

Arguments:
  <NAME>  Preset name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb presets apply`

Apply a preset to the current repository (writes .agents-box/preset.toml)

```console
$ ainb presets apply --help
Apply a preset to the current repository (writes .agents-box/preset.toml)

Usage: ainb presets apply [OPTIONS] <NAME>

Arguments:
  <NAME>  Preset name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb usage`

Usage analytics, reports, export, and optimization

```console
$ ainb usage --help
Usage analytics, reports, export, and optimization

Usage: ainb usage [OPTIONS] <COMMAND>

Commands:
  report       Print a compact burndown report
  status       Print current usage status
  today        Print today's usage
  month        Print current month's usage
  export       Export usage data as CSV or JSON
  plan         Manage usage plan
  currency     Set or reset display currency
  model-alias  Manage model aliases
  optimize     Show read-only optimization findings
  savings      Token-savings rollup (Headroom proxy + RTK + caveman estimate)
  compare      Compare models
  yield        Estimate usage yield from session signals
  cache        Inspect or wipe the persistent usage cache
  models       Per-model rollup or per-model × per-activity-category matrix
  help         Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb usage today                 Today's usage  (needs the burndown plugin)
  ainb usage month                 Current month
  ainb usage report                Compact burndown report
  ainb usage export --format csv   Export usage data
  ainb usage models                Per-model rollup
```

### `ainb usage report`

Print a compact burndown report

```console
$ ainb usage report --help
Print a compact burndown report

Usage: ainb usage report [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage status`

Print current usage status

```console
$ ainb usage status --help
Print current usage status

Usage: ainb usage status [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage today`

Print today's usage

```console
$ ainb usage today --help
Print today's usage

Usage: ainb usage today [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage month`

Print current month's usage

```console
$ ainb usage month --help
Print current month's usage

Usage: ainb usage month [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage export`

Export usage data as CSV or JSON

```console
$ ainb usage export --help
Export usage data as CSV or JSON

Usage: ainb usage export [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -o, --output <OUTPUT>      Output file or directory
  -h, --help                 Print help
```

### `ainb usage plan`

Manage usage plan

```console
$ ainb usage plan --help
Manage usage plan

Usage: ainb usage plan [OPTIONS] <COMMAND>

Commands:
  show    Show configured plan
  set     Set a known or custom plan
  reset   Remove plan
  detect  Attempt to detect plan from Claude CLI
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb usage plan show`

Show configured plan

```console
$ ainb usage plan show --help
Show configured plan

Usage: ainb usage plan show [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

#### `ainb usage plan set`

Set a known or custom plan

```console
$ ainb usage plan set --help
Set a known or custom plan

Usage: ainb usage plan set [OPTIONS] <PLAN>

Arguments:
  <PLAN>  [possible values: claude-pro, claude-max, claude-max5x, cursor-pro, custom, none]

Options:
      --format <format>            Output format [default: text] [possible values: text, json, csv, markdown]
      --monthly-usd <MONTHLY_USD>  
      --provider <PROVIDER>        [default: all] [possible values: all, claude, codex, cursor]
      --reset-day <RESET_DAY>      [default: 1]
  -h, --help                       Print help
```

#### `ainb usage plan reset`

Remove plan

```console
$ ainb usage plan reset --help
Remove plan

Usage: ainb usage plan reset [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb usage plan detect`

Attempt to detect plan from Claude CLI

```console
$ ainb usage plan detect --help
Attempt to detect plan from Claude CLI

Usage: ainb usage plan detect [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb usage currency`

Set or reset display currency

```console
$ ainb usage currency --help
Set or reset display currency

Usage: ainb usage currency [OPTIONS] [CODE]

Arguments:
  [CODE]  Currency code, for example USD, GBP, EUR

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --symbol <SYMBOL>  Display symbol
      --reset            Reset to USD
  -h, --help             Print help
```

### `ainb usage model-alias`

Manage model aliases

```console
$ ainb usage model-alias --help
Manage model aliases

Usage: ainb usage model-alias [OPTIONS] [FROM] [TO]

Arguments:
  [FROM]  Source model name
  [TO]    Alias target model name

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --list             List aliases
      --remove <REMOVE>  Remove alias by source model name
  -h, --help             Print help
```

### `ainb usage optimize`

Show read-only optimization findings

```console
$ ainb usage optimize --help
Show read-only optimization findings

Usage: ainb usage optimize [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage savings`

Token-savings rollup (Headroom proxy + RTK + caveman estimate)

```console
$ ainb usage savings --help
Token-savings rollup (Headroom proxy + RTK + caveman estimate)

Usage: ainb usage savings [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage compare`

Compare models

```console
$ ainb usage compare --help
Compare models

Usage: ainb usage compare [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage yield`

Estimate usage yield from session signals

```console
$ ainb usage yield --help
Estimate usage yield from session signals

Usage: ainb usage yield [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
  -h, --help                 Print help
```

### `ainb usage cache`

Inspect or wipe the persistent usage cache

```console
$ ainb usage cache --help
Inspect or wipe the persistent usage cache

Usage: ainb usage cache [OPTIONS] <COMMAND>

Commands:
  info   Show cache DB path, on-disk size, file count, oldest entry timestamp
  clear  Drop all cached file rows (schema_version row preserved)
  help   Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb usage cache info`

Show cache DB path, on-disk size, file count, oldest entry timestamp

```console
$ ainb usage cache info --help
Show cache DB path, on-disk size, file count, oldest entry timestamp

Usage: ainb usage cache info [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb usage cache clear`

Drop all cached file rows (schema_version row preserved)

```console
$ ainb usage cache clear --help
Drop all cached file rows (schema_version row preserved)

Usage: ainb usage cache clear [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb usage models`

Per-model rollup or per-model × per-activity-category matrix

```console
$ ainb usage models --help
Per-model rollup or per-model × per-activity-category matrix

Usage: ainb usage models [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --period <PERIOD>      Period: today, week, 30days, month, all [default: week] [possible values: today, week, 30days, month, all]
      --from <FROM>          Start date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --to for an explicit range)
      --to <TO>              End date YYYY-MM-DD (mutually exclusive with --month, --quarter, --last-n-days, --ytd; pairs with --from for an explicit range)
      --month <MONTH>        Pin to a specific calendar month, e.g. `2026-04`. Mutually exclusive with --quarter, --last-n-days, --ytd, --from, --to
      --quarter <QUARTER>    Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually exclusive with --month, --last-n-days, --ytd, --from, --to
      --last-n-days <N>      Last N days (rolling window ending today). Mutually exclusive with --month, --quarter, --ytd, --from, --to
      --ytd                  Jan 1 of the current year through today. Mutually exclusive with --month, --quarter, --last-n-days, --from, --to
      --provider <PROVIDER>  Provider: all, claude, codex [default: all] [possible values: all, claude, codex, cursor, copilot, gemini]
      --include <INCLUDE>    Include projects matching substring (repeatable; OR-combined). Note: previously aliased as `--project`; the alias has been removed because `--project` is now a distinct exact-match cross-filter flag (see below). Use `--include <substring>` for the substring/glob behaviour
      --exclude <EXCLUDE>    Exclude projects matching substring (repeatable; OR-combined)
      --no-cache             Bypass the persistent usage cache and force a full re-parse
      --hard                 Hard refresh: wipe the parse cache and stable rollup, then rebuild everything from source before reporting. CPU-heavy on large histories; the flag itself is the explicit opt-in (no interactive prompt, safe for pipes/scripts)
      --project <PROJECT>    Drill into a single project (exact match). Repeatable
      --model <MODEL>        Drill into a single model (exact match). Repeatable
      --activity <ACTIVITY>  Drill into one activity category (Coding, Conversation, Git, etc. — see ActivityCategory::label). Repeatable
      --session <SESSION>    Drill into a single session id. Repeatable
      --branch <BRANCH>      Drill into a single git branch (exact match against `gitBranch` on Claude turns). Repeatable. Codex turns have no recorded branch and are excluded by any non-empty `--branch` filter
      --top <TOP>            Cap the long By-Project / By-Activity / By-Model tables at N rows (default 8 mirrors the historical hard-coded slice). Applies to report, today, month, and export subcommands across every format. 0 means "no cap" — emit every row [default: 8]
      --by-task              Emit a per-model × per-activity-category matrix instead of the flat per-model rollup. Rows = model, columns = activity category, cell = (calls, tokens, cost)
  -h, --help                 Print help
```

## `ainb claudecode`

Claude Code-specific commands (statusline, etc.). Provider-namespaced — other providers grow their own.

```console
$ ainb claudecode --help
Claude Code-specific commands (statusline, etc.). Provider-namespaced — other providers grow their own.

Usage: ainb claudecode [OPTIONS] <COMMAND>

Commands:
  statusline  Claude Code statusline hook: read JSON on stdin, cache rate-limit windows for the TUI, and emit a powerline status string on stdout.
  help        Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb claudecode statusline               Statusline hook (reads Claude JSON on stdin)
  ainb claudecode statusline --cache-only  Cache rate-limit windows, emit nothing
  # wire into ~/.claude/settings.json statusLine.command
```

### `ainb claudecode statusline`

Claude Code statusline hook: read JSON on stdin, cache rate-limit windows for the TUI, and emit a powerline status string on stdout.

```console
$ ainb claudecode statusline --help
Claude Code statusline hook: read JSON on stdin, cache rate-limit windows for the TUI, and emit a powerline status string on stdout.

Usage: ainb claudecode statusline [OPTIONS]

Options:
      --cache-only       Side-channel mode: write the cache only and emit nothing on stdout.
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --install          Wire the statusline into ~/.claude/settings.json (idempotent) instead of running the hook.
  -h, --help             Print help
```

## `ainb codex`

Codex-specific commands (statusline, etc.). Provider-namespaced — the Codex analog of `claudecode`.

```console
$ ainb codex --help
Codex-specific commands (statusline, etc.). Provider-namespaced — the Codex analog of `claudecode`.

Usage: ainb codex [OPTIONS] <COMMAND>

Commands:
  statusline  Pull Codex OAuth quota (5h + weekly) from chatgpt.com and cache it for the ainb TUI top bar. Throttled; hide-on-fail when Codex is not logged in.
  help        Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb codex statusline          Pull + cache Codex OAuth quota for the TUI top bar
  ainb codex statusline --force  Bypass the throttle and pull now
```

### `ainb codex statusline`

Pull Codex OAuth quota (5h + weekly) from chatgpt.com and cache it for the ainb TUI top bar. Throttled; hide-on-fail when Codex is not logged in.

```console
$ ainb codex statusline --help
Pull Codex OAuth quota (5h + weekly) from chatgpt.com and cache it for the ainb TUI top bar. Throttled; hide-on-fail when Codex is not logged in.

Usage: ainb codex statusline [OPTIONS]

Options:
      --force            Bypass the throttle and pull now.
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb tmux`

Manage the rich tmux.conf shipped with ainb-tui (Catppuccin Mocha + TPM + resurrect/continuum/yank + discoverable detach hints).

```console
$ ainb tmux --help
Manage the rich tmux.conf shipped with ainb-tui (Catppuccin Mocha + TPM + resurrect/continuum/yank + discoverable detach hints).

Usage: ainb tmux [OPTIONS] <COMMAND>

Commands:
  install  Install or upgrade the bundled rich tmux.conf to ~/.tmux.conf (backs up any existing file, shows a diff preview, then reloads live sessions).
  status   Report whether ~/.tmux.conf is missing, up to date, or stale relative to the bundled rich conf.
  help     Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb tmux status                 Is ~/.tmux.conf current vs the bundled conf?
  ainb tmux install                Install/upgrade bundled tmux.conf (backs up existing)
```

### `ainb tmux install`

Install or upgrade the bundled rich tmux.conf to ~/.tmux.conf (backs up any existing file, shows a diff preview, then reloads live sessions).

```console
$ ainb tmux install --help
Install or upgrade the bundled rich tmux.conf to ~/.tmux.conf (backs up any existing file, shows a diff preview, then reloads live sessions).

Usage: ainb tmux install [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -y, --yes              Skip the confirmation prompt (non-interactive use)
      --no-plugins       Skip TPM clone + plugin install. Useful in restricted environments or for users who don't want plugins
      --no-reload        Skip `tmux source-file` reload of live sessions
  -h, --help             Print help
```

### `ainb tmux status`

Report whether ~/.tmux.conf is missing, up to date, or stale relative to the bundled rich conf.

```console
$ ainb tmux status --help
Report whether ~/.tmux.conf is missing, up to date, or stale relative to the bundled rich conf.

Usage: ainb tmux status [OPTIONS]

Options:
      --diff             Show a diff preview if the on-disk conf is stale
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb otel`

Set up OpenTelemetry export to Grafana Cloud (Grafana Alloy pipeline)

```console
$ ainb otel --help
Set up OpenTelemetry export to Grafana Cloud (Grafana Alloy pipeline)

Usage: ainb otel [OPTIONS] <COMMAND>

Commands:
  setup   Set up OpenTelemetry export to Grafana Cloud (assets, creds, Alloy)
  status  Show local OTEL pipeline state (env file, Alloy install, tmux session)
  start   (Re)start Grafana Alloy in its tmux session
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb otel setup     Configure OTEL export to Grafana Cloud (assets, creds, Alloy)
  ainb otel status    Show the local OTEL pipeline state
  ainb otel start     (Re)start Grafana Alloy in its tmux session
```

### `ainb otel setup`

Set up OpenTelemetry export to Grafana Cloud (assets, creds, Alloy)

```console
$ ainb otel setup --help
Set up OpenTelemetry export to Grafana Cloud (assets, creds, Alloy)

Usage: ainb otel setup [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --host-name <HOST_NAME>  host.name resource attribute (defaults to the short hostname)
      --no-start               Don't start Alloy after writing config
      --no-install             Don't offer to `brew install` Alloy if it's missing
      --provider <PROVIDER>    Telemetry provider (only grafana-cloud today) [default: grafana-cloud]
  -h, --help                   Print help
```

### `ainb otel status`

Show local OTEL pipeline state (env file, Alloy install, tmux session)

```console
$ ainb otel status --help
Show local OTEL pipeline state (env file, Alloy install, tmux session)

Usage: ainb otel status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb otel start`

(Re)start Grafana Alloy in its tmux session

```console
$ ainb otel start --help
(Re)start Grafana Alloy in its tmux session

Usage: ainb otel start [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb completion`

Generate shell completions (bash, zsh, fish, powershell, elvish)

```console
$ ainb completion --help
Generate shell completions (bash, zsh, fish, powershell, elvish)

Usage: ainb completion [OPTIONS] <shell>

Arguments:
  <shell>  Shell to generate completions for [possible values: bash, elvish, fish, powershell, zsh]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb completion zsh > ~/.zsh/completions/_ainb
  ainb completion bash > /usr/local/etc/bash_completion.d/ainb
  ainb completion fish > ~/.config/fish/completions/ainb.fish
```

## `ainb abtop`

Snapshot running AI agents (top-for-agents) via `abtop --once`

```console
$ ainb abtop --help
Snapshot running AI agents (top-for-agents) via `abtop --once`

Usage: ainb abtop [OPTIONS] [args]...

Arguments:
  [args]...  Extra flags forwarded verbatim to `abtop --once` (e.g. --theme <name>)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb abtop                       Snapshot running AI agents
  ainb abtop --theme dracula       Forward flags to `abtop --once`
```

## `ainb web`

Serve an SSE-live web dashboard (live terminal + web-push) for the fleet

```console
$ ainb web --help
Serve an SSE-live web dashboard (live terminal + web-push) for the fleet

Usage: ainb web [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --listen <ADDR>    Address to bind (default loopback; non-loopback needs --token) [default: 127.0.0.1:8420]
      --token <SECRET>   Bearer token required on every /api/* route (enables non-loopback bind)
      --insecure-bind    Allow a non-loopback bind with no token. DANGEROUS: an unauthenticated bind exposes a control surface — the live WS terminal is interactive shell access to every fleet session. Only honored with --read-only (terminal disabled); otherwise refused. Use --token instead to expose the write surface safely
      --read-only        Viewer-only: disable the live terminal write surface (the WS terminal is refused with 403)
  -h, --help             Print help

EXAMPLES:
  ainb web                                       Serve on 127.0.0.1:8420 (loopback)
  ainb web --listen 0.0.0.0:8420 --token s3cr3t  Expose to the LAN behind a bearer token
  ainb web --read-only                           Viewer-only (live terminal disabled)
  ainb web --insecure-bind --read-only           Non-loopback viewer with no token (DANGEROUS)
```

## `ainb witr`

Trace a running process's causality chain (via the witr plugin)

```console
$ ainb witr --help
Trace a running process's causality chain (via the witr plugin)

Usage: ainb witr [OPTIONS] [args]...

Arguments:
  [args]...  witr target + flags, forwarded verbatim: <name> | --pid <pid> | --port <p> | --file <path> | --container <id>  [--tree|--warnings|--short]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb witr node                   Trace a process by name
  ainb witr --pid 1234             Trace a process by PID
  ainb witr --port 3000            Trace whatever listens on a port
  ainb witr node --tree            Show the ancestry chain as a tree
  ainb witr node --format json     Machine-readable snapshot
```

## `ainb learnings`

Search your learnings knowledge base (via the learnings plugin)

```console
$ ainb learnings --help
Search your learnings knowledge base (via the learnings plugin)

Usage: ainb learnings [OPTIONS] [args]...

Arguments:
  [args]...  subcommand + flags, forwarded verbatim: search <query...> [--bm25] [-k N]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb learnings search "redis connection pooling"   Semantic search
  ainb learnings search rust async --bm25            Fast BM25 (no LLM rerank)
  ainb learnings search clap -k 5                    Top 5 hits
  ainb learnings search clap --format json           Machine-readable hits
```

## `ainb plugin`

Manage ainb plugins

```console
$ ainb plugin --help
Manage ainb plugins

Usage: ainb plugin [OPTIONS] <COMMAND>

Commands:
  install      Install a plugin from a marketplace (NOT YET IMPLEMENTED)
  update       Update an installed plugin to the latest matching version (NOT YET IMPLEMENTED)
  remove       Remove an installed plugin (NOT YET IMPLEMENTED)
  list         List installed plugins
  search       Search registered marketplaces by plugin name (NOT YET IMPLEMENTED)
  marketplace  Manage marketplace registries (NOT YET IMPLEMENTED)
  lint         Validate a plugin manifest + binary (ABI 2.0 sanity checks)
  watch        Live-tail lifecycle + snapshot events for a registered plugin
  tail         Stream the host's tracing layer filtered to a single plugin id
  help         Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb plugin list                 Installed plugins
  ainb plugin lint ./my-plugin     Validate a manifest + binary
  ainb plugin watch burndown       Live-tail a plugin's events
  ainb plugin tail burndown --level info
```

### `ainb plugin install`

Install a plugin from a marketplace (NOT YET IMPLEMENTED)

```console
$ ainb plugin install --help
Install a plugin from a marketplace (NOT YET IMPLEMENTED)

Usage: ainb plugin install [OPTIONS] <plugin>

Arguments:
  <plugin>  plugin id, e.g. burndown or ainb-plugins/burndown@0.1.0

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -y, --yes              skip the capability approval prompt
  -h, --help             Print help
```

### `ainb plugin update`

Update an installed plugin to the latest matching version (NOT YET IMPLEMENTED)

```console
$ ainb plugin update --help
Update an installed plugin to the latest matching version (NOT YET IMPLEMENTED)

Usage: ainb plugin update [OPTIONS] <plugin>

Arguments:
  <plugin>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -y, --yes              skip prompts when new capabilities are requested
  -h, --help             Print help
```

### `ainb plugin remove`

Remove an installed plugin (NOT YET IMPLEMENTED)

```console
$ ainb plugin remove --help
Remove an installed plugin (NOT YET IMPLEMENTED)

Usage: ainb plugin remove [OPTIONS] <plugin>

Arguments:
  <plugin>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -y, --yes              skip the data-directory deletion prompt
  -h, --help             Print help
```

### `ainb plugin list`

List installed plugins

```console
$ ainb plugin list --help
List installed plugins

Usage: ainb plugin list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb plugin search`

Search registered marketplaces by plugin name (NOT YET IMPLEMENTED)

```console
$ ainb plugin search --help
Search registered marketplaces by plugin name (NOT YET IMPLEMENTED)

Usage: ainb plugin search [OPTIONS] <query>

Arguments:
  <query>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb plugin marketplace`

Manage marketplace registries (NOT YET IMPLEMENTED)

```console
$ ainb plugin marketplace --help
Manage marketplace registries (NOT YET IMPLEMENTED)

Usage: ainb plugin marketplace [OPTIONS] <COMMAND>

Commands:
  add     Register a marketplace by URL or local path (NOT YET IMPLEMENTED)
  remove  Unregister a marketplace by name (NOT YET IMPLEMENTED)
  list    List registered marketplaces (NOT YET IMPLEMENTED)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb plugin marketplace add`

Register a marketplace by URL or local path (NOT YET IMPLEMENTED)

```console
$ ainb plugin marketplace add --help
Register a marketplace by URL or local path (NOT YET IMPLEMENTED)

Usage: ainb plugin marketplace add [OPTIONS] <url>

Arguments:
  <url>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb plugin marketplace remove`

Unregister a marketplace by name (NOT YET IMPLEMENTED)

```console
$ ainb plugin marketplace remove --help
Unregister a marketplace by name (NOT YET IMPLEMENTED)

Usage: ainb plugin marketplace remove [OPTIONS] <name>

Arguments:
  <name>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb plugin marketplace list`

List registered marketplaces (NOT YET IMPLEMENTED)

```console
$ ainb plugin marketplace list --help
List registered marketplaces (NOT YET IMPLEMENTED)

Usage: ainb plugin marketplace list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb plugin lint`

Validate a plugin manifest + binary (ABI 2.0 sanity checks)

```console
$ ainb plugin lint --help
Validate a plugin manifest + binary (ABI 2.0 sanity checks)

Usage: ainb plugin lint [OPTIONS] <plugin>

Arguments:
  <plugin>  plugin id, staging dir, or manifest.toml path

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb plugin watch`

Live-tail lifecycle + snapshot events for a registered plugin

```console
$ ainb plugin watch --help
Live-tail lifecycle + snapshot events for a registered plugin

Usage: ainb plugin watch [OPTIONS] <plugin>

Arguments:
  <plugin>  plugin id (matches `ainb plugin list`)

Options:
      --duration <duration>  seconds to watch before exiting (default 30)
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help                 Print help
```

### `ainb plugin tail`

Stream the host's tracing layer filtered to a single plugin id

```console
$ ainb plugin tail --help
Stream the host's tracing layer filtered to a single plugin id

Usage: ainb plugin tail [OPTIONS] <plugin>

Arguments:
  <plugin>  plugin id (matches `ainb plugin list`)

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --level <level>        min log level: trace|debug|info|warn|error (default debug)
      --since <since>        RFC-3339 timestamp; suppresses events older than this
      --duration <duration>  seconds to tail before exiting (default 30)
  -h, --help                 Print help
```

## `ainb fleet`

Fleet orchestration: standup / broadcast / sequence / needs / cost / daemon / daemons / atc / bridge

```console
$ ainb fleet --help
Fleet orchestration: standup / broadcast / sequence / needs / cost / daemon / daemons / atc / bridge

Usage: ainb fleet [OPTIONS] <COMMAND>

Commands:
  approve       Approve a session's pending permission request (no arg: list waiters)
  deny          Deny a session's pending permission request (no arg: list waiters)
  standup       Live fleet status: every claude session across ainb + peers + bg jobs
  broadcast     Send one prompt to selected sessions (peers-first, tmux fallback)
  sequence      Ordered prompts with ack between steps
  needs         Center control panel — sessions blocked on input / errors / idle / waiting
  cost          Per-session / model / day / group spend rollups + budget caps
  daemon        Watcher: registers as ainb-fleet-cp peer, auto-continues API errors
  daemons       Unified runtime health of every long-running daemon (phone bridge / notifyd / ATC / fleet daemon)
  atc           Air Traffic Control — the persistent fleet brain (setup / status / list / teardown)
  bridge        Native phone bridge (Telegram + Slack): relay messages two-way to ainb sessions
  enrich-cache  Content-addressed enrich cache (the producer's write path)
  help          Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb fleet standup               Live status of all sessions
  ainb fleet needs                 Sessions blocked on input / errors
  ainb fleet broadcast "git pull" --all     Send a prompt to every session
  ainb fleet sequence "step 1" "step 2"     Ordered prompts with ack between steps
  ainb fleet approve               List sessions waiting on a permission decision
  ainb fleet approve <session-id>  Approve that session's pending permission request
  ainb fleet deny <session-id> --reason "not now"   Deny it, with a reason
```

### `ainb fleet approve`

Approve a session's pending permission request (no arg: list waiters)

```console
$ ainb fleet approve --help
Approve a session's pending permission request (no arg: list waiters)

Usage: ainb fleet approve [OPTIONS] [session-id]

Arguments:
  [session-id]  Session blocked on a permission decision (omit to list waiters)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --reason <reason>  Optional reason relayed to the agent with the decision [default: ""]
  -h, --help             Print help
```

### `ainb fleet deny`

Deny a session's pending permission request (no arg: list waiters)

```console
$ ainb fleet deny --help
Deny a session's pending permission request (no arg: list waiters)

Usage: ainb fleet deny [OPTIONS] [session-id]

Arguments:
  [session-id]  Session blocked on a permission decision (omit to list waiters)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --reason <reason>  Optional reason relayed to the agent with the decision [default: ""]
  -h, --help             Print help
```

### `ainb fleet standup`

Live fleet status: every claude session across ainb + peers + bg jobs

```console
$ ainb fleet standup --help
Live fleet status: every claude session across ainb + peers + bg jobs

Usage: ainb fleet standup [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --text             Force text output even with --format json
      --no-enrich        Skip AI enrichment — 0-token output (env AINB_FLEET_ENRICH=0)
  -h, --help             Print help
```

### `ainb fleet broadcast`

Send one prompt to selected sessions (peers-first, tmux fallback)

```console
$ ainb fleet broadcast --help
Send one prompt to selected sessions (peers-first, tmux fallback)

Usage: ainb fleet broadcast [OPTIONS] <prompt>

Arguments:
  <prompt>  

Options:
      --all              Fan out to every running session
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --filter <filter>  Regex against tmux/workspace name
      --cwd <cwd>        Substring against cwd
  -h, --help             Print help
```

### `ainb fleet sequence`

Ordered prompts with ack between steps

```console
$ ainb fleet sequence --help
Ordered prompts with ack between steps

Usage: ainb fleet sequence [OPTIONS] <steps>...

Arguments:
  <steps>...  

Options:
      --all                
      --format <format>    Output format [default: text] [possible values: text, json, csv, markdown]
      --timeout <timeout>  Per-step timeout (seconds) [default: 300]
  -h, --help               Print help
```

### `ainb fleet needs`

Center control panel — sessions blocked on input / errors / idle / waiting

```console
$ ainb fleet needs --help
Center control panel — sessions blocked on input / errors / idle / waiting

Usage: ainb fleet needs [OPTIONS]

Options:
      --format <format>      Output format [default: text] [possible values: text, json, csv, markdown]
      --idle-min <idle-min>  Minutes of assistant silence before flagging IDLE (default 5, env AINB_FLEET_IDLE_MIN)
      --no-enrich            Skip AI enrichment — 0-token HUD (env AINB_FLEET_ENRICH=0)
  -h, --help                 Print help
```

### `ainb fleet cost`

Per-session / model / day / group spend rollups + budget caps

```console
$ ainb fleet cost --help
Per-session / model / day / group spend rollups + budget caps

Usage: ainb fleet cost [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --period <period>  Reporting window passed to the burndown plugin [default: month] [possible values: today, week, 30days, month, all]
  -h, --help             Print help
```

### `ainb fleet daemon`

Watcher: registers as ainb-fleet-cp peer, auto-continues API errors

```console
$ ainb fleet daemon --help
Watcher: registers as ainb-fleet-cp peer, auto-continues API errors

Usage: ainb fleet daemon [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -v, --verbose          
  -h, --help             Print help
```

### `ainb fleet daemons`

Unified runtime health of every long-running daemon (phone bridge / notifyd / ATC / fleet daemon)

```console
$ ainb fleet daemons --help
Unified runtime health of every long-running daemon (phone bridge / notifyd / ATC / fleet daemon)

Usage: ainb fleet daemons [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb fleet atc`

Air Traffic Control — the persistent fleet brain (setup / status / list / teardown)

```console
$ ainb fleet atc --help
Air Traffic Control — the persistent fleet brain (setup / status / list / teardown)

Usage: ainb fleet atc [OPTIONS] <COMMAND>

Commands:
  setup     Provision an ATC instance: CLAUDE.md policy + meta + heartbeat timer + session
  teardown  Remove an ATC instance's heartbeat timer + session
  status    Report one ATC instance (meta + timer + session liveness)
  list      List all provisioned ATC instances
  inbox     Inspect / drain / commit a parent's durable completion inbox
  help      Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet atc setup`

Provision an ATC instance: CLAUDE.md policy + meta + heartbeat timer + session

```console
$ ainb fleet atc setup --help
Provision an ATC instance: CLAUDE.md policy + meta + heartbeat timer + session

Usage: ainb fleet atc setup [OPTIONS] <name>

Arguments:
  <name>  Instance name (also the session name)

Options:
      --format <format>          Output format [default: text] [possible values: text, json, csv, markdown]
      --interval <interval>      Heartbeat cadence in minutes (default 15)
      --idle-pause <idle-pause>  Minutes of fleet quiet before the heartbeat downgrades to an idle ping (default 60)
      --no-heartbeat             Provision without installing the OS heartbeat timer
      --no-spawn                 Provision files + timer but do not spawn the ainb session
      --no-hooks                 Skip installing the event-driven lifecycle hooks into ~/.claude/settings.json (poll-mode only)
  -h, --help                     Print help
```

#### `ainb fleet atc teardown`

Remove an ATC instance's heartbeat timer + session

```console
$ ainb fleet atc teardown --help
Remove an ATC instance's heartbeat timer + session

Usage: ainb fleet atc teardown [OPTIONS] <name>

Arguments:
  <name>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --purge            Also delete the instance dir (state.json + task-log.md)
  -h, --help             Print help
```

#### `ainb fleet atc status`

Report one ATC instance (meta + timer + session liveness)

```console
$ ainb fleet atc status --help
Report one ATC instance (meta + timer + session liveness)

Usage: ainb fleet atc status [OPTIONS] <name>

Arguments:
  <name>  

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet atc list`

List all provisioned ATC instances

```console
$ ainb fleet atc list --help
List all provisioned ATC instances

Usage: ainb fleet atc list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet atc inbox`

Inspect / drain / commit a parent's durable completion inbox

```console
$ ainb fleet atc inbox --help
Inspect / drain / commit a parent's durable completion inbox

Usage: ainb fleet atc inbox [OPTIONS] <COMMAND>

Commands:
  peek    Show undrained completions without consuming them
  drain   Drain completions exactly-once and print the Stop-drain decision
  commit  Commit a child completion to a parent's inbox (testing/integration)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb fleet bridge`

Native phone bridge (Telegram + Slack): relay messages two-way to ainb sessions

```console
$ ainb fleet bridge --help
Native phone bridge (Telegram + Slack): relay messages two-way to ainb sessions

Usage: ainb fleet bridge [OPTIONS] [COMMAND]

Commands:
  run        Run the bridge daemon in the foreground (default; reads config.toml)
  install    Install as a launchd/systemd service (tokens read from config, never argv)
  uninstall  Remove the bridge service
  status     Report bridge service install status
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet bridge run`

Run the bridge daemon in the foreground (default; reads config.toml)

```console
$ ainb fleet bridge run --help
Run the bridge daemon in the foreground (default; reads config.toml)

Usage: ainb fleet bridge run [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet bridge install`

Install as a launchd/systemd service (tokens read from config, never argv)

```console
$ ainb fleet bridge install --help
Install as a launchd/systemd service (tokens read from config, never argv)

Usage: ainb fleet bridge install [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet bridge uninstall`

Remove the bridge service

```console
$ ainb fleet bridge uninstall --help
Remove the bridge service

Usage: ainb fleet bridge uninstall [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet bridge status`

Report bridge service install status

```console
$ ainb fleet bridge status --help
Report bridge service install status

Usage: ainb fleet bridge status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb fleet enrich-cache`

Content-addressed enrich cache (the producer's write path)

```console
$ ainb fleet enrich-cache --help
Content-addressed enrich cache (the producer's write path)

Usage: ainb fleet enrich-cache [OPTIONS] <COMMAND>

Commands:
  put   Store a drafted suggestion under a card's enrich_key
  get   Read a cached suggestion by enrich_key (exit non-zero on miss)
  help  Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb fleet enrich-cache put`

Store a drafted suggestion under a card's enrich_key

```console
$ ainb fleet enrich-cache put --help
Store a drafted suggestion under a card's enrich_key

Usage: ainb fleet enrich-cache put [OPTIONS] --key <key> --suggestion <suggestion>

Options:
      --format <format>          Output format [default: text] [possible values: text, json, csv, markdown]
      --key <key>                
      --suggestion <suggestion>  
  -h, --help                     Print help
```

#### `ainb fleet enrich-cache get`

Read a cached suggestion by enrich_key (exit non-zero on miss)

```console
$ ainb fleet enrich-cache get --help
Read a cached suggestion by enrich_key (exit non-zero on miss)

Usage: ainb fleet enrich-cache get [OPTIONS] --key <key>

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --key <key>        
  -h, --help             Print help
```

## `ainb headroom`

Manage the ainb-managed Headroom compression proxy

```console
$ ainb headroom --help
Manage the ainb-managed Headroom compression proxy

Usage: ainb headroom [OPTIONS] <COMMAND>

Commands:
  status  Query the Headroom proxy (running, port, pid, tokens saved)
  stop    Stop the ainb-managed Headroom proxy
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb headroom status    Is the proxy running? port / pid / tokens saved
  ainb headroom stop      Stop the ainb-managed Headroom proxy
```

### `ainb headroom status`

Query the Headroom proxy (running, port, pid, tokens saved)

```console
$ ainb headroom status --help
Query the Headroom proxy (running, port, pid, tokens saved)

Usage: ainb headroom status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb headroom stop`

Stop the ainb-managed Headroom proxy

```console
$ ainb headroom stop --help
Stop the ainb-managed Headroom proxy

Usage: ainb headroom stop [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## `ainb mcp`

Shared MCP server pool: daemon / proxy / status / stop / import / install

```console
$ ainb mcp --help
Shared MCP server pool: daemon / proxy / status / stop / import / install

Usage: ainb mcp [OPTIONS] <COMMAND>

Commands:
  daemon   Run the shared MCP pool daemon (foreground)
  proxy    Stdio shim: bridge this process's stdio onto a pool socket
  status   Query the pool daemon (JSON)
  stop     Stop the pool daemon (or one server with `stop <server>`)
  import   Import stdio servers from .mcp.json / Claude user scope into ainb config
  install  Point other agent CLIs' MCP configs at the pool shim
  help     Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb mcp status                  Query the pool daemon (JSON)
  ainb mcp import                  Import .mcp.json servers into ainb config
  ainb mcp import --user           Also import Claude user-scope servers
  ainb mcp install --codex --copilot   Point other agent CLIs at the pool shim
  ainb mcp stop                    Stop the pool daemon
  ainb mcp stop <server>           Stop one pooled server
```

### `ainb mcp daemon`

Run the shared MCP pool daemon (foreground).

```console
$ ainb mcp daemon --help
Run the shared MCP pool daemon (foreground).

You rarely run this directly — `ainb run` and the TUI overlay's import auto-start it detached. There is exactly ONE daemon per user, keyed by the control socket at ~/.agents-in-a-box/mcp/sockets/control.sock: every `ainb` instance (and Codex/Copilot sessions wired via `ainb mcp install`) shares it, so N sessions share ONE child process per server. A second start is a no-op — it detects the live socket (or loses the bind race) and exits.

Lifecycle: servers spawn lazily on first attach; a server's child is reaped [mcp_pool].idle_grace_secs after its last client detaches (default 300); and the whole daemon exits after [mcp_pool].daemon_idle_grace_secs with no clients anywhere (default 900, 0 = never) so an unused or orphaned pool can't linger.

Usage: ainb mcp daemon [OPTIONS]

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --idle-grace <idle-grace>
          Override [mcp_pool].idle_grace_secs (seconds)

  -h, --help
          Print help (see a summary with '-h')
```

### `ainb mcp proxy`

Stdio shim: bridge this process's stdio onto a pool socket

```console
$ ainb mcp proxy --help
Stdio shim: bridge this process's stdio onto a pool socket

Usage: ainb mcp proxy [OPTIONS] <socket>

Arguments:
  <socket>  Unix socket path

Options:
      --format <format>    Output format [default: text] [possible values: text, json, csv, markdown]
      --session <session>  Session label to announce to the pool (shown in `ainb mcp status`)
  -h, --help               Print help
```

### `ainb mcp status`

Query the pool daemon (JSON)

```console
$ ainb mcp status --help
Query the pool daemon (JSON)

Usage: ainb mcp status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb mcp stop`

Stop the pool daemon (or one server with `stop <server>`)

```console
$ ainb mcp stop --help
Stop the pool daemon (or one server with `stop <server>`)

Usage: ainb mcp stop [OPTIONS] [server]

Arguments:
  [server]  Stop just this server (next attach respawns it); omit to stop the whole daemon

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb mcp import`

Import stdio servers from .mcp.json / Claude user scope into ainb config

```console
$ ainb mcp import --help
Import stdio servers from .mcp.json / Claude user scope into ainb config

Usage: ainb mcp import [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --user             Write to user config instead of project .ainb/config.toml
  -h, --help             Print help
```

### `ainb mcp install`

Point other agent CLIs' MCP configs at the pool shim

```console
$ ainb mcp install --help
Point other agent CLIs' MCP configs at the pool shim

Usage: ainb mcp install [OPTIONS]

Options:
      --codex            Wire ~/.codex/config.toml
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --copilot          Wire ~/.copilot/mcp-config.json
  -h, --help             Print help
```

## `ainb notifyd`

ainb-hooks notification daemon: status, restart (the approve-socket resume/repair command), install/uninstall hooks

```console
$ ainb notifyd --help
ainb-hooks notification daemon: status, restart (the approve-socket resume/repair command), install/uninstall hooks

Usage: ainb notifyd [OPTIONS] [COMMAND]

Commands:
  run        Run the daemon in the foreground (default)
  stop       Stop a running daemon via its PID file
  reap       Kill orphan / wedged notifyd processes, sparing the live owner
  restart    Stop, reap, and respawn the daemon — the single resume/repair command for a dead or wedged approve socket
  install    Install the ainb-hooks hook
  uninstall  Uninstall the ainb-hooks hook
  status     Report install + daemon status
  list       List persisted notifications (most recent first)
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb notifyd status              Install + daemon status
  ainb notifyd restart             Repair a dead/wedged approve socket
  ainb notifyd install --all       Install the hook for every agent
  ainb notifyd list --limit 20     Last 20 persisted notifications
```

### `ainb notifyd run`

Run the daemon in the foreground (default)

```console
$ ainb notifyd run --help
Run the daemon in the foreground (default)

Usage: ainb notifyd run [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb notifyd stop`

Stop a running daemon via its PID file

```console
$ ainb notifyd stop --help
Stop a running daemon via its PID file

Usage: ainb notifyd stop [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb notifyd reap`

Kill orphan / wedged notifyd processes, sparing the live owner

```console
$ ainb notifyd reap --help
Kill orphan / wedged notifyd processes, sparing the live owner

Usage: ainb notifyd reap [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb notifyd restart`

Stop, reap, and respawn the daemon — the single resume/repair command for a dead or wedged approve socket

```console
$ ainb notifyd restart --help
Stop, reap, and respawn the daemon — the single resume/repair command for a dead or wedged approve socket

Usage: ainb notifyd restart [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb notifyd install`

Install the ainb-hooks hook

```console
$ ainb notifyd install --help
Install the ainb-hooks hook

Usage: ainb notifyd install [OPTIONS]

Options:
      --claude           Target Claude Code
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --codex            Target Codex CLI
      --copilot          Target GitHub Copilot CLI
      --all              Target every known agent
  -h, --help             Print help
```

### `ainb notifyd uninstall`

Uninstall the ainb-hooks hook

```console
$ ainb notifyd uninstall --help
Uninstall the ainb-hooks hook

Usage: ainb notifyd uninstall [OPTIONS]

Options:
      --claude           Target Claude Code
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --codex            Target Codex CLI
      --copilot          Target GitHub Copilot CLI
      --all              Target every known agent
  -h, --help             Print help
```

### `ainb notifyd status`

Report install + daemon status

```console
$ ainb notifyd status --help
Report install + daemon status

Usage: ainb notifyd status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb notifyd list`

List persisted notifications (most recent first)

```console
$ ainb notifyd list --help
List persisted notifications (most recent first)

Usage: ainb notifyd list [OPTIONS]

Options:
      --dismissed          Include dismissed notifications
      --format <format>    Output format [default: text] [possible values: text, json, csv, markdown]
      --agent <agent>      Filter by agent (claude|codex|copilot)
      --project <project>  Filter by project (basename of cwd)
      --limit <limit>      Max rows to show [default: 50]
  -h, --help               Print help
```

## `ainb hangar`

Hangar managed-agents control plane (issue / task / beads / daemon)

```console
$ ainb hangar --help
Hangar managed-agents control plane (issue / task / beads / daemon)

Usage: ainb hangar [OPTIONS] <COMMAND>

Commands:
  issue      Manage Hangar issues
  task       Inspect and control Hangar tasks
  beads      Sync Hangar issues with the beads (`bd`) tracker
  daemon     Inspect the Hangar control-plane daemon
  auth       Manage Hangar auth tokens (PATs + daemon tokens)
  config     Configure Hangar (env allowlist, …)
  skills     Import + list workspace-scoped skills
  templates  List, inspect, and apply curated agent templates
  agent      Edit, archive, and list workspace agents
  member     List, re-role, and remove workspace members
  squad      Create squads, manage membership, and view squad status + leader
  autopilot  Create and control cron-scheduled autopilots
  workspace  View + set per-workspace config (context prompt, issue prefix, repo whitelist)
  logs       Read the daemon's structured logs
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb hangar daemon status        Is the control-plane daemon reachable?
  ainb hangar issue list           List Hangar issues
  ainb hangar task list            Inspect pending tasks
  ainb hangar logs tail --follow   Tail daemon logs
```

### `ainb hangar issue`

Manage Hangar issues

```console
$ ainb hangar issue --help
Manage Hangar issues

Usage: ainb hangar issue [OPTIONS] <COMMAND>

Commands:
  create    Create a new issue (bootstraps a default workspace on first use)
  list      List issues in the default workspace
  search    Search issues by title, description, or comment body (ranked)
  show      Show one issue by id
  update    Edit an existing issue's state, assignee, priority, or due date
  delete    Delete an issue and all its history (dry-run without `--yes`)
  label     Attach or detach a label on an issue
  criteria  Inspect or tick off an issue's acceptance criteria
  link      Add, remove, or list an issue's typed links to other issues
  why       Explain why an issue did (or did not) dispatch — its admission history
  timeline  Show one issue's activity timeline: state changes, assignments, comments
  help      Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar issue create`

Create a new issue (bootstraps a default workspace on first use)

```console
$ ainb hangar issue create --help
Create a new issue (bootstraps a default workspace on first use)

Usage: ainb hangar issue create [OPTIONS] --title <TITLE>

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --title <TITLE>
          Issue title

      --description <DESCRIPTION>
          Free-form description

      --state <STATE>
          Initial lifecycle state
          
          [default: open]

      --assign <ASSIGN>
          Assign the issue to an agent (`agent.id`) and enqueue a task for it.
          
          When set, the issue's assignee is the agent and a `queued` task is enqueued for the agent's runtime, so the daemon's claim loop picks it up, materialises the agent's attached skills (P6.4), and dispatches the provider. The created task id is printed alongside the issue id.

      --priority <PRIORITY>
          Urgency: 0..3 mapping P3..P0 — HIGHER = MORE URGENT (default 0).
          
          Stamped onto BOTH the created issue and (when `--assign` enqueues one) the task: the daemon's claim loop drains `priority DESC, created_at, id` (reference ordering parity), so a higher value jumps the queue while equal priorities stay FIFO.
          
          [default: 0]

      --due <DUE>
          Optional due date as `YYYY-MM-DD` (interpreted at UTC midnight).
          
          Persisted onto the issue as an epoch-millisecond deadline; omitted leaves the issue with no due date.

      --label <LABELS>
          A label to attach to the issue (repeatable: `--label bug --label p0`).
          
          Each name is resolve-or-created in the workspace and joined to the issue through the `label` / `issue_label` tables (migration 0016), so a repeated name yields exactly one attachment.

      --acceptance <ACCEPTANCE_CRITERIA>
          An acceptance criterion (repeatable: `--acceptance "x" --acceptance "y"`).
          
          Persisted as the issue's ordered acceptance-criteria list (migration 0048, multica parity); rendered on the detail card's `Acceptance:` block.

      --context-ref <CONTEXT_REFS>
          A context reference — URL / `owner/repo#123` / note (repeatable).
          
          Persisted as the issue's ordered context-reference list (migration 0048, multica parity); rendered on the detail card's `Context:` block.

      --repo <REPO>
          The repo the run executes in: an absolute checkout path, the literal `scratch`, or a REMOTE (`owner/repo`, a full URL, or `git@…`) — a remote is cloned once into the shared clone cache and the local path persisted, exactly like the board card-create path (migration 0032/0042)

      --source-branch <SOURCE_BRANCH>
          The SOURCE branch the run branches FROM (migration 0042); omitted uses the repo's default branch. Persisted on the issue AND the enqueued task

      --target-branch <TARGET_BRANCH>
          The TARGET branch a future PR lands INTO (migration 0042); stored on the issue for later PR automation

      --parent <PARENT>
          Make this a SUB-ISSUE of an existing issue (`issue.id`, migration 0046).
          
          The parent must exist in the same workspace; completing the last child of the lowest unfinished stage cascades a roll-up comment onto the parent.

      --origin-type <ORIGIN_TYPE>
          Provenance of this issue: `autopilot` | `comment_mention` | `manual` (migration 0056, multica parity #21).
          
          Defaults to `$HANGAR_ORIGIN_TYPE` — the daemon injects it into a dispatched agent's environment, so an issue an agent creates mid-run is attributable back to the comment / autopilot that asked for it. With neither flag nor env, a create is stamped `manual`.

      --origin-id <ORIGIN_ID>
          The provenance id: the autopilot id for `autopilot`, the comment id for `comment_mention`. REQUIRED for every kind except `manual`.
          
          Defaults to `$HANGAR_ORIGIN_ID`. Supplying an id with no `--origin-type` is an error, never a silent drop.

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar issue list`

List issues in the default workspace

```console
$ ainb hangar issue list --help
List issues in the default workspace

Usage: ainb hangar issue list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --state <STATE>    Restrict to issues in this lifecycle state [default: open]
  -h, --help             Print help
```

#### `ainb hangar issue search`

Search issues by title, description, or comment body (ranked)

```console
$ ainb hangar issue search --help
Search issues by title, description, or comment body (ranked)

Usage: ainb hangar issue search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  The text to search for (matched across title / description / comments)

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to search within. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar issue show`

Show one issue by id

```console
$ ainb hangar issue show --help
Show one issue by id

Usage: ainb hangar issue show [OPTIONS] <ID>

Arguments:
  <ID>  Issue id (ULID)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar issue update`

Edit an existing issue's state, assignee, priority, or due date

```console
$ ainb hangar issue update --help
Edit an existing issue's state, assignee, priority, or due date

Usage: ainb hangar issue update [OPTIONS] <ID>

Arguments:
  <ID>
          Issue id (ULID) to edit

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --state <STATE>
          New lifecycle state — one of `backlog`, `todo`, `in_progress`, `in_review`, `done`, `blocked`, `cancelled`; omitted leaves it

      --assign <ASSIGN>
          Reassign the issue to an agent (`agent.id`); omitted leaves the assignee.
          
          Mutually exclusive with `--unassign`.

      --unassign
          Clear the assignee (unassign the issue); omitted leaves it

      --priority <PRIORITY>
          New urgency 0..3 (P3..P0, HIGHER = MORE URGENT); omitted leaves it

      --due <DUE>
          New due date as `YYYY-MM-DD` (UTC midnight); omitted leaves it.
          
          Mutually exclusive with `--clear-due`.

      --clear-due
          Clear the due date (remove the deadline); omitted leaves it

      --workspace <WORKSPACE>
          Workspace slug the issue belongs to. Defaults to the bootstrapped `default` workspace

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar issue delete`

Delete an issue and all its history (dry-run without `--yes`)

```console
$ ainb hangar issue delete --help
Delete an issue and all its history (dry-run without `--yes`)

Usage: ainb hangar issue delete [OPTIONS] <ID>

Arguments:
  <ID>  Issue id (ULID) to delete

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --yes                    Actually perform the delete. Without this flag the command only PREVIEWS what would be removed and exits without touching the database
      --workspace <WORKSPACE>  Workspace slug the issue belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar issue label`

Attach or detach a label on an issue

```console
$ ainb hangar issue label --help
Attach or detach a label on an issue

Usage: ainb hangar issue label [OPTIONS] <COMMAND>

Commands:
  attach  Attach a label to an issue (resolve-or-creates the label; idempotent)
  detach  Detach a label from an issue (idempotent; the definition is kept)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar issue criteria`

Inspect or tick off an issue's acceptance criteria

```console
$ ainb hangar issue criteria --help
Inspect or tick off an issue's acceptance criteria

Usage: ainb hangar issue criteria [OPTIONS] <COMMAND>

Commands:
  list     List an issue's acceptance criteria with ordinal, id, and ☑/☐ state
  check    Tick a criterion off (by id or 1-based ordinal). Idempotent
  uncheck  Un-tick a criterion (by id or 1-based ordinal). Idempotent
  help     Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar issue link`

Add, remove, or list an issue's typed links to other issues

```console
$ ainb hangar issue link --help
Add, remove, or list an issue's typed links to other issues

Usage: ainb hangar issue link [OPTIONS] <COMMAND>

Commands:
  add     Link two issues. Re-adding a pair with a new kind replaces the kind
  remove  Remove a link between two issues. Idempotent
  list    List an issue's links (`🔒`/`✓` blocked-by, `→` blocks, `~` related)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar issue why`

Explain why an issue did (or did not) dispatch — its admission history

```console
$ ainb hangar issue why --help
Explain why an issue did (or did not) dispatch — its admission history

Usage: ainb hangar issue why [OPTIONS] <ID>

Arguments:
  <ID>  Issue id (ULID) whose dispatch history to explain

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --limit <LIMIT>          How many attempts to show, newest first [default: 20]
      --workspace <WORKSPACE>  Workspace slug the issue belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar issue timeline`

Show one issue's activity timeline: state changes, assignments, comments

```console
$ ainb hangar issue timeline --help
Show one issue's activity timeline: state changes, assignments, comments

Usage: ainb hangar issue timeline [OPTIONS] <ID>

Arguments:
  <ID>  Issue id (ULID) whose narrative to print

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --limit <LIMIT>          How many entries to show — the newest window, printed oldest-first [default: 200]
      --workspace <WORKSPACE>  Workspace slug the issue belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar task`

Inspect and control Hangar tasks

```console
$ ainb hangar task --help
Inspect and control Hangar tasks

Usage: ainb hangar task [OPTIONS] <COMMAND>

Commands:
  list    List pending (queued / dispatched) tasks
  cancel  Cancel a task (`{queued|dispatched|running} -> cancelled`)
  retry   Spawn a retry child for a retryable failed task
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar task list`

List pending (queued / dispatched) tasks

```console
$ ainb hangar task list --help
List pending (queued / dispatched) tasks

Usage: ainb hangar task list [OPTIONS]

Options:
      --format <format>    Output format [default: text] [possible values: text, json, csv, markdown]
      --runtime <RUNTIME>  Restrict to a single runtime id. When omitted, every runtime in the default workspace is scanned
  -h, --help               Print help
```

#### `ainb hangar task cancel`

Cancel a task (`{queued|dispatched|running} -> cancelled`)

```console
$ ainb hangar task cancel --help
Cancel a task (`{queued|dispatched|running} -> cancelled`)

Usage: ainb hangar task cancel [OPTIONS] <ID>

Arguments:
  <ID>  Task id (ULID)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar task retry`

Spawn a retry child for a retryable failed task

```console
$ ainb hangar task retry --help
Spawn a retry child for a retryable failed task

Usage: ainb hangar task retry [OPTIONS] <ID>

Arguments:
  <ID>  Task id (ULID)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb hangar beads`

Sync Hangar issues with the beads (`bd`) tracker

```console
$ ainb hangar beads --help
Sync Hangar issues with the beads (`bd`) tracker

Usage: ainb hangar beads [OPTIONS] <COMMAND>

Commands:
  reconcile  Walk the mapping table and repair Hangar <-> bd drift
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar beads reconcile`

Walk the mapping table and repair Hangar <-> bd drift

```console
$ ainb hangar beads reconcile --help
Walk the mapping table and repair Hangar <-> bd drift

Usage: ainb hangar beads reconcile [OPTIONS]

Options:
      --dry-run          Diff only — report drift without writing either side
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --label <LABEL>    Restrict to bd issues carrying this label (repeatable)
      --json             Emit the reconcile report as JSON instead of a summary line
  -h, --help             Print help
```

### `ainb hangar daemon`

Inspect the Hangar control-plane daemon

```console
$ ainb hangar daemon --help
Inspect the Hangar control-plane daemon

Usage: ainb hangar daemon [OPTIONS] <COMMAND>

Commands:
  status   Report whether the daemon is running (PID file + socket) and the database is reachable
  run      Run the daemon in the FOREGROUND (boot + claim loop until interrupted)
  start    Start the daemon as a BACKGROUND child, recording its PID
  stop     Stop the running daemon: signal the exact recorded PID, then remove the PID file
  restart  Restart the daemon: `stop` (if running) then `start`
  setup    One-command bring-up: ensure the store + socket-auth token, then `start`
  config   View + edit the daemon's user-config knobs (`list`/`get`/`set`)
  cred     Manage the one-time, host-wide `claude` credential the daemon injects into confined headless runs (`status`/`set`/`clear`)
  help     Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon status`

Report whether the daemon is running (PID file + socket) and the database is reachable

```console
$ ainb hangar daemon status --help
Report whether the daemon is running (PID file + socket) and the database is reachable

Usage: ainb hangar daemon status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon run`

Run the daemon in the FOREGROUND (boot + claim loop until interrupted).

```console
$ ainb hangar daemon run --help
Run the daemon in the FOREGROUND (boot + claim loop until interrupted).

This blocks; `start` is the background variant. Equivalent to launching the `ainb-hangar-daemon` binary directly.

Usage: ainb hangar daemon run [OPTIONS]

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar daemon start`

Start the daemon as a BACKGROUND child, recording its PID.

```console
$ ainb hangar daemon start --help
Start the daemon as a BACKGROUND child, recording its PID.

Spawns the `ainb-hangar-daemon` binary detached and writes its exact pid to `<hangar_home>/hangar/daemon.pid`. A no-op (with a notice) if a live daemon is already recorded.

Usage: ainb hangar daemon start [OPTIONS]

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar daemon stop`

Stop the running daemon: signal the exact recorded PID, then remove the PID file

```console
$ ainb hangar daemon stop --help
Stop the running daemon: signal the exact recorded PID, then remove the PID file

Usage: ainb hangar daemon stop [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon restart`

Restart the daemon: `stop` (if running) then `start`

```console
$ ainb hangar daemon restart --help
Restart the daemon: `stop` (if running) then `start`

Usage: ainb hangar daemon restart [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon setup`

One-command bring-up: ensure the store + socket-auth token, then `start`

```console
$ ainb hangar daemon setup --help
One-command bring-up: ensure the store + socket-auth token, then `start`

Usage: ainb hangar daemon setup [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon config`

View + edit the daemon's user-config knobs (`list`/`get`/`set`)

```console
$ ainb hangar daemon config --help
View + edit the daemon's user-config knobs (`list`/`get`/`set`)

Usage: ainb hangar daemon config [OPTIONS] <COMMAND>

Commands:
  list  List every configurable: key, current value (or default), default, type
  get   Print one knob's current value (or its default when unset)
  set   Validate + persist one knob's value (rejects unknown keys / bad values)
  help  Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar daemon cred`

Manage the one-time, host-wide `claude` credential the daemon injects into confined headless runs (`status`/`set`/`clear`)

```console
$ ainb hangar daemon cred --help
Manage the one-time, host-wide `claude` credential the daemon injects into confined headless runs (`status`/`set`/`clear`)

Usage: ainb hangar daemon cred [OPTIONS] <COMMAND>

Commands:
  status  Report whether a credential is configured and where it resolves from (env override / secret store / not set). Never prints the value
  set     Store a long-lived token. Reads the token from STDIN by default (so it never lands on argv or in shell history); `--setup-token` instead drives the interactive `claude setup-token` browser flow and captures the result
  clear   Remove the stored credential. Idempotent
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb hangar auth`

Manage Hangar auth tokens (PATs + daemon tokens)

```console
$ ainb hangar auth --help
Manage Hangar auth tokens (PATs + daemon tokens)

Usage: ainb hangar auth [OPTIONS] <COMMAND>

Commands:
  token  Manage personal access tokens
  help   Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar auth token`

Manage personal access tokens

```console
$ ainb hangar auth token --help
Manage personal access tokens

Usage: ainb hangar auth token [OPTIONS] <COMMAND>

Commands:
  create  Mint a new PAT. Prints the plaintext **once** — it is never recoverable
  list    List this user's PATs (id, scope, timestamps — never the plaintext)
  revoke  Revoke a PAT by id
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb hangar config`

Configure Hangar (env allowlist, …)

```console
$ ainb hangar config --help
Configure Hangar (env allowlist, …)

Usage: ainb hangar config [OPTIONS] <COMMAND>

Commands:
  env.allow  Manage the provider-subprocess env allowlist
  warnings   Manage danger-full-access warning acknowledgements
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar config env.allow`

Manage the provider-subprocess env allowlist

```console
$ ainb hangar config env.allow --help
Manage the provider-subprocess env allowlist

Usage: ainb hangar config env.allow [OPTIONS] <COMMAND>

Commands:
  list    Show the merged effective allowlist (`[deny-locked]` marks deny entries)
  add     Add an env-var name (or `*`-suffix glob) to the allowlist
  remove  Remove an env-var name from the allowlist
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar config warnings`

Manage danger-full-access warning acknowledgements

```console
$ ainb hangar config warnings --help
Manage danger-full-access warning acknowledgements

Usage: ainb hangar config warnings [OPTIONS] <COMMAND>

Commands:
  reset  Clear recorded warning acks so they show again
  help   Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb hangar skills`

Import + list workspace-scoped skills

```console
$ ainb hangar skills --help
Import + list workspace-scoped skills

Usage: ainb hangar skills [OPTIONS] <COMMAND>

Commands:
  sync    Import skills from a toolkit directory into a workspace (idempotent)
  list    List the skills imported into a workspace
  attach  Attach a skill to an agent (idempotent; never re-enables a disabled link)
  detach  Detach a skill from an agent (idempotent)
  toggle  Enable or disable an already-attached skill for one agent (parity #24)
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar skills sync`

Import skills from a toolkit directory into a workspace (idempotent)

```console
$ ainb hangar skills sync --help
Import skills from a toolkit directory into a workspace (idempotent)

Usage: ainb hangar skills sync [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to import into. Defaults to the bootstrapped `default` workspace
      --source <SOURCE>        Source directory holding `<name>/SKILL.md` skill dirs. Defaults to `$AINB_TOOLKIT_SKILLS_DIR`, else a walk up to `ainb-toolkit/skills`
      --dry-run                Print the skills that would be imported without writing anything
  -h, --help                   Print help
```

#### `ainb hangar skills list`

List the skills imported into a workspace

```console
$ ainb hangar skills list --help
List the skills imported into a workspace

Usage: ainb hangar skills list [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to list. Defaults to the bootstrapped `default` workspace
      --agent <AGENT>          List one agent's ATTACHMENTS (with their enabled/disabled state) instead of the workspace's skills. Accepts an agent id or its name
  -h, --help                   Print help
```

#### `ainb hangar skills attach`

Attach a skill to an agent (idempotent; never re-enables a disabled link)

```console
$ ainb hangar skills attach --help
Attach a skill to an agent (idempotent; never re-enables a disabled link)

Usage: ainb hangar skills attach [OPTIONS] --agent <AGENT> <SKILL>

Arguments:
  <SKILL>  Skill to link: its id, or its kebab-case name within the workspace

Options:
      --agent <AGENT>          Agent to link it to: its id, or its name within the workspace
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar skills detach`

Detach a skill from an agent (idempotent)

```console
$ ainb hangar skills detach --help
Detach a skill from an agent (idempotent)

Usage: ainb hangar skills detach [OPTIONS] --agent <AGENT> <SKILL>

Arguments:
  <SKILL>  Skill to link: its id, or its kebab-case name within the workspace

Options:
      --agent <AGENT>          Agent to link it to: its id, or its name within the workspace
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar skills toggle`

Enable or disable an already-attached skill for one agent (parity #24)

```console
$ ainb hangar skills toggle --help
Enable or disable an already-attached skill for one agent (parity #24)

Usage: ainb hangar skills toggle [OPTIONS] --agent <AGENT> --enabled <ENABLED> <SKILL>

Arguments:
  <SKILL>  Skill to toggle: its id, or its kebab-case name within the workspace

Options:
      --agent <AGENT>          Agent whose link is toggled: its id, or its name within the workspace
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --enabled <ENABLED>      `true` = the link materialises; `false` = it stays attached but is suppressed at dispatch [possible values: true, false]
      --workspace <WORKSPACE>  Workspace slug. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar templates`

List, inspect, and apply curated agent templates

```console
$ ainb hangar templates --help
List, inspect, and apply curated agent templates

Usage: ainb hangar templates [OPTIONS] <COMMAND>

Commands:
  list  List every embedded curated template
  show  Show one template in full (instructions + skill list)
  use   Create an agent from a template, attaching its bundled skills
  help  Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar templates list`

List every embedded curated template

```console
$ ainb hangar templates list --help
List every embedded curated template

Usage: ainb hangar templates list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar templates show`

Show one template in full (instructions + skill list)

```console
$ ainb hangar templates show --help
Show one template in full (instructions + skill list)

Usage: ainb hangar templates show [OPTIONS] <NAME>

Arguments:
  <NAME>  Template name (e.g. `code-reviewer`)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar templates use`

Create an agent from a template, attaching its bundled skills

```console
$ ainb hangar templates use --help
Create an agent from a template, attaching its bundled skills

Usage: ainb hangar templates use [OPTIONS] <NAME>

Arguments:
  <NAME>  Template name to apply (e.g. `code-reviewer`)

Options:
      --format <format>          Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>    Workspace slug to create the agent in. Defaults to the bootstrapped `default` workspace
      --agent-name <AGENT_NAME>  Name the created agent something other than the template name
  -h, --help                     Print help
```

### `ainb hangar agent`

Edit, archive, and list workspace agents

```console
$ ainb hangar agent --help
Edit, archive, and list workspace agents

Usage: ainb hangar agent [OPTIONS] <COMMAND>

Commands:
  create      Create a new agent from scratch (fills workspace/runtime/owner behind the scenes)
  list        List the workspace's agents (active by default; `--all` includes archived)
  edit        Edit an agent's config knobs (model / args / MCP / thinking / env / name)
  archive     Archive an agent (hide it from the active picker)
  unarchive   Un-archive an agent (restore it to the active picker)
  permission  Set an agent's invocation permission mode (gap #8: `private`/`public_to`)
  allow       Manage an agent's invocation allow-list (add/revoke/list a target)
  can-invoke  Report whether a user (or agent actor) may invoke an agent (`ALLOW`/`DENY`)
  env         Show an agent's per-agent env: variable NAMES only, values masked
  help        Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar agent create`

Create a new agent from scratch (fills workspace/runtime/owner behind the scenes)

```console
$ ainb hangar agent create --help
Create a new agent from scratch (fills workspace/runtime/owner behind the scenes)

Usage: ainb hangar agent create [OPTIONS] --name <NAME>

Options:
      --format <format>              Output format [default: text] [possible values: text, json, csv, markdown]
      --name <NAME>                  The new agent's name
      --provider <PROVIDER>          Provider to record (`claude`/`codex`/`copilot`); defaults to `claude`
      --model <MODEL>                Optional per-agent model override (e.g. `sonnet`, `gpt-5-codex`)
      --instructions <INSTRUCTIONS>  Optional instructions / system prompt for the agent
      --description <DESCRIPTION>    Optional short blurb rendered beside the agent (≤255 characters)
      --avatar <AVATAR>              Optional avatar token (e.g. `emoji:🦊`); omitted mints a random emoji
      --service-tier <SERVICE_TIER>  Optional Codex service tier (e.g. `priority`); omitted inherits the local Codex config. Stored + surfaced only — no dispatch-time override yet
      --workspace <WORKSPACE>        Workspace slug to create the agent in. Defaults to the bootstrapped `default` workspace (created if absent)
  -h, --help                         Print help
```

#### `ainb hangar agent list`

List the workspace's agents (active by default; `--all` includes archived)

```console
$ ainb hangar agent list --help
List the workspace's agents (active by default; `--all` includes archived)

Usage: ainb hangar agent list [OPTIONS]

Options:
      --all                    Include archived agents in the listing (default: active only)
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to list. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar agent edit`

Edit an agent's config knobs (model / args / MCP / thinking / env / name)

```console
$ ainb hangar agent edit --help
Edit an agent's config knobs (model / args / MCP / thinking / env / name)

Usage: ainb hangar agent edit [OPTIONS] <ID>

Arguments:
  <ID>  Agent id (ULID) to edit

Options:
      --format <format>              Output format [default: text] [possible values: text, json, csv, markdown]
      --name <NAME>                  Rename the agent; omitted leaves the name
      --instructions <INSTRUCTIONS>  New instructions; omitted leaves them. Mutually exclusive with `--clear-instructions`
      --clear-instructions           Clear the instructions; omitted leaves them
      --model <MODEL>                New model override (e.g. `claude-opus-4`); omitted leaves it. Mutually exclusive with `--clear-model`
      --clear-model                  Clear the model override (back to the provider default); omitted leaves it
      --arg <ARGS>                   A CLI arg to pass the provider (repeatable: `--arg --verbose --arg -x`). When ANY `--arg` is given the whole arg list is REPLACED with the values
      --mcp <MCP>                    New MCP config as a raw JSON-object string; omitted leaves it. Mutually exclusive with `--clear-mcp`
      --clear-mcp                    Clear the MCP config; omitted leaves it
      --thinking <THINKING>          New thinking level (e.g. `low`/`medium`/`high`); omitted leaves it. Mutually exclusive with `--clear-thinking`
      --clear-thinking               Clear the thinking level; omitted leaves it
      --env <ENV>                    A `KEY=VALUE` env var for the agent (repeatable; ANY `--env` REPLACES the whole map). Visible in `ps` / shell history — prefer `--env-stdin` / `--env-file` for secrets
      --env-stdin                    Read the whole env map from STDIN as a JSON object of string→string, keeping secrets off argv; `{}` clears it and empty input is an ERROR, not a clear
      --env-file <ENV_FILE>          Read the whole env map from a FILE as a JSON object of string→string (same contract as `--env-stdin`)
      --token-budget <TOKEN_BUDGET>  New token budget (rtk/headroom, migration 0042); omitted leaves it. Mutually exclusive with `--clear-token-budget`
      --clear-token-budget           Clear the token budget (back to unlimited); omitted leaves it
      --description <DESCRIPTION>    New description (≤255 characters); omitted leaves it. Pass `--description ""` to blank it (the column is NOT NULL, so `""` IS its cleared state)
      --avatar <AVATAR>              New avatar token; omitted leaves it. Mutually exclusive with `--clear-avatar`
      --clear-avatar                 Clear the avatar; omitted leaves it
      --service-tier <SERVICE_TIER>  New Codex service tier; omitted leaves it. Mutually exclusive with `--clear-service-tier`
      --clear-service-tier           Clear the service tier (back to inheriting the local Codex config)
      --workspace <WORKSPACE>        Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                         Print help
```

#### `ainb hangar agent archive`

Archive an agent (hide it from the active picker)

```console
$ ainb hangar agent archive --help
Archive an agent (hide it from the active picker)

Usage: ainb hangar agent archive [OPTIONS] <ID>

Arguments:
  <ID>  Agent id (ULID) to (un)archive

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
      --by <BY>                The `user.id` recorded as the archiving actor (migration 0052). Omitted defaults to the workspace owner — the ordinary single-operator archive
  -h, --help                   Print help
```

#### `ainb hangar agent unarchive`

Un-archive an agent (restore it to the active picker)

```console
$ ainb hangar agent unarchive --help
Un-archive an agent (restore it to the active picker)

Usage: ainb hangar agent unarchive [OPTIONS] <ID>

Arguments:
  <ID>  Agent id (ULID) to (un)archive

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
      --by <BY>                The `user.id` recorded as the archiving actor (migration 0052). Omitted defaults to the workspace owner — the ordinary single-operator archive
  -h, --help                   Print help
```

#### `ainb hangar agent permission`

Set an agent's invocation permission mode (gap #8: `private`/`public_to`)

```console
$ ainb hangar agent permission --help
Set an agent's invocation permission mode (gap #8: `private`/`public_to`)

Usage: ainb hangar agent permission [OPTIONS] --mode <MODE> <ID>

Arguments:
  <ID>  Agent id (ULID) to set the permission mode on

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --mode <MODE>            The new mode: `private` (owner-only, deny-by-default) or `public_to` (the allow-list decides)
      --workspace <WORKSPACE>  Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar agent allow`

Manage an agent's invocation allow-list (add/revoke/list a target)

```console
$ ainb hangar agent allow --help
Manage an agent's invocation allow-list (add/revoke/list a target)

Usage: ainb hangar agent allow [OPTIONS] <ID>

Arguments:
  <ID>  Agent id (ULID) whose allow-list to manage

Options:
      --format <format>
          Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace
          Grant/revoke the WHOLE workspace (a workspace target). Mutually exclusive with `--member` / `--team`
      --member <MEMBER>
          Grant/revoke a specific member (a user id or email). Mutually exclusive with `--workspace` / `--team`
      --team <TEAM>
          Grant/revoke a reserved team target (inert in V1). Mutually exclusive with `--workspace` / `--member`
      --revoke
          Remove the named target instead of adding it
      --list
          Print the current allow-list (ignores the target flags)
      --workspace-slug <WORKSPACE_SLUG>
          Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help
          Print help
```

#### `ainb hangar agent can-invoke`

Report whether a user (or agent actor) may invoke an agent (`ALLOW`/`DENY`)

```console
$ ainb hangar agent can-invoke --help
Report whether a user (or agent actor) may invoke an agent (`ALLOW`/`DENY`)

Usage: ainb hangar agent can-invoke [OPTIONS] --as <AS_USER> <ID>

Arguments:
  <ID>  Agent id (ULID) to test invocation on

Options:
      --as <AS_USER>           The invoking user id or email to judge the run by
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --actor <ACTOR>          Treat the invoker as an `agent` actor (no resolved originator) rather than a `member`. Exercises the A2A / workspaceBroad path
      --workspace <WORKSPACE>  Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar agent env`

Show an agent's per-agent env: variable NAMES only, values masked

```console
$ ainb hangar agent env --help
Show an agent's per-agent env: variable NAMES only, values masked

Usage: ainb hangar agent env [OPTIONS] <ID>

Arguments:
  <ID>  Agent id (ULID) to inspect

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the agent belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar member`

List, re-role, and remove workspace members

```console
$ ainb hangar member --help
List, re-role, and remove workspace members

Usage: ainb hangar member [OPTIONS] <COMMAND>

Commands:
  add       Add a human member (find-or-create the user by email, then join)
  list      List the workspace's members (email + role)
  set-role  Change a member's role (`owner` / `admin` / `member`)
  remove    Remove a member from the workspace (the user row survives)
  help      Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar member add`

Add a human member (find-or-create the user by email, then join)

```console
$ ainb hangar member add --help
Add a human member (find-or-create the user by email, then join)

Usage: ainb hangar member add [OPTIONS] --email <EMAIL>

Options:
      --email <EMAIL>
          The member's email (find-or-create the user by this address)

      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --role <ROLE>
          The role to grant: `owner`, `admin`, or `member` (default `member`)

          Possible values:
          - owner:  Full administrative control; a workspace must always keep one
          - admin:  Elevated management, short of ownership
          - member: A regular member
          
          [default: member]

      --workspace <WORKSPACE>
          Workspace slug to add the member to. Defaults to the bootstrapped `default` workspace

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar member list`

List the workspace's members (email + role)

```console
$ ainb hangar member list --help
List the workspace's members (email + role)

Usage: ainb hangar member list [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to list. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar member set-role`

Change a member's role (`owner` / `admin` / `member`)

```console
$ ainb hangar member set-role --help
Change a member's role (`owner` / `admin` / `member`)

Usage: ainb hangar member set-role [OPTIONS] <USER_ID> <ROLE>

Arguments:
  <USER_ID>
          The member's user id (`user.id`)

  <ROLE>
          The new role: `owner`, `admin`, or `member`

          Possible values:
          - owner:  Full administrative control; a workspace must always keep one
          - admin:  Elevated management, short of ownership
          - member: A regular member

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --workspace <WORKSPACE>
          Workspace slug the member belongs to. Defaults to the bootstrapped `default` workspace

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar member remove`

Remove a member from the workspace (the user row survives)

```console
$ ainb hangar member remove --help
Remove a member from the workspace (the user row survives)

Usage: ainb hangar member remove [OPTIONS] <USER_ID>

Arguments:
  <USER_ID>  The member's user id (`user.id`) to remove

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the member belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar squad`

Create squads, manage membership, and view squad status + leader

```console
$ ainb hangar squad --help
Create squads, manage membership, and view squad status + leader

Usage: ainb hangar squad [OPTIONS] <COMMAND>

Commands:
  list           List the workspace's squads (name, leader, members) — the status view
  create         Create a squad with a leader actor-ref (`agent:<id>` / `member:<id>`)
  add-member     Add a member actor to a squad (`agent:<id>` / `member:<id>`)
  remove-member  Remove a member actor from a squad (`agent:<id>` / `member:<id>`)
  assign         Route a task to the squad's LEADER (leader routing taking effect)
  archive        Archive a squad: it leaves the active list and refuses new assignments
  unarchive      Restore an archived squad (clears the archive audit stamp)
  member-role    Set or clear an existing member's free-text role on a squad
  instructions   Show, set, or clear a squad's user-authored routing instructions
  briefing       Print the leader briefing this squad would inject into a leader run
  help           Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar squad list`

List the workspace's squads (name, leader, members) — the status view

```console
$ ainb hangar squad list --help
List the workspace's squads (name, leader, members) — the status view

Usage: ainb hangar squad list [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to list. Defaults to the bootstrapped `default` workspace
      --all                    Include ARCHIVED squads (migration 0052). The default list is active-only
  -h, --help                   Print help
```

#### `ainb hangar squad create`

Create a squad with a leader actor-ref (`agent:<id>` / `member:<id>`)

```console
$ ainb hangar squad create --help
Create a squad with a leader actor-ref (`agent:<id>` / `member:<id>`)

Usage: ainb hangar squad create [OPTIONS] --leader <LEADER> <NAME>

Arguments:
  <NAME>  The squad name (unique within the workspace)

Options:
      --format <format>              Output format [default: text] [possible values: text, json, csv, markdown]
      --leader <LEADER>              The squad leader as an actor-ref (`agent:<id>` / `member:<id>`). An `agent` leader is the actor a squad-assigned task is routed to
      --instructions <INSTRUCTIONS>  Initial routing guidance for the squad, rendered VERBATIM as the leader briefing's `## Squad Instructions` section. Omitted leaves it empty, and a blank field omits that section entirely
      --workspace <WORKSPACE>        Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                         Print help
```

#### `ainb hangar squad add-member`

Add a member actor to a squad (`agent:<id>` / `member:<id>`)

```console
$ ainb hangar squad add-member --help
Add a member actor to a squad (`agent:<id>` / `member:<id>`)

Usage: ainb hangar squad add-member [OPTIONS] --member <MEMBER> <SQUAD_ID>

Arguments:
  <SQUAD_ID>  The squad id (`squad.id`) to mutate

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --member <MEMBER>        The member actor-ref (`agent:<id>` / `member:<id>`)
      --role <ROLE>            Free-text role for the ADDED member ("owns the migrations"), which the squad leader reads in its briefing. Honoured by `add-member` and IGNORED by `remove-member`. Omitted leaves an existing member's role untouched
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar squad remove-member`

Remove a member actor from a squad (`agent:<id>` / `member:<id>`)

```console
$ ainb hangar squad remove-member --help
Remove a member actor from a squad (`agent:<id>` / `member:<id>`)

Usage: ainb hangar squad remove-member [OPTIONS] --member <MEMBER> <SQUAD_ID>

Arguments:
  <SQUAD_ID>  The squad id (`squad.id`) to mutate

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --member <MEMBER>        The member actor-ref (`agent:<id>` / `member:<id>`)
      --role <ROLE>            Free-text role for the ADDED member ("owns the migrations"), which the squad leader reads in its briefing. Honoured by `add-member` and IGNORED by `remove-member`. Omitted leaves an existing member's role untouched
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar squad assign`

Route a task to the squad's LEADER (leader routing taking effect)

```console
$ ainb hangar squad assign --help
Route a task to the squad's LEADER (leader routing taking effect)

Usage: ainb hangar squad assign [OPTIONS] <SQUAD_ID>

Arguments:
  <SQUAD_ID>  The squad id (`squad.id`) whose leader the task routes to

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --issue <ISSUE>          The issue the routed task carries (`issue.id`), or omit for an ad-hoc task
      --work-dir <WORK_DIR>    The run's working directory, or omit
      --priority <PRIORITY>    Claim urgency (0..3, higher = more urgent). Defaults to `0` (routine) [default: 0]
      --fanout                 Fan the work out across the WHOLE squad (leader brief + one task per distinct `agent` member) instead of briefing the leader alone
      --invoker <INVOKER>      The user the invocation-permission gate judges this assignment by (a user id or an email). Omitted defaults to the workspace owner — the ordinary single-operator assign, which the gate always admits
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar squad archive`

Archive a squad: it leaves the active list and refuses new assignments

```console
$ ainb hangar squad archive --help
Archive a squad: it leaves the active list and refuses new assignments

Usage: ainb hangar squad archive [OPTIONS] <ID>

Arguments:
  <ID>  Squad id to (un)archive

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
      --by <BY>                The `user.id` recorded as the archiving actor (migration 0052). Omitted defaults to the workspace owner
  -h, --help                   Print help
```

#### `ainb hangar squad unarchive`

Restore an archived squad (clears the archive audit stamp)

```console
$ ainb hangar squad unarchive --help
Restore an archived squad (clears the archive audit stamp)

Usage: ainb hangar squad unarchive [OPTIONS] <ID>

Arguments:
  <ID>  Squad id to (un)archive

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
      --by <BY>                The `user.id` recorded as the archiving actor (migration 0052). Omitted defaults to the workspace owner
  -h, --help                   Print help
```

#### `ainb hangar squad member-role`

Set or clear an existing member's free-text role on a squad

```console
$ ainb hangar squad member-role --help
Set or clear an existing member's free-text role on a squad

Usage: ainb hangar squad member-role [OPTIONS] --member <MEMBER> <SQUAD_ID>

Arguments:
  <SQUAD_ID>  The squad id (`squad.id`) whose membership to edit

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --member <MEMBER>        The existing member actor-ref (`agent:<id>` / `member:<id>`)
      --role <ROLE>            The free-text role label. Pass an empty string to clear it [default: ""]
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar squad instructions`

Show, set, or clear a squad's user-authored routing instructions

```console
$ ainb hangar squad instructions --help
Show, set, or clear a squad's user-authored routing instructions

Usage: ainb hangar squad instructions [OPTIONS] <SQUAD_ID>

Arguments:
  <SQUAD_ID>  The squad id (`squad.id`) to read or edit

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --set <SET>              Replace the squad's instructions with this text (stored verbatim)
      --clear                  Clear the squad's instructions, so the leader briefing omits the section
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

#### `ainb hangar squad briefing`

Print the leader briefing this squad would inject into a leader run

```console
$ ainb hangar squad briefing --help
Print the leader briefing this squad would inject into a leader run

Usage: ainb hangar squad briefing [OPTIONS] <ID>

Arguments:
  <ID>  Squad id whose leader briefing to render

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug the squad belongs to. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar autopilot`

Create and control cron-scheduled autopilots

```console
$ ainb hangar autopilot --help
Create and control cron-scheduled autopilots

Usage: ainb hangar autopilot [OPTIONS] <COMMAND>

Commands:
  create       Create a cron-scheduled autopilot (rejects an invalid cron expression)
  list         List the workspace's autopilots (cron, next tick, last run, enabled)
  disable      Disable an autopilot so the scheduler stops firing it
  enable       Re-enable an autopilot, recomputing its next tick from now
  edit         Edit an autopilot's config (cron / agent / instructions / policy). A substantive edit appends a rule version naming the accountable human; a rename alone is cosmetic and mints none
  versions     Show the autopilot's rule-version ledger (who published what, when)
  run          Fire one tick immediately, bypassing the schedule (`--source` picks the trigger recorded on the run: `manual` by default, or `api`)
  api-trigger  Arm (or `--disable`) the bare programmatic `api` trigger
  runs         List the autopilot's recent runs (status, trigger source, reason)
  webhook      Configure the HTTP webhook trigger (enable/disable, rotate secret, filter)
  deliveries   List the autopilot's recent webhook deliveries (audit log)
  help         Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar autopilot create`

Create a cron-scheduled autopilot (rejects an invalid cron expression)

```console
$ ainb hangar autopilot create --help
Create a cron-scheduled autopilot (rejects an invalid cron expression)

Usage: ainb hangar autopilot create [OPTIONS] --name <NAME> --cron <CRON> --agent <AGENT>

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --name <NAME>
          Name, unique within the workspace

      --cron <CRON>
          Cron expression (UTC, 5-field) — validated before insert

      --agent <AGENT>
          Agent id to dispatch to at each tick (`agent.id`)

      --instructions <INSTRUCTIONS>
          Optional instructions handed to the agent on every tick

      --max-concurrent-runs <MAX_CONCURRENT_RUNS>
          Maximum simultaneous in-flight runs before the concurrency policy applies
          
          [default: 1]

      --execution-mode <EXECUTION_MODE>
          What a fired tick materialises: `run-only` (a task with no issue, the default) or `create-issue` (an issue plus a task against it)

          Possible values:
          - run-only:     Enqueue a task with no issue (the v1 default)
          - create-issue: Create an issue, then enqueue a task against it
          
          [default: run-only]

      --concurrency-policy <CONCURRENCY_POLICY>
          What the scheduler does when a tick comes due at the in-flight limit: `skip` (drop it, the default), `queue` (fire it anyway to run after the in-flight one), or `replace` (supersede the in-flight run and fire fresh)

          Possible values:
          - skip:    Drop a tick that comes due at the in-flight limit (the v1 default)
          - queue:   Fire the tick anyway; the queue runs it after the in-flight one
          - replace: Supersede the in-flight run and fire fresh
          
          [default: skip]

      --as-user <AS_USER>
          The ACCOUNTABLE HUMAN for this rule (`user.id` or email). Recorded on rule-version v1, which creation writes in the same transaction. Omitted defaults to the local human (`member:me`) — a CLI create always has a human at the keyboard

      --workspace <WORKSPACE>
          Workspace slug to create in. Defaults to the bootstrapped `default`

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar autopilot list`

List the workspace's autopilots (cron, next tick, last run, enabled)

```console
$ ainb hangar autopilot list --help
List the workspace's autopilots (cron, next tick, last run, enabled)

Usage: ainb hangar autopilot list [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to list. Defaults to the bootstrapped `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot disable`

Disable an autopilot so the scheduler stops firing it

```console
$ ainb hangar autopilot disable --help
Disable an autopilot so the scheduler stops firing it

Usage: ainb hangar autopilot disable [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --disable                Turn the trigger OFF instead of on (`api-trigger` only)
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --as-user <AS_USER>      The accountable human for this publish (`user.id` or email). Pausing, resuming and arming a trigger are all SUBSTANTIVE publishes, so each stamps a rule version. Defaults to the local human (`member:me`)
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot enable`

Re-enable an autopilot, recomputing its next tick from now

```console
$ ainb hangar autopilot enable --help
Re-enable an autopilot, recomputing its next tick from now

Usage: ainb hangar autopilot enable [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --disable                Turn the trigger OFF instead of on (`api-trigger` only)
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --as-user <AS_USER>      The accountable human for this publish (`user.id` or email). Pausing, resuming and arming a trigger are all SUBSTANTIVE publishes, so each stamps a rule version. Defaults to the local human (`member:me`)
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot edit`

Edit an autopilot's config (cron / agent / instructions / policy). A substantive edit appends a rule version naming the accountable human; a rename alone is cosmetic and mints none

```console
$ ainb hangar autopilot edit --help
Edit an autopilot's config (cron / agent / instructions / policy). A substantive edit appends a rule version naming the accountable human; a rename alone is cosmetic and mints none

Usage: ainb hangar autopilot edit [OPTIONS] <ID>

Arguments:
  <ID>
          The autopilot id (`autopilot.id`)

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --name <NAME>
          New display name (cosmetic on its own)

      --cron <CRON>
          New cron expression (UTC, 5-field) — revalidated before any write

      --agent <AGENT>
          Re-target the rule at a different agent (`agent.id`)

      --instructions <INSTRUCTIONS>
          New instructions handed to the agent on every tick

      --clear-instructions
          Clear the instructions entirely

      --max-concurrent-runs <MAX_CONCURRENT_RUNS>
          New maximum simultaneous in-flight runs

      --execution-mode <EXECUTION_MODE>
          New execution mode (`run-only` | `create-issue`)

          Possible values:
          - run-only:     Enqueue a task with no issue (the v1 default)
          - create-issue: Create an issue, then enqueue a task against it

      --concurrency-policy <CONCURRENCY_POLICY>
          New concurrency policy (`skip` | `queue` | `replace`)

          Possible values:
          - skip:    Drop a tick that comes due at the in-flight limit (the v1 default)
          - queue:   Fire the tick anyway; the queue runs it after the in-flight one
          - replace: Supersede the in-flight run and fire fresh

      --as-user <AS_USER>
          The ACCOUNTABLE HUMAN for this edit (`user.id` or email) — the name recorded on the minted rule version. Defaults to the local human (`member:me`)

      --workspace <WORKSPACE>
          Workspace slug the autopilot belongs to. Defaults to `default`

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar autopilot versions`

Show the autopilot's rule-version ledger (who published what, when)

```console
$ ainb hangar autopilot versions --help
Show the autopilot's rule-version ledger (who published what, when)

Usage: ainb hangar autopilot versions [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --limit <LIMIT>          Maximum number of versions to show (newest-first) [default: 20]
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot run`

Fire one tick immediately, bypassing the schedule (`--source` picks the trigger recorded on the run: `manual` by default, or `api`)

```console
$ ainb hangar autopilot run --help
Fire one tick immediately, bypassing the schedule (`--source` picks the trigger recorded on the run: `manual` by default, or `api`)

Usage: ainb hangar autopilot run [OPTIONS] <ID>

Arguments:
  <ID>
          The autopilot id (`autopilot.id`)

Options:
      --format <format>
          Output format
          
          [default: text]
          [possible values: text, json, csv, markdown]

      --source <SOURCE>
          Which trigger to record on the run (`manual` | `api`)

          Possible values:
          - manual: An operator firing by hand (the default)
          - api:    The bare programmatic `api` trigger; requires it to be armed
          
          [default: manual]

      --as-user <AS_USER>
          The human firing it (`user.id` or email). A `manual` run attributes to this human (`direct_human`) — them, not the rule's owner. An `api` run stays UNATTENDED (`rule_owner`), matching multica. Defaults to the local human (`member:me`)

      --workspace <WORKSPACE>
          Workspace slug the autopilot belongs to. Defaults to `default`

  -h, --help
          Print help (see a summary with '-h')
```

#### `ainb hangar autopilot api-trigger`

Arm (or `--disable`) the bare programmatic `api` trigger

```console
$ ainb hangar autopilot api-trigger --help
Arm (or `--disable`) the bare programmatic `api` trigger

Usage: ainb hangar autopilot api-trigger [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --disable                Turn the trigger OFF instead of on (`api-trigger` only)
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --as-user <AS_USER>      The accountable human for this publish (`user.id` or email). Pausing, resuming and arming a trigger are all SUBSTANTIVE publishes, so each stamps a rule version. Defaults to the local human (`member:me`)
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot runs`

List the autopilot's recent runs (status, trigger source, reason)

```console
$ ainb hangar autopilot runs --help
List the autopilot's recent runs (status, trigger source, reason)

Usage: ainb hangar autopilot runs [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --limit <LIMIT>          Maximum number of runs to show (latest-first) [default: 20]
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot webhook`

Configure the HTTP webhook trigger (enable/disable, rotate secret, filter)

```console
$ ainb hangar autopilot webhook --help
Configure the HTTP webhook trigger (enable/disable, rotate secret, filter)

Usage: ainb hangar autopilot webhook [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --disable                Disable the webhook (clears the secret); mutually exclusive with `--rotate`
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --rotate                 Mint a fresh signing secret for an already-enabled webhook (prints the new secret once)
      --event <EVENT>          Set the exact-match event filter (only this event name fires). Mutually exclusive with `--clear-event`
      --clear-event            Clear the event filter (fire on every signed request)
      --url-host <URL_HOST>    The host:port the webhook ingress listens on, used only to render the URL hint (defaults to `127.0.0.1:8718`). The daemon's actual bind is set by `AINB_HANGAR_WEBHOOK_PORT` [default: 127.0.0.1:8718]
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

#### `ainb hangar autopilot deliveries`

List the autopilot's recent webhook deliveries (audit log)

```console
$ ainb hangar autopilot deliveries --help
List the autopilot's recent webhook deliveries (audit log)

Usage: ainb hangar autopilot deliveries [OPTIONS] <ID>

Arguments:
  <ID>  The autopilot id (`autopilot.id`)

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --limit <LIMIT>          Maximum number of deliveries to show (latest-first) [default: 20]
      --workspace <WORKSPACE>  Workspace slug the autopilot belongs to. Defaults to `default`
  -h, --help                   Print help
```

### `ainb hangar workspace`

View + set per-workspace config (context prompt, issue prefix, repo whitelist)

```console
$ ainb hangar workspace --help
View + set per-workspace config (context prompt, issue prefix, repo whitelist)

Usage: ainb hangar workspace [OPTIONS] <COMMAND>

Commands:
  create  Create a new workspace (slug + display name)
  list    List every workspace on this instance
  config  Set one or more of the workspace's config knobs
  show    Show the workspace's current config
  help    Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar workspace create`

Create a new workspace (slug + display name)

```console
$ ainb hangar workspace create --help
Create a new workspace (slug + display name)

Usage: ainb hangar workspace create [OPTIONS] --slug <SLUG> --name <NAME>

Options:
      --format <format>              Output format [default: text] [possible values: text, json, csv, markdown]
      --slug <SLUG>                  Short handle for the workspace (`^[a-z0-9]+(-[a-z0-9]+)*$`), unique host-wide
      --name <NAME>                  Human-readable display name
      --issue-prefix <ISSUE_PREFIX>  Optional prefix prepended to a newly-created issue's title in this workspace (e.g. `OPS`). Omitted leaves titles verbatim
  -h, --help                         Print help
```

#### `ainb hangar workspace list`

List every workspace on this instance

```console
$ ainb hangar workspace list --help
List every workspace on this instance

Usage: ainb hangar workspace list [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar workspace config`

Set one or more of the workspace's config knobs

```console
$ ainb hangar workspace config --help
Set one or more of the workspace's config knobs

Usage: ainb hangar workspace config [OPTIONS]

Options:
      --context-prompt <CONTEXT_PROMPT>
          Set the context prompt injected into every agent run as a `CLAUDE.md`
      --format <format>
          Output format [default: text] [possible values: text, json, csv, markdown]
      --clear-context-prompt
          Unset the context prompt (back to no per-workspace context)
      --issue-prefix <ISSUE_PREFIX>
          Set the prefix prepended to a newly-created issue's title (e.g. `[OPS] `)
      --clear-issue-prefix
          Unset the issue prefix (titles used verbatim)
      --repo-whitelist <REPO_WHITELIST>
          Set the repo whitelist as a comma-separated list of `owner/name` slugs (e.g. `org/api,org/web`). The empty string sets a configured-but-empty whitelist (allows nothing); use `--clear-repo-whitelist` to remove the gate
      --clear-repo-whitelist
          Unset the repo whitelist (no gate — every repo allowed)
      --workspace <WORKSPACE>
          Workspace slug to configure. Defaults to the bootstrapped `default` workspace
  -h, --help
          Print help
```

#### `ainb hangar workspace show`

Show the workspace's current config

```console
$ ainb hangar workspace show --help
Show the workspace's current config

Usage: ainb hangar workspace show [OPTIONS]

Options:
      --format <format>        Output format [default: text] [possible values: text, json, csv, markdown]
      --workspace <WORKSPACE>  Workspace slug to show. Defaults to the bootstrapped `default` workspace
  -h, --help                   Print help
```

### `ainb hangar logs`

Read the daemon's structured logs

```console
$ ainb hangar logs --help
Read the daemon's structured logs

Usage: ainb hangar logs [OPTIONS] <COMMAND>

Commands:
  tail  Pretty-print recent log events; `--follow` streams live
  help  Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

#### `ainb hangar logs tail`

Pretty-print recent log events; `--follow` streams live

```console
$ ainb hangar logs tail --help
Pretty-print recent log events; `--follow` streams live

Usage: ainb hangar logs tail [OPTIONS]

Options:
  -f, --follow           Stream new events live as the daemon writes them (poll-append loop)
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
      --lines <LINES>    Print the last N events and exit (the bounded tail window) [default: 200]
      --level <LEVEL>    Only show events at or above this level (`trace`/`debug`/`info`/`warn`/`error`)
      --no-follow        Print + exit even when `--follow` is set (bounded mode for tests/CI)
  -h, --help             Print help
```

## `ainb rtk`

RTK (Rust Token Killer): compress CLI output in Claude Code via PreToolUse hook

```console
$ ainb rtk --help
RTK (Rust Token Killer): compress CLI output in Claude Code via PreToolUse hook

Usage: ainb rtk [OPTIONS] <COMMAND>

Commands:
  status     Show RTK install state, hook wiring, and total tokens saved
  install    Install rtk (brew install rtk) and wire the Claude Code PreToolUse hook (rtk init -g)
  uninstall  Remove the Claude Code hook from ~/.claude/settings.json (rtk init -g --uninstall). Leaves the rtk binary installed.
  help       Print this message or the help of the given subcommand(s)

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help

EXAMPLES:
  ainb rtk status      Install state + total tokens saved
  ainb rtk install     Install rtk + wire the Claude Code PreToolUse hook
  ainb rtk uninstall   Remove the hook (keeps the rtk binary)
```

### `ainb rtk status`

Show RTK install state, hook wiring, and total tokens saved

```console
$ ainb rtk status --help
Show RTK install state, hook wiring, and total tokens saved

Usage: ainb rtk status [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb rtk install`

Install rtk (brew install rtk) and wire the Claude Code PreToolUse hook (rtk init -g)

```console
$ ainb rtk install --help
Install rtk (brew install rtk) and wire the Claude Code PreToolUse hook (rtk init -g)

Usage: ainb rtk install [OPTIONS]

Options:
      --codex            Also wire Codex AGENTS.md prompt injection (rtk init -g --codex). Best-effort; weaker than the Claude Code hook path.
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

### `ainb rtk uninstall`

Remove the Claude Code hook from ~/.claude/settings.json (rtk init -g --uninstall). Leaves the rtk binary installed.

```console
$ ainb rtk uninstall --help
Remove the Claude Code hook from ~/.claude/settings.json (rtk init -g --uninstall). Leaves the rtk binary installed.

Usage: ainb rtk uninstall [OPTIONS]

Options:
      --format <format>  Output format [default: text] [possible values: text, json, csv, markdown]
  -h, --help             Print help
```

## Skill manager

The skill-manager commands (`skill`, `source`, `search`, `migrate`) are
intercepted before clap and routed to the unit manager; they are documented in
their own section under [Skill manager](../skill-manager/guide) and surfaced in
`ainb --help` under "SKILL MANAGER". Run `ainb skill --help` for the full verb
list.

## Hidden / daemon commands

A few commands are hidden from `ainb --help` because they are internal
daemon/hook entrypoints rather than everyday verbs. Run `<cmd> --help` for each:

- `ainb notifyd <run|stop|install|uninstall|status|list>` — the ainb-hooks
  notification daemon. `ainb notifyd list [--format json]` reads persisted
  notifications headlessly; the TUI Inbox is the interactive view.
- `ainb statusline` — legacy Claude Code statusline alias (prefer
  `ainb claudecode statusline`).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Runtime error (the command ran but failed). |
| `2` | Usage error (bad flags/args), or a required plugin/tool is not installed. |

## Scripting recipes

```bash
# List running sessions as JSON and pull workspace names
ainb list --running --format json | jq -r '.[].workspace'

# Spawn a session in an isolated worktree with an initial prompt
ainb run --repo . --worktree -p "fix the failing tests"

# Machine-readable diff of the working tree (no TUI)
ainb diff-review --format json | jq '.total_insertions, .total_deletions'

# Find orphaned sessions and clean them up
ainb recover list --format json && ainb recover cleanup

# Search the knowledge base headlessly
ainb learnings search "redis connection pooling" --format json
```
