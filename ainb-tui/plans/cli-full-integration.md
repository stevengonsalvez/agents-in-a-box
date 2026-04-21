# CLI Full Integration Plan

## Overview

Expose all major `ainb` TUI capabilities as CLI commands so users can script and automate workflows without the TUI. The CLI already has 7 commands (`run`, `list`, `logs`, `attach`, `status`, `kill`, `auth`). This plan adds 7 new command groups and enhances the existing `run` command for full multi-provider support.

## Current State Analysis

**Existing CLI** (`src/cli/`): 7 commands using shared modules (`interactive/session_manager`, `tmux/`, `git/`, `config/`, `models/`). All commands follow a consistent pattern: clap derive args → async handler → shared module calls → text/json output.

**Shared modules already CLI-ready** (no extraction needed):
- `config/favorites_store.rs` → `FavoritesStore` with full CRUD + YAML persistence
- `config/presets.rs` → `PresetManager` with TOML persistence + defaults
- `config/ssh_display_names.rs` → `SshDisplayNameStore` with JSON persistence
- `config/onboarding.rs` → `OnboardingConfig` with completion tracking
- `config/mod.rs` → `AppConfig` with multi-level merging, `CliProvider` with command/flag builders
- `interactive/session_manager.rs` → `SessionStore` with discovery
- `tmux/process_detection.rs` → `ClaudeProcessDetector`
- `git/worktree_manager.rs` → `WorktreeManager` with create/remove

**Key discovery:** The `CliProvider` enum already has `command()`, `api_key_env_var()`, `skip_permissions_flag()` for Claude/Codex/Gemini/Copilot — but `run.rs:build_claude_command()` hardcodes `"claude"` and ignores `--tool`.

### Key Discoveries:
- `run.rs:259` hardcodes `claude` in `build_claude_command()` — never checks `args.tool`
- `SessionMetadata` (session_manager.rs) has `agent_type` field but `run.rs:90-96` never sets it
- `FavoritesStore` stores at `~/.agents-in-a-box/favorites.yaml` with usage tracking
- `PresetManager` stores at `~/.agents-in-a-box/presets/` as individual TOML files
- Session recovery in TUI scans `~/.claude/agents/` for orphaned JSON + `~/.agents-in-a-box/worktrees/by-session/` for broken symlinks
- `CliProvider::skip_permissions_flag()` already handles per-provider differences

## Desired End State

Running `ainb --help` shows all subcommands. Every operation possible in the TUI is also possible via CLI flags. All commands support `--format json` for scripting. The `run` command works with all providers (Claude, Codex, Gemini, Copilot).

**Verification:**
```bash
ainb --help                     # Shows all subcommands
ainb run --tool codex --repo .  # Spawns codex session
ainb recover list               # Shows orphaned sessions
ainb config show                # Displays config
ainb git worktrees              # Lists worktrees
ainb favorites list             # Shows favorites
ainb init --check               # Prereq check
cargo test                      # All tests pass
cargo clippy -- -D warnings     # No lint warnings
```

## What We're NOT Doing

- Docker/container session creation via CLI (TUI-only for now — complex multi-step with image building)
- Interactive multi-step session wizard in CLI (use flags instead)
- SSH session creation via CLI (complex config, use TUI)
- TUI-specific features (mascot, animations, syntax highlighting in terminal)
- MCP server management via CLI

---

## Phase 1: Multi-Provider Run Command
<!-- wave: 1 | depends_on: [] | files: [src/cli/run.rs, src/cli/mod.rs] -->

### Overview
Fix `run` command to actually use `--tool` flag and `CliProvider` abstractions. Currently hardcodes `claude`.

### Changes Required:

#### 1. Fix `build_claude_command` → `build_agent_command`
**File**: `src/cli/run.rs`

Replace `build_claude_command` with provider-aware command building:

