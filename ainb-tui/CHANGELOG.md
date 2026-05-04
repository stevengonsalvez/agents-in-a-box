# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Changed
- **BREAKING (ainb-tui usage cache)**: bumped the cache blob format to
  V3 (`BLOB_FORMAT_BINCODE_CURRENT = 3`). Caches built under V1
  (pre-branch) or V2 (Local-timestamp) are recognised as stale and
  skipped on first run after upgrade — the affected files are
  re-parsed from the underlying JSONL transparently. No user action
  required, but the first analytics scan after the bump will be
  slower than usual while the cache rebuilds. The bump folds two
  layout changes:
  - `ProviderCall.timestamp` migrated from `DateTime<Local>` to
    `DateTime<Utc>` so cached blobs are timezone-independent
    (cache vs full-reparse no longer drifts after a timezone move
    or DST transition). Display is still local — render sites
    convert via `.with_timezone(&Local)` at the boundary.
  - `ProviderCall.id: u64` added (stable hash of `path:offset`) so
    `analyze_turns` results can be precomputed once on the
    unfiltered call set; chip-pivot re-aggregates in
    `filter_usage_data` now skip the per-session timeline rewalk.
- **BREAKING (ainb-tui CLI)**: `--include` no longer treats `--project` as
  an alias. Users must migrate `--project foo` filters that relied on
  substring matching to `--include foo` (substring match) or keep
  `--project foo` for exact-match. The split makes intent explicit:
  `--include` is the substring-match flag, `--project` is the exact
  cross-filter chip equivalent of clicking a project in the burndown.

### Added
- **ainb-tui**: branch attribution panel data — `BranchUsage` rows on
  `UsageData` aggregate per-`gitBranch` token totals (rendering wired
  in a follow-on; data is already available via the cache).
- **ainb-tui**: `recover_user_message_before` recovers `user_message`
  attribution on cache-hit append paths so cached and full-reparse
  rows agree turn-for-turn.
- **ainb-tui**: `model_project_counts` precomputed index on `UsageData`
  removes the O(N·M) per-render scan for "top projects per model".

### Fixed
- **ainb-tui**: cache `clear` now `VACUUM`s so the on-disk db shrinks.
- **ainb-tui**: cache write rejects non-UTF8 paths instead of silent
  lossy collisions.
- **ainb-tui**: append parser rolls back `end_offset` on I/O errors,
  preventing silent data loss on the next scan.
- **ainb-tui**: fuzzy-search routes non-ASCII queries through
  `Utf32String` so unicode queries match unicode haystacks.
- **ainb-tui**: `last_day_of_month` returns `Option` for invalid input
  instead of silently producing the 28th.
- **ainb-tui**: session-duration column distinguishes `0m` from `<1m`.

