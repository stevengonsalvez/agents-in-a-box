# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-05-05
### Added
- Merge pull request #52 from stevengonsalvez/feat/codeburn
- Merge pull request #53 from stevengonsalvez/fix/tmux-hang
- Merge pull request #59 from stevengonsalvez/feat/usage-sqlite-cache
- Merge pull request #60 from stevengonsalvez/feat/session-status-filter
- Merge pull request #61 from stevengonsalvez/feat/usage-filter-ux-crossfilter
- Merge pull request #62 from stevengonsalvez/feat/usage-zoom-and-dates
- Merge pull request #63 from stevengonsalvez/feat/usage-branch-attribution
- Merge pull request #66 from stevengonsalvez/feat/reflect-existing-skill-routing
- Merge pull request #69 from stevengonsalvez/feat/usage-utc-and-analyze-turns
- **ainb-tui**: --month/--quarter/--last-n-days/--ytd CLI flags
- **ainb-tui**: --project/--model/--activity/--session CLI flags
- **ainb-tui**: UsagePeriod variants for 90d/YTD/Month/Quarter
- **ainb-tui**: add Skills browser screen
- **ainb-tui**: add UsageFilters struct + filter_usage_data helper
- **ainb-tui**: add rusqlite + blake3 + bincode deps
- **ainb-tui**: add session stop/resume audit hooks
- **ainb-tui**: add stable ProviderCall.id from (path, offset)
- **ainb-tui**: aggregate per-branch usage rows on UsageData
- **ainb-tui**: ainb usage cache (clear|info) subcommand
- **ainb-tui**: attach git branch to ProviderCall via Claude JSONL
- **ainb-tui**: bind Tab/Enter/Esc/C to cross-filter pivot
- **ainb-tui**: branch chip on UsageFilters + --branch CLI flag
- **ainb-tui**: cache-bypass force-refresh via Shift+R and --no-cache
- **ainb-tui**: commit focused row as exclude chip via X
- **ainb-tui**: cross-filter dashboard pivot (focus, chips, filtered data)
- **ainb-tui**: cycle filter to hide stopped sessions
- **ainb-tui**: distinguish include vs exclude in pop-chip notification
- **ainb-tui**: labelled period+provider strip in burndown header
- **ainb-tui**: log stale/unknown blob_format on cache miss
- **ainb-tui**: per-file fingerprint with blake3 suffix hash
- **ainb-tui**: period strip swap d/Custom for m/q/4/5/a/D mappings
- **ainb-tui**: pretty_project_name renders <repo>:<branch>
- **ainb-tui**: scaffold usage_cache module with sqlite schema
- **ainb-tui**: show [R] force refresh in usage help bar
- **ainb-tui**: soft-stop and resume actions for stuck sessions
- **ainb-tui**: support claude --resume in tmux launcher
- **ainb-tui**: wire usage_cache into claude/codex parsers
- **ainb-tui**: z toggles fullscreen panel zoom with all rows + extra cols
- **ainb-tui/skills**: surface parse failures via notification
- **bootstrap**: add --verify integrity check
- **bootstrap**: add catalog-only flag to skip agent-skills install
- **bootstrap**: add externalSkillsSubpath for per-tool external skill nesting
- **bootstrap**: add multi-subpath install for bundled skill repos
- **bootstrap**: install learnings CLI from ai-coder-rules
- **bootstrap**: install reflect-kb + per-harness adapters
- **bootstrap**: prune orphan files; document CLI/content split
- **bootstrap**: ship statusline.sh as a claude-code-4.5 tool-specific file
- **bootstrap**: support subpath for agent-skills with non-root SKILL.md
- **cli**: full CLI feature parity with TUI (15 commands) (#32)
- **deps**: add fireworks-tech-graph as primary technical diagram skill
- **deps**: enable multi-subpath install for ui-ux-pro-max and stitch-skills
- **deps**: extend ui-ux-pro-max and notebooklm to hermes-agent and nanoclaw
- **hooks**: TTS opt-in gate via ~/.claude/.tts-on sentinel (#45)
- **hooks**: add context-aware tts announcements
- **reflect**: Claude Code adapter
- **reflect**: Codex CLI adapter
- **reflect**: GitHub Copilot adapter
- **reflect**: add Copilot provider for cross-tool memory discovery
- **reflect**: add migrate_v2.py for legacy v2 state import
- **reflect**: add reflect:ingest sub-skill, separate from consolidate
- **reflect**: enterprise rewrite with SQLite, TOML config, multi-tool providers
- **reflect**: hybrid lex+vec retrieval — fuse qmd BM25 with graphrag
- **reflect**: port learning_template.md asset with provenance fields
- **reflect**: restore reverted lifecycle state in SQLite schema
- **reflect**: route signals to existing skills before falling through to memory
- **reflect**: v3.1.0 — add /reflect:recall + SessionStart auto-retrieval
- **reflect**: v3.2 SQLite state manager foundation
- **reflect**: wire recall preamble into tier-1+2 skills + sandbox tests
- **release**: build Windows x64 target and emit .zip artifacts
- **scoop**: add Scoop bucket manifest + auto-update job
- **toolkit/skills**: add git-history-surgery skill
- add CodeBurn usage parsing foundation
- add Langfuse observability integration (default off)
- add usage burndown analytics

### Fixed
- Merge pull request #51 from stevengonsalvez/fix/windows
- Merge pull request #56 from stevengonsalvez/fix/dashboard-design
- Merge pull request #57 from stevengonsalvez/fix/dashboard-polish
- Merge pull request #58 from stevengonsalvez/fix/dead-worktree-bogus-workspace
- Merge pull request #64 from stevengonsalvez/fix/help-bar-overflow
- Merge pull request #71 from stevengonsalvez/fix/usage-tui-cleanups
- **ainb-tui**: VACUUM after Cache::clear to reclaim disk space
- **ainb-tui**: add filters field to integration test query
- **ainb-tui**: align BlobFormat discriminant with bumped V1 constant
- **ainb-tui**: broaden period chip activity to LastNDays(7|30)
- **ainb-tui**: clamp step_period_back at unfiltered call-set extent
- **ainb-tui**: classify same-size+different-suffix as FullReparse
- **ainb-tui**: clear oldest_call_day on force-refresh
- **ainb-tui**: derive workspace_name from source repo for flat worktrees
- **ainb-tui**: distinguish 0m from <1m in session duration column
- **ainb-tui**: drop crossterm Release key events on Windows
- **ainb-tui**: drop j/k nav from sessions help bar
- **ainb-tui**: expose test_support to bin compile under cfg(test)
- **ainb-tui**: gate pretty_project_name branch width at >= 2 chars
- **ainb-tui**: handle bracketed paste in new-session input fields
- **ainb-tui**: honour force-refresh when cache clear fails
- **ainb-tui**: make last_day_of_month return Option for invalid input
- **ainb-tui**: move usage parsing off event thread
- **ainb-tui**: polish burndown panel review findings
- **ainb-tui**: preserve zoom search query when re-entering search mode
- **ainb-tui**: qualify session filter with owning project chip
- **ainb-tui**: recover from poisoned usage_cache mutex
- **ainb-tui**: recover user_message attribution on append-from-cache path
- **ainb-tui**: refuse step_period_back when usage data not yet loaded
- **ainb-tui**: reject non-UTF8 paths at cache write time
- **ainb-tui**: render branch chip in cross-filter strip
- **ainb-tui**: roll back end_offset on append parse I/O error
- **ainb-tui**: route non-ASCII queries through Utf32String in fuzzy_score
- **ainb-tui**: route zoom Esc through state machine when search active
- **ainb-tui**: stop dead worktrees fabricating phantom workspaces
- **ainb-tui**: truncate_string char-boundary safe slicing
- **ainb-tui**: truthful refresh notification + preserve cache on panic
- **ainb-tui**: width-aware burndown panels with gradient bars
- **ainb-tui/skills**: compute body offset safely on CRLF files
- **ainb-tui/skills**: drop dead scroll_offset field
- **ainb-tui/skills**: quote-aware tools parser
- **ainb-tui/skills**: skip indented map/JSON blocks in frontmatter
- **ainb-tui/skills**: treat hyphen as word boundary in association match
- **bootstrap**: skip agent-skills without repo to prevent clone failures
- **bootstrap**: skip catalog-only npx-skills, use non-interactive install
- **deps**: modernize vercel-labs skill install commands to non-interactive
- **external-deps**: update reflect entry to v3 plugin path
- **homebrew**: move Formula to repo root for tap discovery
- **learnings**: qmd update before qmd embed in add()
- **reflect**: add sidecar validator + inline schema (closes #41)
- **reflect**: address critical review findings
- **reflect**: address review majors + minors
- **reflect**: apply v3.2 review findings + extend tests
- **reflect**: close 3 integrity gaps (LOW + MEDIUM)
- **reflect**: close self-improvement loop — capture → index → recall
- **reflect**: harden recall against parser and runtime edge cases
- **reflect**: make auto-reflect actually capture transcripts via queue + drain
- **reflect**: refuse to overwrite hand-written SKILL.md siblings
- **reflect**: rename status sub-skill to avoid collision with generic /status
- **reflect**: substitute HOME_TOOL_DIR placeholder at adapter install time
- **reflect**: update marketplace.json to point to v3 plugin
- **release**: chain scoop job after homebrew to avoid push race
- **release**: standardize binary name on ainb across publishing pipeline
- align usage dashboard design
- correct usage analytics projections
- correct usage dashboard projections

### Documentation
- Merge pull request #55 from stevengonsalvez/docs/git-surgery-squash-recipe
- **ainb-tui**: CHANGELOG entries for the PR-E cleanup pass
- **ainb-tui**: CHANGELOG entry for V3 cache blob format
- **ainb-tui**: TODO for render_zoom_* table extraction
- **ainb-tui**: TODO marker for analyze_turns precompute
- **ainb-tui**: TODOs for DateTime<Utc> migration and rayon parallelism
- **ainb-tui**: TODOs for UsageViewState zoom collapse and aggregate_calls accumulator extraction
- **ainb-tui**: clarify aggregate_calls_with_analysis fallback semantics
- **ainb-tui**: correct quarter_bounds clamp behaviour comment
- **ainb-tui**: explain why UsageFilterChip::label stays a manual match
- **ainb-tui**: refresh parse_claude_source_append doc-comment
- **ainb-tui**: warn about bincode layout stability for ProviderCall
- **assets**: add 7 TUI screenshots for README showcase
- **cli**: add comprehensive CLI reference and link from README
- **hooks**: add utilities/hooks README with TTS toggle, Langfuse, sync notes
- **plans**: add CLI full-integration plan
- **readme**: add dedicated CLI section with command overview
- **readme**: add usage analytics hero below the dashboard
- **readme**: document Scoop install + correct Homebrew tap
- **readme**: expand feature highlights with multi-provider + analytics
- **readme**: rename ainb section to "Terminal UI + CLI"
- **readme**: replace broken demo.gif with live dashboard hero
- **readme**: replace broken screenshot block with 6-panel showcase
- **readme**: surface Homebrew + Scoop install paths
- **reflect**: add handover + architecture diagram
- **reflect**: document closed-loop auto-drain (replaces stale dashboard section)
- **reflect**: document split PreCompact + SessionStart drain in snippet
- **reflect**: full architecture reference with mermaid diagrams
- **reflect**: note closed-loop drain TODO in codex/copilot adapters
- **skill/git-history-surgery**: recipe for swapping squash-merge to merge commit
- **sync-learnings**: add settings.json + statusline.sh drift checks
- **toolkit**: sync CLAUDE.md commit hygiene rules from user-level
- expand burndown reporting scope
- expand codeburn cli parity scope
- plan codeburn burndown usage tab
- rewrite toolkit README and fix bootstrap script name

### Other
- Merge pull request #72 from stevengonsalvez/chore/release-v1
- **ainb-tui**: bump usage cache blob format to V3
- **ainb-tui**: bump version to 1.0.0 and fix repository URL
- **ainb-tui**: refresh accumulator-trait TODO rationale
- **ainb-tui/skills**: drop unused search state helpers
- **bootstrap**: delete stale global-learnings-template
- **ci**: remove stale duplicate ainb-tui workflow
- **claude**: default bootstrap instructions to caveman (#48)
- **deps**: list cocoon architecture-diagram as catalog-only alternative
- **homebrew**: update formula to v0.5.5-beta1
- **reflect**: archive v1 monolith to toolkit/archive/reflect-v1
- **skills**: document caveman external dependency (#47)
- add caveman default to agent instructions (#49)
- clean generated planning artifacts
- ignore beads runtime files
- remove redundant hermes-agent installs, rely on external_dirs
- sync mobile-e2e-mcp and posthog-replay-analysis skills
- **ainb-tui**: apply cross-filter chips before aggregate on CLI path
- **ainb-tui**: hoist Pattern::parse out of apply_zoom_filter inner loop
- **ainb-tui**: precompute analyze_turns once on the unfiltered set
- **ainb-tui**: precompute top_projects_for_model index in aggregate_calls
- **ainb-tui**: widen SUFFIX_HASH_BYTES from 4 KiB to 64 KiB
- **ainb-tui**: centralise truncate_with_ellipsis in widgets
- **ainb-tui**: co-locate period helpers in models::usage
- **ainb-tui**: collapse StoredRow and LoadedRow into CacheRow
- **ainb-tui**: convert SessionUsage timestamps to DateTime<Utc>
- **ainb-tui**: convert period date ranges to Utc internals
- **ainb-tui**: drop dead parse_usage() shim
- **ainb-tui**: drop free-text include/exclude/clear filter prompts
- **ainb-tui**: drop two redundant doc comments and add BranchUsage TODO
- **ainb-tui**: extract add_bucket / bump map micro-helpers
- **ainb-tui**: extract lock_conn() helper for poisoned-mutex recovery
- **ainb-tui**: extract merge_oldest_call_day helper with test
- **ainb-tui**: extract sort_by_bucket_desc helper
- **ainb-tui**: introduce ProviderCall::recorded_branch accessor
- **ainb-tui**: rename BLOB_FORMAT_BINCODE_V1 to BLOB_FORMAT_BINCODE_CURRENT
- **ainb-tui**: rename BlobFormat::Bincode variant to BincodeV2
- **ainb-tui**: rename const_expected_len to expected_layout_len
- **ainb-tui**: render usage timestamps in local time at the boundary
- **ainb-tui**: split stale-vs-unknown blob format lookup
- **ainb-tui**: store ProviderCall.timestamp as DateTime<Utc>
- **ainb-tui**: unify step_period_back / forward into one helper
- **ainb-tui**: use file.by_ref().take() in recover_user_message_before
- **ainb-tui/sidebar**: derive layout constraints dynamically
- **ainb-tui/skills**: pre-lowercase scanner data once
- **reflect**: apply /simplify review fixes
- **reflect**: extract AdapterBase to remove ~80% adapter duplication

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