```rust
use crate::config::CliProvider;
use crate::models::session::SessionAgentType;

/// Build the CLI command for the selected provider
fn build_agent_command(args: &RunArgs, model: Option<ClaudeModel>) -> String {
    let provider = CliProvider::from_str(&args.tool);
    let mut cmd_parts = vec![provider.command().to_string()];

    // Add model flag (Claude-only)
    if provider == CliProvider::Claude {
        if let Some(m) = model {
            cmd_parts.push("--model".to_string());
            cmd_parts.push(m.cli_value().to_string());
        }
    }

    // Add skip-permissions flag (provider-specific)
    if args.dangerously_skip_permissions {
        cmd_parts.push(provider.skip_permissions_flag().to_string());
    }

    // Add initial prompt for providers that support it
    if let Some(ref prompt) = args.prompt {
        match provider {
            CliProvider::Claude => {
                cmd_parts.push("--prompt".to_string());
                cmd_parts.push(format!("\"{}\"", prompt.replace('"', "\\\"")));
            }
            CliProvider::Codex => {
                cmd_parts.push(format!("\"{}\"", prompt.replace('"', "\\\"")));
            }
            CliProvider::Gemini => {
                // Gemini takes prompt as positional
                cmd_parts.push(format!("\"{}\"", prompt.replace('"', "\\\"")));
            }
            CliProvider::Copilot => {
                // Copilot uses stdin prompt
            }
        }
    }

    cmd_parts.join(" ")
}
```

#### 2. Set `agent_type` in `SessionMetadata`
**File**: `src/cli/run.rs`

The `SessionMetadata` struct needs agent_type set from the tool flag. Update the metadata creation block (~line 90):

```rust
let agent_type = match CliProvider::from_str(&args.tool) {
    CliProvider::Claude => SessionAgentType::Claude,
    CliProvider::Codex => SessionAgentType::Codex,
    CliProvider::Gemini => SessionAgentType::Gemini,
    CliProvider::Copilot => SessionAgentType::Copilot,
};

let metadata = SessionMetadata {
    session_id,
    tmux_session_name: tmux_name.clone(),
    worktree_path: work_dir.clone(),
    workspace_name: workspace_name.clone(),
    created_at: Utc::now(),
    agent_type,
};
```

#### 3. Validate provider CLI is installed
**File**: `src/cli/run.rs`

Add a check before session creation:

```rust
fn validate_provider_installed(provider: &CliProvider) -> Result<()> {
    let cmd = provider.command();
    if which::which(cmd).is_err() {
        anyhow::bail!(
            "{} CLI ('{}') not found in PATH. Install it first.\n\
             See: {}",
            provider.display_name(),
            cmd,
            match provider {
                CliProvider::Claude => "https://docs.anthropic.com/en/docs/claude-code",
                CliProvider::Codex => "https://github.com/openai/codex",
                CliProvider::Gemini => "https://github.com/google-gemini/gemini-cli",
                CliProvider::Copilot => "https://githubnext.com/projects/copilot-cli",
            }
        );
    }
    Ok(())
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes (update existing `test_build_claude_command` tests)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] New tests: `test_build_codex_command`, `test_build_gemini_command`, `test_build_copilot_command`

#### Manual Verification:
- [ ] `ainb run --tool codex --repo . --worktree` spawns a codex session
- [ ] `ainb run --tool gemini --repo .` spawns a gemini session
- [ ] `ainb list` shows correct agent type for each session
- [ ] `ainb run --tool nonexistent` gives helpful error

---

## Phase 2: Session Recovery Command
<!-- wave: 1 | depends_on: [] | files: [src/cli/recover.rs] -->

### Overview
Add `ainb recover` command to detect, list, resume, and clean up orphaned sessions.

### Changes Required:

#### 1. Create `src/cli/recover.rs`

```rust
// Subcommands:
//   ainb recover list              - Show orphaned sessions + worktrees
//   ainb recover resume <session>  - Re-attach to orphaned session
//   ainb recover cleanup           - Remove all orphaned sessions
//   ainb recover cleanup <session> - Remove specific orphaned session