## [0.5.5-beta1] - 2026-04-14
### Added
- **ainb-tui**: favorite remote repositories instead of local paths
- **ainb-tui**: open shell directly from repo picker with $ key
- **ainb-tui**: persist agent_type in session metadata and detect from tmux
- **ainb-tui**: show agent type icon in session list
- **bootstrap**: add hermes-agent and nanoclaw tool configs
- **bootstrap**: copy plugin skills to skills dir and enhance sync-learnings
- **bootstrap**: manifest-driven agent-skills with DRY git-clone installation
- **bootstrap**: namespace hermes-agent skills under toolkit/ category
- **copilot**: add GitHub Copilot CLI as first-class agent
- **create-rule**: generate setup-external.sh for codex and copilot home installs
- **crypto-research**: add markdown.new/r.jina.ai web page fetching guidance
- **gemini**: support native sub-agents and tool-translated agents
- **global-learnings**: add file-lock concurrency guard for GraphRAG
- **global-learnings**: add learnings visualize command
- **learnings**: auto-generate entity sidecars in add and reindex
- **onboarding**: add GitHub Copilot CLI to dependency checker and generalize auth step
- **plugins**: add /plugins add subcommand and universal skill lifecycle flow
- **reflect**: add --ingest-memories for project memory archival
- **reflect**: expand knowledge signal detection patterns
- **reflect**: rework as plugin with colon-namespaced sub-skills
- **research**: add r.jina.ai as fallback for webpage markdown conversion
- **skill**: add CLAUDE.md sync and reverse template interpolation to sync-learnings
- **skill**: add test-driven-development skill
- **skill**: add tmux-based coding-agent skill
- **skill**: add token-usage skill for CLI usage analytics
- **skills**: add Google Stitch design-to-code skills
- **skills**: add argument-hint to skills that take arguments
- **skills**: add caveman token-compression skill to external dependencies
- **skills**: add media-processing skill (FFmpeg + ImageMagick)
- **skills**: add notebooklm agent-skill for NotebookLM integration
- **skills**: add prompt injection guardrails and mandatory WebFetch converters
- **skills**: add scrapling skill and research fallback for antibot bypass
- **skills**: track all untracked external skills in dependencies manifest
- **toolkit**: add skills, hooks, and config from everything-claude-code research
- **tui**: add bulk session recovery and periodic snapshots
- **tui**: add multi-select and bulk delete in recovery screen
- **tui**: add usage analytics screen with daily/weekly/project views
- **tui**: multi-select recovery resume, provider selector, R shortcut
- add flake.nix exposing skills, agents, and toolkit as Nix packages (#38)
- add gemini to toolkit installer & remove unused ts_check hook
- add showcase README, Rust CI pipeline, and cargo-deny config
- steering protocol + multi-tool fallback for tmux/spawn skills (#39)

### Fixed
- **ainb-tui**: correct Copilot CLI configuration from actual --help output
- **ainb-tui**: use --yolo flag for Copilot skip-permissions
- **ainb-tui**: use brand-accurate icons for Claude and Codex sessions
- **research**: revert to markdown.new - confirmed it works as URL-to-markdown converter
- **research**: use correct Jina.ai Reader URL for webpage fetching
- **rules**: ban tmux kill-server and wildcard tmux kill commands
- **skill**: add backtick-quoted path matching to reverse interpolation
- **skill**: show all projects without truncation in token-usage
- **skill**: token-usage always outputs markdown tables directly
- **skills**: connect reflect output to global learnings search
- **skills**: notebooklm applies to all tools, not just claude
- **skills**: replace hardcoded ~/.claude paths with template placeholders
- **swarm**: increase post-ready delay and add tmux prompt verification
- **sync-learnings**: generalize description to cover codex and copilot, update architecture comment
- **tui**: add 3s timeout to Docker availability checks
- **tui**: force preview refresh on session/workspace switch
- **tui**: preserve agent_type during session recovery
- **tui**: prevent workspace header from scrolling off-screen
- enforce template interpolation in sync-learnings skill

### Documentation
- **nanoclaw**: add OpenClaw→NanoClaw migration guide + manifest fix
- **nanoclaw**: move migration guide to the nanoclaw fork
- add knowledge notes from reflect session
- add knowledge system architecture documentation
- comprehensive knowledge and memory system documentation

### Other
- **deps**: add OpenAI Codex plugin to external dependencies manifest
- **homebrew**: update formula to v0.5.4-beta1
- **manifest**: sync extension manifest with installed state
- **nanoclaw**: point at public fork main branch
- **toolkit**: add test-bootstrap-parity.sh for regression testing
- remove plans/ and research/ output dirs from tracking
- sync expect-test skill with replay publishing and evidence extraction
- sync learnings to packages
- sync tmux_protection safety rule to CLAUDE.md source
- untrack pycache and runtime log debris
- **tui**: throttle tmux preview updates to fix UI sluggishness
- **copilot**: switch from .github/copilot-instructions.md to AGENTS.md
- **deps**: update MCP/mcporter sections, remove stale reflect-learning
- **skills**: update skill metadata, reflect internals, and test scaffolding
- **toolkit**: single CLAUDE.md source of truth with symlinks + fix reflect paths


## [0.5.4-beta1] - 2026-03-04
### Added
- **release**: add Intel Mac (x86_64-apple-darwin) build target

### Other
- **homebrew**: update formula to v0.5.3-beta1


## [0.5.3-beta1] - 2026-03-04
### Added
- **ainb-tui**: add Favorites source choice in new session
- **ainb-tui**: add SelectFavorite step to new session flow
- **ainb-tui**: add display name renaming for SSH sessions
- **ainb-tui**: add repository favorites store
- **ainb-tui**: add star workspace from sessions screen
- **ainb-tui**: add tmux config auto-install in onboarding
- **ainb-tui**: auto-trust worktrees in Claude Code
- **ainb-tui**: improve SSH session UX with dedicated source option and display section
- **config**: add tmux.conf with Claude Code optimizations
- **knowledge**: add distributed knowledge capture system
- **knowledge**: implement global learnings GraphRAG system
- **marketplace**: add Claude plugin marketplace with reflect-learning
- **research**: integrate markdown.new for token-efficient web fetching
- **settings**: upgrade default model to claude-opus-4-6
- **skills**: add interview skill for plan specifications
- **skills**: add nano-banana-pro image generation skill
- **skills**: add oracle skill for multi-model code review
- **toolkit**: adopt GSD patterns — model routing, .planning/, checkpoints, waves
- **toolkit**: unify commands into skills architecture
- **utilities**: add openclaw-agents usage tracking hook
- persist SSH session display names across TUI restarts

### Fixed
- **ainb-tui**: add placeholder text for SSH Host input field
- **ainb-tui**: populate tmux_sessions HashMap for session previews
- **ainb-tui**: preserve SSH session display_name across reloads
- **homebrew**: add ainb symlink for expected command name
- **hooks**: prevent nested Claude Code session errors
- **nested-session**: disable action-summary claude invocation + guard swarm-lib
- **plan**: remove redundant learnings search step
- **settings**: add compaction threshold override and fix PreCompact hook
- **settings**: add permissionMode to bypass workspace trust prompt
- **settings**: use ccstatusline package instead of local script
- **swarm**: add orphaned process cleanup to prevent CPU hogs
- **swarm**: allow spawning agents from within Claude Code session
- **tui**: decouple local repo discovery from Docker dependency
- compact SSH config modal layout to prevent field cutoff
- enable delete (d) key for SSH sessions
- make tmux capture filter patterns more specific

### Documentation
- **learning**: add tmux global env as root cause of nested session error
- **oracle**: rewrite skill with working mode recommendations
- add learning for nested Claude session error
- add skills migration plan and agent teams research

### Other
- **beads**: migrate config to metadata.json
- **homebrew**: update formula to v0.5.2-beta1
- **toolkit**: add missing swarm commands and utils
- **toolkit**: sync learnings from ~/.claude
- **toolkit**: sync learnings to packages
- **toolkit**: sync learnings, restructure utils, cleanup orchestration
- **toolkit**: update catalog with new skills and plugins
- **workflows**: remove deprecated m-workflow orchestration
- sync agent learnings to packages
- **toolkit**: unify CLAUDE.md and AGENTS.md into single source


## [0.5.2-beta1] - 2026-01-29
### Added
- **ainb-tui**: add session recovery tile to home screen
- **ainb-tui**: extend session recovery with orphaned worktree detection
- **clawdhub**: add ClawdHub skills directory and installer
- **multi-agent**: add session persistence and recovery for agent worktrees
- **reflect**: consolidate command + agent into portable skill
- **toolkit**: add interactive package selection for project installs
- **toolkit**: add unified plugin/skill tracking manifest

### Fixed
- **ainb-tui**: add Recovery to sidebar navigation
- **ainb-tui**: auto-cleanup orphaned branches when creating worktrees
- **ainb-tui**: enable navigation to Other tmux sessions
- **ainb-tui**: handle existing suffixed branches gracefully
- **ainb-tui**: handle transcrypt filter in suffixed branch worktree creation
- **clawdhub**: flatten reflect skill structure for web upload
- **skills**: use absolute paths for browser-tools binary
- **tui**: handle branch worktree collision with auto-suffix

### Documentation
- **sync-learnings**: enhance with bidirectional sync and session learnings

### Other
- **deps**: add open-prose to external plugins manifest
- **homebrew**: update formula to v0.5.1-beta1
- sync claude commands and agents
- **toolkit**: make packages/ canonical source for all tools


## [0.5.1-beta1] - 2026-01-23
### Added
- **agents**: retrofit reflect learnings to test agents
- **commands**: add /sync-learnings command
- **tui**: add uncommitted files warning on session deletion
- **tui**: refine new-session branch/mode UX

### Fixed
- **codex**: remove prompt args
- **tui**: auto-rename worktrees on collision

### Other
- **homebrew**: update formula to v0.0.0-beta1
- **tui**: move inline regex compilations to lazy_static


## [0.0.0-beta1] - 2026-01-20
### Added
- **agents**: add Gemini 3 preview models
- **agents**: enable Codex and Gemini CLI providers
- **cli**: add provider-specific skip permissions flags for Codex and Gemini
- **providers**: add multi-provider CLI support for Codex and Gemini (#30)
- **toolkit**: add reflect self-improvement system
- **tui**: add Shift+scroll for horizontal pan in logs viewer
- **tui**: add logs viewer improvements
- add codex bootstrap and prompt generation

### Fixed
- **agents**: update Codex and Gemini models to latest versions
- **agents**: update Codex models to match actual CLI options
- **git**: resolve worktree creation failure for branches with slashes
- **git**: show clear error when worktree already exists for branch
- **tui**: resolve ghost/duplicate UI elements on resize
- sanitize codex prompts to avoid arg parsing
- standardize tool paths and tmux-monitor frontmatter

### Documentation
- add FAQ with tmux tips and troubleshooting
- add claude-code packages migration issue stub

### Other
- **changelog**: remove duplicate 0.5.0 entry
- **homebrew**: update formula to v0.5.0


## [0.5.0] - 2026-01-16
### Added
- **audit**: add audit trail for user-initiated mutations
- **cleanup**: add orphaned tmux shell cleanup to 'x' key
- **git**: add checkout existing remote branch option
- **git**: add read-through cache for repository discovery
- **new-session**: add fuzzy filter and scroll to branch selection
- **onboarding**: add tmux anti-flicker config and setup check
- **session**: add session metadata persistence for reliable discovery
- **tmux**: improve session naming with folder prefix
- **tui**: add F2 rename for Other tmux sessions

### Fixed
- **config**: handle boolean defaults for old config files
- **git**: handle transcrypt smudge filter in checkout existing branch
- **git**: handle transcrypt/smudge filters in worktree creation
- **git**: skip branch input step for CheckoutExisting mode
- **git**: use -B flag for existing branch worktree checkout
- **session**: wait for shell ready before starting claude in tmux
- **session-loader**: don't mark orphaned worktrees as Boss sessions
- **sessions**: use canonicalized path comparison on startup
- **tmux**: add reattach-to-user-namespace for macOS services
- **tmux**: enable clipboard integration for shell sessions (#28)
- **tmux**: enable macOS audio/clipboard access in tmux sessions (#26)
- **tui**: auto-select newly created sessions to prevent list clipping
- **ui**: make branch checkout mode toggle more prominent

### Documentation
- **deps**: clarify reattach-to-user-namespace description
- **tmux**: add clipboard integration config and setup guide

### Other
- **audit**: simplify to use standard tracing log

## [0.4.0] - 2026-01-11
### Fixed
- **git**: credential helper support + commits tab in git view (#27)

### Other
- **homebrew**: update formula to v0.3.0


## [0.3.0] - 2026-01-10
### Added
- **changelog**: add in-app changelog viewer and manual release pipeline
- **startup**: async workspace loading with timeout

### Fixed
- **release**: correct SHA256 extraction path and add formula values

### Other
- **homebrew**: update formula to v0.2.1


### Added
- **startup**: Async workspace loading with 10s timeout to prevent hanging on slow Docker
- **changelog**: In-app changelog viewer (press `v` on home screen)

## [0.2.1] - 2026-01-10
### Added
- **release**: add manual release pipeline with changelog generation
- **tui**: add Open in Editor feature and improve Config navigation
- **tui**: add popup-based config editing for all settings

### Fixed
- **git-view**: handle directories in diff view
- **release**: create CHANGELOG.md if it doesn't exist
- **release**: update root workflow with manual trigger pipeline
- **tui**: add 'o open' to bottom menu bar legend
- **tui**: fix quick commit dialog bugs and styling
- **tui**: remove redundant 'search' from menu bar
- **tui**: return to previous view when exiting Git view
- address PR #24 review comments

### Documentation
- **tui**: remove duplicate UI directive from project CLAUDE.md

### Other
- **config**: move Editor to its own category
- **editors**: centralize editor logic with cross-platform detection
- **tui**: expand menu bar to 2 lines with 'o editor' label

## [0.2.0] - 2026-01-10

### Added
- **Open in Editor**: Press `o` to open sessions in your preferred editor (VS Code, Cursor, Zed, etc.)
- **Popup-based Config Editing**: All config settings now use intuitive popup dialogs
- **Onboarding Wizard**: First-run experience with dependency checking and setup
- **Remote Repository Support**: Clone and work with remote git repositories
- **Centralized Editor Module**: Cross-platform editor detection using `which` crate
- **JSONL Log Persistence**: Session logs saved with history viewer
- **Tmux Preview**: Preview tmux sessions before attaching
- **Workspace Shell**: Quick shell access with `$` shortcut
- **Delete Confirmation**: Confirmation dialogs for destructive actions
- **Model Selection**: Choose Claude model for sessions
- **Homebrew Formula**: Easy installation via `brew install ainb`
- **Install Script**: One-liner installation for macOS and Linux

### Changed
- Editor moved to separate config category (not under Appearance)
- Menu bar expanded to 2 lines for better visibility
- Home screen refreshed with sidebar navigation and mascot
- Config screen navigation improved (Up/Down within pane, Left/Right to switch)

### Fixed
- Git view directory handling in diff view
- Quick commit dialog bugs and styling
- Navigation flow with HomeScreen as hub
- Shell sessions preserved across workspace refresh
- Stuck navigation issues resolved

## [0.1.0] - 2025-12-01

### Added
- Initial release of agents-in-a-box TUI
- Docker container management for Claude Code agents
- Session lifecycle management (create, attach, restart, delete)
- Git integration with worktree isolation
- Live log streaming from containers
- Claude API integration for chat
- Configuration management with TOML persistence
- Help overlay with keyboard shortcuts
- Agent selection (Claude models)
- Workspace scanning for git directories

### Technical
- Built with Rust + ratatui for terminal UI
- Tokio async runtime
- Bollard for Docker API
- git2 for Git operations
- portable-pty for tmux/PTY integration