#[derive(Subcommand)]
pub enum RecoverCommands {
    /// List orphaned sessions and worktrees
    List,
    /// Resume an orphaned session
    Resume { session: String },
    /// Clean up orphaned sessions and worktrees
    Cleanup {
        /// Specific session to clean up (all if omitted)
        session: Option<String>,
        /// Skip confirmation
        #[arg(long, short)]
        force: bool,
    },
}
```

**Core logic** (extract from TUI's `session_recovery.rs` patterns):

1. **Scan `~/.claude/agents/`** for JSON files → parse metadata → check tmux alive
2. **Scan `~/.agents-in-a-box/worktrees/by-session/`** for broken symlinks
3. **Cross-reference with `SessionStore`** to find metadata mismatches
4. **Resume**: Re-register in SessionStore if tmux still alive
5. **Cleanup**: Kill tmux session + remove worktree + remove metadata file

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod recover;

// In Commands enum:
/// Recover orphaned or crashed sessions
Recover {
    #[command(subcommand)]
    command: recover::RecoverCommands,
},
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes with new unit tests for orphan detection
- [ ] `cargo clippy -- -D warnings` passes

#### Manual Verification:
- [ ] `ainb recover list` shows orphaned sessions (if any exist)
- [ ] `ainb recover list --format json` outputs parseable JSON
- [ ] `ainb recover cleanup --force` removes stale entries

---

## Phase 3: Configuration Management Command
<!-- wave: 1 | depends_on: [] | files: [src/cli/config_cmd.rs] -->

### Overview
Add `ainb config` command for viewing and modifying configuration.

### Changes Required:

#### 1. Create `src/cli/config_cmd.rs`

```rust
// Subcommands:
//   ainb config show                    - Display full config (merged)
//   ainb config get <key>               - Get specific value (dot-notation)
//   ainb config set <key> <value>       - Set value in user config
//   ainb config reset                   - Reset to defaults
//   ainb config path                    - Show config file locations
//   ainb config edit                    - Open config in $EDITOR

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Display current configuration
    Show,
    /// Get a specific config value
    Get { key: String },
    /// Set a config value
    Set { key: String, value: String },
    /// Reset configuration to defaults
    Reset {
        #[arg(long, short)]
        force: bool,
    },
    /// Show config file locations
    Path,
    /// Open config in editor
    Edit,
}
```

**Key implementation detail**: Use `AppConfig::load()` for reading and dot-notation keys mapped to TOML paths. E.g., `ainb config get authentication.default_model` → reads `[authentication] default_model`.

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod config_cmd;

// In Commands enum:
/// Manage configuration
Config {
    #[command(subcommand)]
    command: config_cmd::ConfigCommands,
},
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes
- [ ] Tests for `config show` output format, `config get` dot-notation parsing

#### Manual Verification:
- [ ] `ainb config show` displays merged config
- [ ] `ainb config get authentication.default_model` → `"sonnet"`
- [ ] `ainb config set authentication.default_model opus` persists
- [ ] `ainb config path` shows all 3 config locations with existence markers

---

## Phase 4: Git Worktree Management
<!-- wave: 2 | depends_on: [1] | files: [src/cli/git_cmd.rs] -->

### Overview
Add `ainb git` command for worktree inspection and cleanup.

### Changes Required:

#### 1. Create `src/cli/git_cmd.rs`

```rust
// Subcommands:
//   ainb git worktrees                 - List all managed worktrees
//   ainb git cleanup                   - Remove orphaned worktrees
//   ainb git status <session>          - Git status for session's worktree

#[derive(Subcommand)]
pub enum GitCommands {
    /// List all managed worktrees
    Worktrees,
    /// Clean up orphaned worktrees (no active session)
    Cleanup {
        #[arg(long, short)]
        force: bool,
        /// Dry run - show what would be cleaned
        #[arg(long)]
        dry_run: bool,
    },
    /// Show git status for a session's worktree
    Status {
        /// Session ID or name
        session: String,
    },
}
```

**Uses**: `WorktreeManager`, `SessionStore`, `find_session()` from util.

Worktree listing: Scan `~/.agents-in-a-box/worktrees/` → cross-ref with SessionStore → show status (active/orphaned).

Cleanup: Find worktrees not referenced by any session → `WorktreeManager::remove_worktree()`.

Status: `find_session(id)` → `git2::Repository::open(worktree_path)` → diff stats.

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod git_cmd;

// In Commands enum:
/// Git worktree operations
Git {
    #[command(subcommand)]
    command: git_cmd::GitCommands,
},
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes
- [ ] Unit tests for worktree listing and orphan detection

#### Manual Verification:
- [ ] `ainb git worktrees` lists worktrees with session association
- [ ] `ainb git worktrees --format json` outputs JSON
- [ ] `ainb git cleanup --dry-run` shows what would be removed
- [ ] `ainb git status <session>` shows files changed

---

## Phase 5: Favorites Management
<!-- wave: 2 | depends_on: [] | files: [src/cli/favorites.rs] -->

### Overview
Add `ainb favorites` command wrapping the existing `FavoritesStore`.

### Changes Required:

#### 1. Create `src/cli/favorites.rs`

```rust
// Subcommands:
//   ainb favorites list                 - Show all favorites
//   ainb favorites add <source>         - Add a favorite
//   ainb favorites remove <alias>       - Remove a favorite
//   ainb favorites use <alias>          - Record usage (for sorting)

#[derive(Subcommand)]
pub enum FavoritesCommands {
    /// List all favorites sorted by usage
    List,
    /// Add a new favorite
    Add {
        /// Repository URL, path, or GitHub shorthand
        source: String,
        /// Friendly alias
        #[arg(long)]
        alias: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Tags (comma-separated)
        #[arg(long)]
        tags: Option<String>,
    },
    /// Remove a favorite by alias
    Remove { alias: String },
    /// Record usage of a favorite (updates sort order)
    Use { alias: String },
}
```

**Uses**: `FavoritesStore::load()`, `.add()`, `.remove()`, `.record_use()`, `.save()`.

Source type detection: Check if input is HTTPS URL, SSH URL, GitHub shorthand (`owner/repo`), or local path. `FavoritesStore` already has `SourceType` enum.

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod favorites;

// In Commands enum:
/// Manage favorite repositories
Favorites {
    #[command(subcommand)]
    command: favorites::FavoritesCommands,
},
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes
- [ ] Tests for source type detection, add/remove round-trip

#### Manual Verification:
- [ ] `ainb favorites add owner/repo --alias myrepo` adds entry
- [ ] `ainb favorites list` shows entry with usage count
- [ ] `ainb favorites remove myrepo` removes it
- [ ] `ainb favorites list --format json` outputs JSON

---

## Phase 6: Init / Setup Command
<!-- wave: 2 | depends_on: [3] | files: [src/cli/init.rs] -->

### Overview
Add `ainb init` command for first-time setup and prerequisite checking.

### Changes Required:

#### 1. Create `src/cli/init.rs`

```rust
// Subcommands:
//   ainb init                          - Run first-time setup
//   ainb init --check                  - Check prerequisites only
//   ainb init --reset                  - Factory reset (with confirmation)
//   ainb init --status                 - Show onboarding completion status

#[derive(clap::Args)]
pub struct InitArgs {
    /// Only check prerequisites, don't set up
    #[arg(long)]
    pub check: bool,
    /// Factory reset (removes all config, sessions, worktrees)
    #[arg(long)]
    pub reset: bool,
    /// Show setup completion status
    #[arg(long)]
    pub status: bool,
}
```

**Prerequisites to check** (via `which` crate):
- `tmux` installed
- `git` installed
- `claude` (or configured provider CLI) installed
- `docker` installed (optional, warn if missing)
- Config directory exists and is writable
- Authentication configured

**Uses**: `OnboardingConfig`, `AppConfig`, `which::which()`.

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod init;

// In Commands enum:
/// First-time setup and prerequisite checking
Init(init::InitArgs),
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes
- [ ] Tests for prerequisite detection logic

#### Manual Verification:
- [ ] `ainb init --check` shows green/red status for each prerequisite
- [ ] `ainb init --status` shows onboarding completion
- [ ] `ainb init` creates default config if missing

---

## Phase 7: Presets Management
<!-- wave: 3 | depends_on: [3] | files: [src/cli/presets.rs] -->

### Overview
Add `ainb presets` command wrapping the existing `PresetManager`.

### Changes Required:

#### 1. Create `src/cli/presets.rs`

```rust
// Subcommands:
//   ainb presets list                   - Show all presets (built-in + custom)
//   ainb presets show <name>            - Show preset details
//   ainb presets create <name>          - Create a custom preset interactively
//   ainb presets delete <name>          - Delete a custom preset
//   ainb presets apply <name>           - Apply preset to current repo (.agents-box/preset.toml)

#[derive(Subcommand)]
pub enum PresetsCommands {
    /// List all available presets
    List,
    /// Show preset details
    Show { name: String },
    /// Create a new preset
    Create {
        name: String,
        /// Agent provider
        #[arg(long)]
        provider: Option<String>,
        /// Model to use
        #[arg(long)]
        model: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a custom preset
    Delete { name: String },
    /// Apply preset to current repository
    Apply { name: String },
}
```

**Uses**: `PresetManager::load_all()`, `.get()`, `.save()`, `.delete()`. Apply writes to `.agents-box/preset.toml` in current directory.

#### 2. Add to `src/cli/mod.rs`

```rust
pub mod presets;

// In Commands enum:
/// Manage session presets
Presets {
    #[command(subcommand)]
    command: presets::PresetsCommands,
},
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` passes
- [ ] Tests for preset CRUD operations

#### Manual Verification:
- [ ] `ainb presets list` shows built-in presets (rust-backend, typescript-frontend, fast-iteration)
- [ ] `ainb presets show rust-backend` displays full preset
- [ ] `ainb presets apply rust-backend` creates `.agents-box/preset.toml`

---

## Dependency Analysis

```
Wave 1 (parallel):
  Phase 1: Multi-Provider Run  [src/cli/run.rs, src/cli/mod.rs]
  Phase 2: Session Recovery     [src/cli/recover.rs]
  Phase 3: Config Management    [src/cli/config_cmd.rs]

  Note: Phase 1 touches mod.rs. Phases 2 & 3 also add to mod.rs.
  Resolution: Phase 1 modifies existing entries in mod.rs.
  Phases 2, 3 only ADD new entries (no conflicts if done carefully).
  For safety: Phase 1 first, then 2 & 3 can parallel.

Wave 2 (after Phase 1):
  Phase 4: Git Worktrees        [src/cli/git_cmd.rs]     depends on Phase 1 (mod.rs stable)
  Phase 5: Favorites            [src/cli/favorites.rs]   no deps
  Phase 6: Init/Setup           [src/cli/init.rs]        depends on Phase 3 (config must exist)

Wave 3 (after Wave 2):
  Phase 7: Presets              [src/cli/presets.rs]      depends on Phase 3 (config patterns)
```

## Testing Strategy

### Unit Tests (per phase):
- Command building for each provider (Phase 1)
- Orphan detection and classification (Phase 2)
- Config dot-notation key parsing (Phase 3)
- Worktree orphan detection (Phase 4)
- Source type detection for favorites (Phase 5)
- Prerequisite detection (Phase 6)
- Preset CRUD round-trips (Phase 7)

### Behavioral Tests (extend `tests/behavioral/`):
- `multi_provider_sessions.rs` - Create sessions with each provider, verify metadata
- `recovery_workflow.rs` - Create session → kill tmux → recover list → cleanup
- `config_persistence.rs` - Set → get → reset cycle
- `worktree_lifecycle.rs` - (existing, extend with cleanup)
- `favorites_persistence.rs` - Add → list → use → remove cycle

### Integration / Manual Testing:
1. Full workflow: `ainb init --check` → `ainb run --tool codex` → `ainb list` → `ainb logs` → `ainb kill`
2. Recovery: kill a tmux session manually → `ainb recover list` → `ainb recover cleanup`
3. Config: `ainb config set authentication.cli_provider codex` → `ainb run` uses codex

## Performance Considerations

- `recover list` scans filesystem — could be slow with many orphaned sessions. Add a cache or limit scan depth.
- `favorites list` sorts by usage — O(n log n) which is fine for typical favorite counts (<100).
- `config show` merges 3 config files — already fast via TOML parsing.

## Migration Notes

- No data migration needed — all persistence formats unchanged.
- Existing sessions created by TUI will be visible to new CLI commands.
- Existing sessions created by current CLI will work with new recovery/cleanup.
- `SessionMetadata` may need `agent_type` field added if not already present (it is — but `run.rs` doesn't set it currently, Phase 1 fixes this).

## References

- CLI module: `ainb-tui/src/cli/mod.rs`
- Run command: `ainb-tui/src/cli/run.rs`
- Session store: `ainb-tui/src/interactive/session_manager.rs`
- Config system: `ainb-tui/src/config/mod.rs`
- Provider enum: `ainb-tui/src/config/mod.rs:79-151` (CliProvider)
- Favorites: `ainb-tui/src/config/favorites_store.rs`
- Presets: `ainb-tui/src/config/presets.rs`
- Onboarding: `ainb-tui/src/config/onboarding.rs`
- Worktree mgr: `ainb-tui/src/git/worktree_manager.rs`
- Process detection: `ainb-tui/src/tmux/process_detection.rs`
- TUI recovery component: `ainb-tui/src/components/session_recovery.rs`
