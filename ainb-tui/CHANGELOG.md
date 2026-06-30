# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.10.0] - 2026-06-30
### Added
- Merge pull request #360 from stevengonsalvez/f/notifyd-reap
- Merge pull request #368 from stevengonsalvez/worktree-add-source-legend
- Merge pull request #369 from stevengonsalvez/f/raycast-multi-host-ssh-paste
- Merge pull request #370 from stevengonsalvez/o/rtk-statusline-pill
- feat!: remove the destructive `ainb migrate` subcommand
- **abtop**: link the docsite page from the empty/ready/missing states
- **notifyd**: add `notifyd reap` CLI verb
- **notifyd**: add process enumerator + orphan classifier
- **notifyd**: reap orphan daemons, sparing the live owner
- **otel**: surface the docsite link in otel setup (CLI + onboarding)
- **raycast**: add Set SSH Host command to switch paste target
- **raycast**: add shared host registry for cc-paste scripts
- **raycast**: target a selectable host from clipboard SSH paste
- **setup**: bootstrap node/cargo/uv toolchains in generated script
- **setup**: check gh auth status in the GitHub setup section
- **setup**: install ainb-owned tools by default in generated script
- **skill-manager**: auto-gh add-source input + backend legend
- **skill-manager**: confirm before removing a unit with [r]
- **statusline**: add RTK pill to Claude Code statusline
- **tui**: mark Shift chords with a glyph in the menu bar
- **tui**: surface notifyd daemons and orphans in Daemons overlay
- **witr**: link the docsite page from the empty/missing states

### Fixed
- Merge pull request #358 from stevengonsalvez/f/notifyd-orphan-view-and-spawn-hardening
- Merge pull request #362 from stevengonsalvez/worktree-picker-clone-error-cta
- Merge pull request #364 from stevengonsalvez/o/debug-errors
- Merge pull request #366 from stevengonsalvez/f/codex-hooks-parity
- **adapters**: refuse to uninstall protected user state
- **new-session**: pin auth-modal CTA so long gh errors can't hide it
- **new-session**: surface exact git-auth error in dismissible modal
- **notifyd**: classify by live socket probe and exclude CLI calls
- **notifyd**: harden lazy-spawn against orphan daemons
- **notifyd**: reap spares the real socket holder, not the pid-file owner
- **otel**: disambiguate local Alloy vs remote Grafana Cloud endpoint
- **setup**: detect Claude plugins via installed_plugins.json registry
- **setup**: install reflect plugin via HTTPS marketplace, retarget to ainb-reflect-memory
- **setup**: point the toolkit install at additive `ainb skill sync`
- **skill-manager**: reset unit cursor when the search filter changes
- **tui**: bind capital S to star/unstar in session list
- **tui**: cap notifyd overlay rows to height with overflow pointer
- **tui**: correct stale help bindings + spell out Shift chords
- **tui**: drop dead 'k kill' hint from session preview footer
- **tui**: point orphan cleanup hint at `ainb notifyd reap`
- **tui**: survive transient EINTR from terminal I/O on flaky links
- align Codex notify hook parity

### Documentation
- **otel**: add Grafana dashboard examples + concrete endpoint
- **plugins**: use HTTPS marketplace-add to avoid SSH default
- group burndown/abtop/witr/otel under an Observability section
- rewrite install as additive; drop the migrate --clean recipe

### Other
- pin reflect plugin to v5.0.4
- **notifyd**: close nc liveness probe with -N
- **tui**: drop the Boss/container option from new-session
- **tui**: hide docker/boss/container from the setup surface

### Removed
- **cli**: remove the `ainb migrate` subcommand. Its `--clean` mode wiped each
  tool's entire install root (e.g. all of `~/.claude`) before re-syncing from
  the manifest, which destroyed user state (`CLAUDE.md`, `settings.json`,
  `projects/` history, `memory/`, custom agents). Installs are additive via
  `ainb skill install` / `ainb skill sync`; discover + adopt now lives in the
  Skill Manager TUI.

### Changed
- **skill manager**: the `[r]` remove key now requires a confirm — the first
  press arms, a second press on the same unit uninstalls. Moving the cursor or
  leaving the screen cancels.

### Security
- **adapters**: `uninstall` now hard-refuses any path that resolves into
  protected user state (`projects/`, `memory/`, `todos/`, `settings.json`,
  `CLAUDE.md`, …) or escapes the install root, guarding against a corrupt
  lockfile deleting non-unit files.

## [1.9.6] - 2026-06-27
### Added
- **onboarding**: auto-copy the G installer command to the clipboard


## [1.9.5] - 2026-06-27
### Added
- **doctor**: point at the dependency catalog + installer
- **illustration:popa**: intricate sketchnote style with text integrity (#355)
- **onboarding**: G key generates an agent-specific install script
- **setup**: generate an agent-specific install script (ainb init --script)

### Documentation
- **cli**: regenerate CLI reference for ainb init --script/--agent

### Other
- **illustration**: remove Alex/Sport Head, Popa-only plugin (#354)


## [1.9.4] - 2026-06-24
### Added
- **onboarding**: show success/failure feedback for the I tmux-config install


## [1.9.3] - 2026-06-24
### Added
- **onboarding**: add Homebrew to the catalog


## [1.9.2] - 2026-06-24
### Added
- **burndown**: mouse support on the usage screen + a switch-tab legend

### Fixed
- **burndown**: clamp wheel scroll to the tab-aware row_count, redraw on movement
- **burndown**: clip tab titles to inner width so they can't overflow


## [1.9.1] - 2026-06-24
### Added
- **onboarding**: correctness audit fixes, reflect plugin, Codex, checkbox UI


## [1.9.0] - 2026-06-23
### Added
- Merge pull request #325 from stevengonsalvez/chore/otel-dashboards
- Merge pull request #333 from stevengonsalvez/feat/zellij-config
- **ainb-tui**: add zellij config alongside tmux.conf
- **burndown**: make the outer usage tabs keyboard-reachable via [ / ]
- **illustration:alex**: switch Alex to sketchnote style (#331)
- **init**: drive ainb init from the shared setup catalog
- **onboarding**: render TUI dependency step from the setup catalog
- **onboarding**: two-column layout, per-dep why, Codex parity
- **otel**: expand Claude Code dashboard to full telemetry coverage
- **setup**: provisioner engine with consent policy
- **setup**: shared topic/dependency catalog + detection engine
- **site**: add a light-mode palette
- **site**: add explainer-style matrix + callout components
- **site**: add option-card component for the systems comparison

### Fixed
- Merge pull request #328 from stevengonsalvez/f/clean-menu
- **onboarding**: address PR #348 review
- **site**: impeccable polish on reflect-memory components
- **tui**: even sidebar spacing and aligned shortcut hints

### Documentation
- Merge pull request #330 from stevengonsalvez/docs/reflect-memory-polish
- Merge pull request #332 from stevengonsalvez/docs/impeccable-polish
- Merge pull request #338 from stevengonsalvez/docs/eight-systems-cards
- **assets**: add reflect session-timeline diagram
- **burndown**: correct the headroom_tokens_saved source comment
- **cli**: regenerate CLI reference for ainb init --yes
- **illustration**: add Popa edition of the 22-frame aib mural (#342)
- **illustration**: add Sport Head trademark notice, move Popa to top, drop credits (#347)
- **illustration**: add example gallery to plugin README (#335)
- **illustration**: credit original + English port (#336)
- **illustration**: densify sparse Popa aib frames (#343)
- **illustration**: extend aib mural to 22 frames (#340)
- **illustration**: featured 10-frame agents-in-a-box series (#339)
- **illustration**: fix garbled text on Popa inbox frame (#345)
- **illustration**: restore dense Popa inbox frame with clean text (#346)
- **illustration**: retitle Popa witr frame to 'Why Is This Running' (#344)
- **illustration**: trademarked Sport Head examples, drop old agents-box ones (#349)
- **reflect**: add 'why build, not adopt' comparison page
- **reflect**: convert problem-and-fit memory-product table to scroll matrix
- **reflect**: convert recall feature tables to callout cards
- **reflect**: fit construct loop diagram in a box
- **reflect**: fit problem-and-fit ASCII diagrams in boxes
- **reflect**: fit recall pipeline diagram in a box
- **reflect**: reframe problem-and-fit around context engineering
- **reflect**: render the 8 systems as option cards + facet legend
- **reflect**: scroll-matrix + scored heatmap + LOCOMO on comparison
- **site**: link the comparison page in the Reflect Memory sidebar
- **tui**: document Headroom/RTK token optimisation + the Daemons overlay
- add impeccable design context (.impeccable.md)

### Other
- pin reflect plugin to v5.0.3
- **illustration**: rename alex sub-skill to sporthead-alex (#334)


## [1.8.1] - 2026-06-22
### Added
- Merge pull request #327 from stevengonsalvez/feat/mcp-socket
- **mcp-pool**: daemon self-shuts down when idle

### Fixed
- Merge pull request #323 from stevengonsalvez/fix/caveman-stats-hook-path
- **caveman-stats**: move hooks to plugin root so PreCompact/PostToolUse resolve

### Documentation
- Merge pull request #324 from stevengonsalvez/docs/reflect-memory-section
- **mcp-pool**: document pool lifecycle, singleton & self-shutdown
- **overview**: point to the reflect-memory section; trim relocated backend block
- **reflect**: add reflect-memory construct page
- **reflect**: add reflect-memory problem & fit page
- **reflect**: expand recall reference to all 57 ports as tables
- **site**: add Reflect Memory sidebar group + redirect old recall URL

### Other
- pin reflect plugin to v5.0.2


## [1.8.0] - 2026-06-20
### Added
- Merge pull request #144 from stevengonsalvez/feat/skill-manager
- Merge pull request #303 from stevengonsalvez/feat/otel-grafana-onboarding
- Merge pull request #306 from stevengonsalvez/f/reflect-locomo
- Merge pull request #316 from stevengonsalvez/f/ainb-cli
- **adapters-tool**: asymmetric R/W gate — read_root_for defaults to real home
- **ainb**: wire ainb-cli subcommands into the binary entrypoint
- **ainb-cli**: P3 reconciler for class-A + class-C walker outputs
- **ainb-cli**: add `ainb skill check` drift report
- **ainb-cli**: add class-A discovery walker for Claude Code plugin cache
- **ainb-cli**: add migrate --discover / --legacy-yaml / --force flags
- **ainb-cli**: class-C orphan walker for skill-manager v1.1 discovery
- **ainb-cli**: hdt.5 migrate --discover orchestration
- **ainb-cli**: hdt.7 ainb skill promote command — git+gh roundtrip with manifest URI rewrite
- **ainb-skill-core**: hdt.3 schema additions for discovery (shadowed_by, read_only, claude-marketplace kind, marketplace URI)
- **burndown**: token-savings tab (Headroom + RTK + caveman estimate)
- **catalog**: install npx/plugin/mcp entries by running their command
- **catalog**: model install kind; browse npx/plugin/mcp externals
- **cli**: add AinbCuratedCatalogBackend over the release index
- **cli**: add EXAMPLES to headroom + rtk (audit gate) + regen reference
- **cli**: add `ainb notifyd list` to read persisted notifications
- **cli**: add `ainb skill usage` subcommand
- **cli**: add `skill browse --catalog ainb` for the curated shelf
- **cli**: add headless `ainb learnings search <query>`
- **cli**: add headless `ainb witr <target>` process-trace command
- **cli**: ainb skill browse <query> [--json] via injectable CatalogBackend
- **cli**: ainb skill library {list,add,new} over library.yaml
- **cli**: headless `ainb diff-review --format json`
- **cli**: make `ainb --help` agent-friendly for headless use
- **configure**: contextual Headroom usage guide in the new-session filler
- **configure**: explain the Headroom toggle inline
- **configure**: gate the Headroom toggle on availability
- **daemons**: read-only Daemons overlay (MCP pool + Headroom proxy)
- **deps**: register rtk + headroom as token-optimisation external deps
- **events**: AppEvent::GoToSkillManager + SkillManagerBack
- **explain-to-me**: add --gist publish alternate (permanent htmlpreview URL)
- **headroom**: ainb-managed shared proxy daemon + headroom CLI
- **headroom**: idle-reap shared proxy when last session closes
- **headroom**: in-loop proxy watchdog + 'watched' on the Daemons row
- **headroom**: manual mid-session downgrade with H (resume direct)
- **home**: rebind SkillManager nav from uppercase M to lowercase m
- **illustration**: add mascot illustration plugin (alex + popa) (#309)
- **illustration:alex**: restyle Alex to hand-drawn pastel, drop glasses (#314)
- **migrate**: add --upgrade-schema to backfill bootstrap target_layout
- **otel**: add 'ainb otel {setup,status,start}' command
- **otel**: add OpenTelemetry setup module
- **otel**: add optional Telemetry step to onboarding wizard
- **otel**: register alloy dependency under new otel consumer
- **otel**: vendor Grafana Alloy assets for telemetry setup
- **reflect-kb**: add LOCOMO long-term-memory benchmark
- **reflect-kb**: env-gated retrieval-quality knobs (embedder swap, HyDE, recall budget)
- **rtk**: detect + install/uninstall RTK from ainb
- **rtk**: per-session RTK via project-local worktree hook
- **screens**: add SKILL_MANAGER screen id constant
- **screens**: register SkillManagerScreen in the registry
- **scripts**: skill-manager-sandbox.sh up/down manual launcher
- **session**: per-session Headroom proxy opt-in + env injection
- **sidebar**: SidebarItem::SkillManager — discoverable nav entry
- **skill-cli**: ainb skill scan — provenance tree CLI
- **skill-cli**: bidirectional content sync in `ainb skill sync`
- **skill-core**: CatalogBackend trait + CatalogHit + MockCatalogBackend + URL builder
- **skill-core**: YAML-backed own-skill library (library.yaml, no SQLite)
- **skill-core**: add BOOTSTRAP_DEFAULT_MAPPINGS + resolve_pair fallback
- **skill-core**: add DriftDetector with mockable backend
- **skill-core**: add MappingEngine resolve_pair glob→path resolver
- **skill-core**: add SyncEngine apply_to_home (TO_HOME executor)
- **skill-core**: add SyncEngine apply_to_repo (TO_REPO executor)
- **skill-core**: add SyncPlanner (plan_sync) for bidirectional home↔repo sync
- **skill-core**: add curated catalog index types and transforms
- **skill-core**: add optional target_layout schema to SourceEntry
- **skill-core**: add per-unit usage telemetry to lockfile (schema v2)
- **skill-core**: gated sandbox test-fixture for SkillManager
- **skill-core**: per-source advisory lock around apply_to_repo (v12.1.T7)
- **skill-install**: honour SourceEntry.target_layout when computing dst
- **skill-manager**: P5 discovery banner overlay + import/skip flow
- **skill-manager**: P7 [s] keybind flips shadowed_by on conflict pair
- **skill-manager**: [b]rowse catalog modal -> select -> install
- **skill-manager**: [l] own-skill Library view + help-bar entry
- **skill-manager**: [r] remove drops the unit from the manifest + live tripwire
- **skill-manager**: `[s]` routes to SkillManagerSync when no conflict
- **skill-manager**: add drift status column to Units panel
- **skill-manager**: arrow/j-k nav + selection highlight + empty-state hint
- **skill-manager**: background drift poll on screen-enter
- **skill-manager**: live-data binding on SkillsScreenData
- **skill-manager**: provenance matcher + provenance-aware reconcile
- **skill-manager**: provenance-aware discovery import in the TUI
- **skill-manager**: resizable + selectable Sources column with mouse support
- **skill-manager**: wire UsageCache into Detail pane
- **skill-manager**: wire reload_from_disk into GoToSkillManager
- **state**: HomeTile::SkillManager + skill_manager_state
- **statusline**: Headroom routing indicator (Claude + Codex)
- **swarm-lib**: descriptive team_id (`swarm-<branch>-<rand>`) instead of bare epoch
- **tui**: browse the curated catalog in the [b] modal
- **tui**: wire every SkillManager help-bar key
- **usage**: add per-tool invocation detector
- **workspace**: add 7 skill-manager crates
- **xtask**: generate catalog-index from an external ainb-toolkit checkout
- **xtask**: generate enriched curated catalog index

### Fixed
- Merge pull request #301 from stevengonsalvez/ops/ainb
- Merge pull request #315 from stevengonsalvez/fix/docsite-otel-frontmatter
- **ainb-cli**: class-C walker skips ~/.claude/plugins/cache/
- **cli**: don't re-rank the curated catalog; show a kind column
- **cli**: lazy reqwest client + AINB_CATALOG_MOCK_INSTALL_URI for TUI browse
- **cli-docs**: resolve AINB_BIN robustly + regen rtk/headroom EXAMPLES
- **configure**: offer the RTK toggle only for Claude sessions
- **docs**: add required frontmatter title to otel-grafana (unblocks docsite build)
- **headroom**: degrade to direct when the proxy can't come up
- **headroom**: ensure proxy before re-injecting env on session restart
- **headroom**: parse real /stats shape (savings.total_tokens, api_requests)
- **headroom**: serialize proxy spawn against double-spawn + pid clobber
- **headroom**: stop() reports whether it actually stopped a proxy
- **hooks**: isolate uv-run hooks from the cwd project with --no-project
- **just**: build before exec so tui/cli don't break rustup HOME lookup
- **layout**: drop the 'H headroom off' menu-bar hint (width regression)
- **otel**: harden secret handling and filesystem edge cases
- **otel**: read API token without echo + validate endpoint
- **promote**: block argv smuggling in git clone of promote-cache
- **reflect-kb**: harden env-override parsing + embedding-dim getter (review)
- **rtk**: harden the project-hook merge and gate it to Claude
- **session**: atomic SessionStore::save (tmp + rename)
- **skill-core**: block argv smuggling in drift git ls-remote call
- **skill-core**: disable interactive git auth prompts in drift backend
- **skill-core**: strip tool dotdir in apply_to_home/apply_to_repo
- **skill-core+promote**: finish argv-smuggle hardening + sync auth-prompt guard
- **skill-manager**: install/sync outcome-correctness fixes found by recording validation
- **skill-manager**: marketplace sources now surface their plugin skills
- **skill-manager**: offload catalog browse off the tokio runtime + ConflictFlip toast
- **skill-manager**: sidebar nav now triggers discovery banner, same as `m` keybind
- **skill-manager**: stop double-printing the unit path in content-sync plan
- **skills**: repair malformed sentry-cli frontmatter close
- **sync**: block argv smuggling in git push of TO_REPO executor
- **tripwires**: bump poll deadlines for slow debug-binary spawn
- **tripwires**: wait for HomeScreen fully painted before keystroke
- **tui**: disarm command-install confirm on edit-query and catalog toggle
- **usage**: make 'ainb usage savings' reachable and non-hanging

### Documentation
- Merge pull request #307 from stevengonsalvez/f/reflect-locomo
- Merge pull request #308 from stevengonsalvez/f/reflect-locomo
- Merge pull request #312 from stevengonsalvez/docs/refresh-reflect-extraction
- Merge pull request #318 from stevengonsalvez/docs/reflect-postgres-topology
- Merge pull request #319 from stevengonsalvez/f/ainb-cli
- Merge pull request #320 from stevengonsalvez/docs/cli-nav
- Merge pull request #321 from stevengonsalvez/docs/table-style
- Merge pull request #322 from stevengonsalvez/docs/table-width
- chore(just): skill-manager sandbox + TUI/CLI launcher recipes
- **CONTRIBUTING**: point setup at the ainb binary
- **README**: point at skill-manager v1.1 discovery + promote refs
- **README**: retire bootstrap.js commands, point at ainb
- **burndown**: document why caveman savings stays a blanket estimate
- **claude-md**: add lead-with-recommendation instruction
- **cli**: generate the full CLI reference from the binary
- **cli**: regenerate CLI reference after merge (headroom + rtk)
- **config**: document [skills].catalog_release pin
- **knowledge**: reflect Postgres backend + topology section + 3-harness short-version
- **knowledge**: refresh overview for reflect extraction + SVG architecture diagram
- **otel**: add Grafana Cloud telemetry setup guide
- **readme**: point CLI link at the generated multi-hierarchy reference
- **reflect**: correct 4-cat mean to 77.5, reframe as preliminary, widen leaderboard
- **reflect**: embed both-judge LOCOMO positioning chart in READMEs
- **reflect**: surface LOCOMO benchmark results at the top of the READMEs
- **reflection**: point cross-tool deployment at `ainb skill install`
- **site**: add Repositories reference page + ainb-toolkit links
- **site**: clean up markdown table styling (padding, frame, zebra)
- **site**: give the CLI reference its own top-level sidebar heading + top-nav link
- **site**: tables hug content width (fix blank right region)
- **skill-manager**: add Starlight frontmatter to reference pages
- **skill-manager**: add v1.1 ainb skill promote reference
- **skill-manager**: add v1.1 discovery flow reference
- **skill-manager**: add v1.2 references for usage, sync, and check
- **skill-manager**: document [b] browse + skills.sh API key/env
- **skill-manager**: document the sandbox safety-guard test
- **skill-manager**: one-page sandbox-testing how-to
- **skill-manager**: re-record 6 journeys to prove real outcomes + outcomes contract
- **skill-manager**: re-record cli-sync-edit for the de-doubled sync path
- **skill-manager**: tabbed guide page in the docsite
- **skill-manager**: tabbed user/demos/internals guide page
- **skill-manager**: vhs recordings of every TUI + CLI journey
- **toolkit**: add ainb migration notice at top of README
- repoint toolkit references to the external ainb-toolkit repo
- fix(docs): add required frontmatter title to otel-grafana (unblocks docsite build)

### Other
- Merge pull request #298 from stevengonsalvez/feat/extract-ainb-toolkit
- Merge pull request #311 from stevengonsalvez/chore/extract-reflect-repoint
- **just**: skill-manager sandbox + TUI/CLI launcher recipes
- **marketplace**: point reflect plugin to ainb-reflect-memory@v5.0.0
- **reflect**: extract reflect into its own repo (ainb-reflect-memory)
- **reflect**: sync plugin.json version to 4.1.0
- **skill-cli**: silence empty-format-string clippy lint in run_check header
- **skill-manager**: split apply_discovery_import first-doc paragraph
- **tests**: quarantine pre-existing NewSessionState drift for v12.1 verify
- **toolkit**: delete bootstrap.js + parity scripts (P9 cutover)
- delete toolkit/ from the monorepo (now the ainb-toolkit repo)
- drop unused deps flagged by cargo-machete
- move tmux-ui-tripwire skill to repo root
- pin reflect plugin to v5.0.1
- **cli**: apply code-review polish to the headless commands
- **cli**: make --catalog a ValueEnum to reject typos
- **cli**: tool_dotdir returns String, drop Box::leak fallback
- **skill-core tests**: consume sandbox fixture from sync + drift tests
- **skill-core**: hoist strip_tool_dotdir to mapping module
- **skill-core**: point owned catalog entries at the ainb-toolkit mirror
- **usage**: swap hand-rolled days_from_civil for chrono (v12.1.T6)
- repoint hangar skills-sync + migrate to external ainb-toolkit


## [1.7.7] - 2026-06-17
### Added
- Merge pull request #289 from stevengonsalvez/f/copilot-upgrade
- Merge pull request #297 from stevengonsalvez/feat/mcp-socket
- **mcp-pool**: auto-start the pool when importing into a stopped daemon
- **notifyd**: wire Copilot hooks install/uninstall via ainb-notifyd
- **reflect**: per-repo installer for the SG2 post-commit hook
- **reflect**: wire S8 doc-chunk-learning grouping into the drain
- **scripts**: add Raycast clipboard-image-to-ssh-path command

### Fixed
- chore(reflect): bump to 4.1.0 / reflect-kb 0.2.0 — recall upgrade (57 ports)
- **notifyd**: JSON-escape hook path + update stale Copilot doc comments
- **notifyd**: mention Copilot in TUI install success notification
- **reflect**: S8 grouping links by source, not content_hash (review)
- **reflect**: clear PR #248 LOW/NIT review items (#296)
- **reflect**: clear the PR #248 LOW/NIT review items

### Documentation
- **mcp-pool**: reflect import auto-starting the pool + re-record GIF

### Other
- **reflect**: 4.1.0 release hardening — version bump, SG2 installer, S8 drain wiring (#294)
- **reflect**: bump to 4.1.0 / reflect-kb 0.2.0 — recall upgrade (57 ports)


## [1.7.6] - 2026-06-17
### Added
- Merge pull request #288 from stevengonsalvez/f/copilot-upgrade
- Merge pull request #295 from stevengonsalvez/f/copilot-burndown
- **ainb-hooks**: add copilot notify path (ainb-notifyd --copilot)
- **copilot**: port the rich statusline to the Copilot CLI
- **reflect**: KB export/import for cross-machine snapshots (C5)
- **reflect**: MMR diversity step after rerank (R3)
- **reflect**: add fuzzy Jaccard cache tier before vector search (R9)
- **reflect**: add installed-skills index for fast query matching (R20)
- **reflect**: add pinned editable memory slots (A1)
- **reflect**: add temporal retrieval arm filtered by query date range (R5)
- **reflect**: auto-flag and refresh skills when backing learnings change (R13)
- **reflect**: auto-refreshing per-project conventions doc (O2)
- **reflect**: auto-trigger consolidation when N new learnings land (C2)
- **reflect**: belief revision on ingest with CREATE/UPDATE/DELETE actions (S5)
- **reflect**: bitemporal graph edges — tcommit + tvalid (A2)
- **reflect**: bounded multiplicative rerank boosts (R8)
- **reflect**: branch-aware capture & isolation + behavioral proof (A6)
- **reflect**: capture TodoWrite completions as process learnings (SG7)
- **reflect**: capture permission prompt replies as policy learnings (SG8)
- **reflect**: chunk-hash delta retain dedup + behavioral proof (S7)
- **reflect**: compute per-skill staleness on read (R14)
- **reflect**: consolidated observations layer for persona/conventions (O1)
- **reflect**: copilot adapter reaches reflect hook parity (native drop-in hooks)
- **reflect**: cross-encoder rerank after RRF fusion (R2)
- **reflect**: cross-turn contradiction detection on learning writes (SG1)
- **reflect**: detect agent tool-loops and arm mini-learnings
- **reflect**: document->chunks->learnings grouping persistence (S8)
- **reflect**: enforced 3-layer staged recall workflow (M1)
- **reflect**: extract structured fields at drain (S1)
- **reflect**: first-class persona/preference fields per scope (O3)
- **reflect**: followup-rate recall-quality diagnostic (A4)
- **reflect**: forced-grounding short-circuit on warm skill hit (R11)
- **reflect**: git event capture — commit_links + commits.jsonl, revert demotes session learnings (SG2)
- **reflect**: graph arm, OOD gate, token budget in recall (R1/R7/R4)
- **reflect**: graph maintenance post-delete sweep (C3)
- **reflect**: idle-session sweep with speculative down-rank (SG3)
- **reflect**: knowledge-corpus Q&A — build/prime/query/reprime (M7)
- **reflect**: lifecycle events JSONL + per-event shell hooks (C4)
- **reflect**: make hook scripts harness-aware (camelCase stdin + copilot output envelope)
- **reflect**: move volatile ranking signals into reflect.db sidecar (S9)
- **reflect**: native copilot + codex marketplace plugin manifests
- **reflect**: parse natural-language dates from queries into temporal ranges (R6)
- **reflect**: parse test-runner outcomes from Bash output into memory signals (SG4)
- **reflect**: per-arm calibrated OOD thresholds (R12)
- **reflect**: per-ingest semantic-dedup adjudication (C1)
- **reflect**: per-project sharding in recall + behavioral proof (R15)
- **reflect**: per-row TTL with hourly forget sweep (A3)
- **reflect**: persist zero-result recalls as knowledge-gap signals (SG6)
- **reflect**: pluggable mode system with parent--override inheritance (M4)
- **reflect**: project-affinity multiplicative boost in recall rerank (R16)
- **reflect**: provenance source ids and proof_count on learnings (S4)
- **reflect**: recall-upgrade — 57/57 ports, all behaviorally proven (#248)
- **reflect**: recover typed causal links from stored graph (S2)
- **reflect**: snapshot old learning form to history on update (S6)
- **reflect**: store numeric confidence 0-1 beside display tiers (S3)
- **reflect**: strip private tags at the LLM-prompt boundary
- **reflect**: subscription-quota-aware writer abort via quota store (M3)
- **reflect**: surface token economics on every recall block (M8)
- **reflect**: synthetic no-LLM compression fallback (A5)
- **reflect**: tiered skills-first injection at session start (R10)
- **reflect**: typed causal-link enum in sidecar validator + drain (S2 plugin half)
- **reflect**: verify commit refs in learnings before persistence
- **reflect**: write-validate-retry loop on drain note body (S10)
- **reflect**: writer-output classifier + respawn circuit breaker (M2)
- **reflect-kb**: recall eval harness with hermetic KB and golden queries
- **session-reader**: add GitHub Copilot CLI provider
- fix(reflect): pin trunk branch in R15 proof for A6 branch-shard parity

### Fixed
- **burndown**: unify the two provider controls into one
- **notifyd**: add Copilot arm to render_title so OS notifications capitalize correctly
- **reflect**: add isinstance guards to inline fallback get_* helpers in hook scripts
- **reflect**: apply R14 computed staleness to the inject tier
- **reflect**: clean error on malformed corpus date filter (M7)
- **reflect**: correct copilot plugin.json skills paths + drop premature hooks field
- **reflect**: degrade validate_sidecar as a library, not sys.exit
- **reflect**: pin trunk branch in R15 proof for A6 branch-shard parity
- **reflect**: strip nested <private> spans depth-aware (M6)
- **reflect**: write SG2 commit_captured event to the caller's connection
- **session-reader**: address review on copilot provider

### Documentation
- **reflect**: correct copilot plugin install hook status
- **reflect**: correct stale 'no hooks' claims for codex + copilot
- **reflect**: native plugin install for all three harnesses
- **reflect**: retrieval feature guide — example + counterfactual per feature
- **site**: how recall works — by example (end-to-end + per-feature)

### Other
- **settings**: sync editor prefs + generic OTEL; portable marketplace source
- merge origin/main — resolve install.rs Copilot agent conflict


## [1.7.5] - 2026-06-16
### Fixed
- Merge pull request #285 from stevengonsalvez/feat/mcp-socket
- **mcp-pool**: overlay import targets the user config, drop project variant

### Documentation
- **mcp-pool**: reflect user-config import + re-record GIF


## [1.7.4] - 2026-06-15
### Added
- **session-list**: state colours, blue folders, unboxed selected glyph

### Fixed
- **notifyd**: tolerate copilot + unknown agents in install.json
- **session-list**: 1-cell Nerd Font pause glyph for stopped status


## [1.7.3] - 2026-06-15
### Added
- Merge pull request #282 from stevengonsalvez/feat/statusline-ttl-10min
- Merge pull request #283 from stevengonsalvez/feat/mcp-socket
- **mcp-pool**: import servers from the pool overlay
- **tui**: extend Claude statusline freshness TTL to 10 minutes

### Documentation
- **mcp-pool**: document + record the overlay import action


## [1.7.2] - 2026-06-15
### Added
- Merge pull request #267 from stevengonsalvez/feat/mcp-socket
- Merge pull request #279 from stevengonsalvez/feat/gemini-copilot-agent-pills
- Merge pull request #281 from stevengonsalvez/feat/statusline-slim-quota
- **cli**: add mcp namespace — daemon / proxy / status / stop
- **cli**: mcp import and mcp install --codex/--copilot
- **config**: add [mcp_pool] monitor_refresh_secs
- **config**: add [mcp_pool] section and per-server shared flag
- **config**: read project config from .ainb/ (legacy .agents-box/ kept)
- **mcp-pool**: per-server stop control command
- **mcp-pool**: runtime server registration over the control socket
- **mcp-pool**: session identity + uptime in pool status
- **mcp-pool**: shared MCP server pool — daemon, mux, shim
- **new-session**: restore Gemini and Copilot as agent options
- **run**: auto-import stdio servers from project .mcp.json into the pool
- **run**: wire shared MCP pool into session creation
- **tui**: MCP Pool config category
- **tui**: shared MCP pool observability overlay
- **tui**: slim top bar to a dedicated quota line + abbreviate-then-shed

### Fixed
- **mcp-pool**: address PR review — name validation, atomic writes, backup guard
- **mcp-pool**: move overlay shortcut to p — m collides with Memory tile
- **mcp-pool**: per-server stop is reap-only; chain overlay refresh
- **mcp-pool**: proxy robustness — status ordering, crash recovery, line cap
- **new-session**: harden greyed-agent handling and narrow-terminal Agent row
- **tui**: vanish the unwired-statusline CTA when it can't fit
- **validate**: trust canonical /private/tmp paths, target active tmux window

### Documentation
- Merge pull request #280 from stevengonsalvez/docs/new-session-guide
- **screenshots**: from-scratch MCP pool walkthrough (journey GIF + tape)
- **screenshots**: mcp-pool vhs tape + animated GIF
- **screenshots**: slow the MCP pool walkthrough GIF ~1.4x
- **tui**: add "Starting a new session" guide with wizard walkthrough
- **tui**: add scannable Enable & Use quickstart to MCP pool page
- **tui**: document the MCP pool observability overlay
- **tui**: embed from-scratch walkthrough GIF on the MCP pool page
- **tui**: port the full rich explainer into the MCP pool page
- **tui**: shared MCP pool page with embedded proof GIF
- **tui**: tabbed, per-agent MCP pool guide (mdx)
- cover mcp import/install and .mcp.json auto-import
- document shared MCP pool settings and architecture
- point project config references at .ainb/

### Other
- fix(mcp-pool): move overlay shortcut to p — m collides with Memory tile


## [1.7.1] - 2026-06-15
### Added
- Merge pull request #277 from stevengonsalvez/worktree-codex-statusline
- **bootstrap**: wire caveman hooks and marketplace in settings.json
- **cli**: add ainb codex statusline to pull Codex OAuth quota
- **hooks**: add caveman PostToolUse and PreCompact hooks
- **marketplace**: register caveman-stats plugin in agents-in-a-box marketplace
- **onboarding**: clearer Welcome screen with CTAs and Esc hint
- **plugins**: extract caveman-stats as standalone plugin
- **session-list**: brand-color agent pill, ballot checkbox, drop tmux dot
- **session-list**: use Nerd Font brand logos for agent icons
- **statusline**: add caveman mode badge and savings segment
- **tui**: overlay Codex usage onto the live window reader
- **tui**: pull Codex usage from the live-window watcher + e2e tripwire
- **tui**: render Codex quota (cx5h/cxwk) on the top bar next to Claude

### Fixed
- Merge pull request #276 from stevengonsalvez/onboarding-esc-menu
- **onboarding**: Esc opens the Setup menu instead of cancelling to Home
- **tui**: per-process tmp name for codex cache atomic write

### Documentation
- **readme**: document Homebrew's untrusted-tap gate
- **release**: use cargo install --git one-liner in release notes
- **tmux-ui-tripwire**: add gotcha 15 — AppState::new restores persisted UI prefs
- **tui**: add Codex-on-top-bar proof captures (vhs frames + gif)
- **tui**: document ainb codex statusline + live status bar

### Other
- **tui**: compact provider-grouped statusline, keep reset date/time


## [1.7.0] - 2026-06-12
### Added
- Merge pull request #256 from stevengonsalvez/feat/antv-infographic-skill
- Merge pull request #260 from stevengonsalvez/fix/legend-cleanup
- Merge pull request #263 from stevengonsalvez/feat/tmux-in-pane-2
- Merge pull request #264 from stevengonsalvez/feat/embed-honor-sidebar
- **home**: add Memory tile to the home sidebar menu
- **tmux**: PtyWrapper owns + kills the embed child; panic-hook drains leaked clients
- **tmux**: encode KeyEvents to terminal bytes for the embed PTY
- **tmux**: expand the pane to near-full width while interactive (P4/B7)
- **tmux**: forward mouse events into the embed as SGR sequences
- **tmux**: live EmbedClient — stream tmux attach into vt100 + forward input
- **tmux**: wire interactive embed into the live TUI (i enters, Ctrl+Q releases)
- **tui**: 'B' toggles the sessions sidebar (keyboard twin of the [-]/[+] glyph)
- **tui**: add 'i interactive' hint to the session menu bar
- **tui**: pair the attach keys — Shift+A opens the in-pane embed
- **tui**: the in-pane embed honors the sidebar layout
- **tui**: two-column session legend with mode-aware key dimming
- register antv-infographic external agent-skill

### Fixed
- Merge pull request #261 from stevengonsalvez/fix/memory-panel-exit
- **deps**: cap transitive time below the broken 0.3.48 release
- **learnings**: close the knowledge-base panel on root Esc
- **tests**: share one lock across all REGISTRY-touching PTY tests
- **tmux**: cover modifier chords in the embed key encoder
- **tmux**: enforce locale and tmux socket env for the embed client
- **tmux**: enforce mode-boundary coherence for the interactive embed
- **tmux**: kill the double reflow at embed entry and harden resize ordering
- **tmux**: make the panic-hook registry drain deadlock-proof
- **tmux**: move embed PTY writes off the UI thread
- **tmux**: re-target the embed when entering on a different row
- **tmux**: survive EINTR in the embed reader thread
- **tui**: release the embed on the first input-write failure
- **tui**: saturate the sidebar+border addition in interactive_embed_size
- **tui**: surface embed failures and auto-release as notifications
- **update-externals**: harden antv-infographic flatten loop

### Documentation
- **explain-to-me**: add /infographic-creator sister skill
- **plans**: add TDD plan for in-place tmux pane embed
- **plans**: correct Phase 0 risk with measured cargo check results
- **plans**: expand Phase 0 with tmux-ui-tripwire render-parity gate
- **plans**: lock embed source, focus cue, death + poll decisions
- **plans**: lock scrollback, enter-render, copy-out, footer decisions
- **plans**: mark the TDD plan as a historical record
- **plans**: re-verify embed spec on v1.3.3 after rebase
- **plans**: spec interactive in-place tmux pane embed
- **research**: analyze in-place tmux pane embedding and prior art
- **tui**: add June 2026 performance review
- **tui**: attach guide + spec follow the honor-sidebar behavior
- **tui**: attach guide — full-screen and in-pane flows with recordings
- **tui**: correct the stale re-auth key to 'u'
- **tui**: re-record in-pane attach — embed honors the sidebar layout
- **tui**: record perf fixes shipped on the review

### Other
- Merge pull request #262 from stevengonsalvez/f/perform-review
- **deps**: migrate to ratatui 0.30 (+crossterm 0.29, vt100 0.16, portable-pty 0.9, ansi-to-tui 8)
- **tmux**: post-ship hygiene sweep for the embed feature
- **hangar**: add idle read timeout to daemon RPC connections
- **tui**: add env-gated render-loop instrumentation and micro-benchmarks
- **tui**: eliminate idle redraw burn and per-frame session-list work
- **tui**: poll non-selected session status on a longer cadence


## [1.6.1] - 2026-06-10
### Added
- Merge pull request #214 from stevengonsalvez/feat/learnings-plugin
- Merge pull request #235 from stevengonsalvez/worktree-fleet-tmux-transport-toggle
- Merge pull request #236 from stevengonsalvez/f/graph-memory
- Merge pull request #253 from stevengonsalvez/f/abtop-overlay
- Merge pull request #259 from stevengonsalvez/worktree-ainb-fleet-token-efficiency
- **burndown**: clear scan banner on terminal done progress event
- **config**: render per-plugin manifest config in Settings and persist to config.toml
- **fleet**: add AINB_FLEET_TRANSPORT toggle, default tmux-first
- **fleet**: collapse hangar enrich into one batched agent
- **fleet**: token-efficient enrich — content cache, JSONL ERR fallback, --no-enrich
- **learnings**: deterministic radial layout for the ego map
- **learnings**: ego-subgraph extraction for the radial map
- **learnings**: make qmd search killable via a SearchCancel handle
- **learnings**: map interaction state, mouse hit-test, recentre animation
- **learnings**: non-blocking document search with spinner + timeout
- **learnings**: render the radial ego map into a ratatui buffer
- **learnings**: two-stage BM25 fast-paint for document search
- **learnings**: wire the radial map into the Graph tab + plugin
- **plugin-config**: resolve per-plugin config from config.toml and inject at init
- **plugin-learnings**: Browse tab + filter chips + tabbed UI shell
- **plugin-learnings**: Detail/read pane (Enter opens, Backspace closes)
- **plugin-learnings**: Graph tab — typed entity neighbourhood + community clusters
- **plugin-learnings**: Search tab — query box, qmd ranked results, open detail
- **plugin-learnings**: data layer — records, graph, qmd search, filters
- **plugin-learnings**: scaffold plugin crate + host screen wiring
- **plugin-learnings**: wire /recall and /memory slash commands to open the screen
- **plugin-protocol**: add read_paths capability, [config] schema, and InitParams.config
- **plugin-protocol**: add render redraw-hint for self-animation
- **plugin-protocol**: forward mouse events to focused plugin
- **plugin-runtime**: bound runaway plugin self-redraws with a host governor
- **plugin-runtime**: enforce read_paths on host/fs reads + CTS conformance axis
- **plugin-sdk**: forward resolved config to Plugin::on_init via InitContext
- **session-reader**: skip aggregation and republish when snapshot unchanged
- **tui**: make abtop a first-class overlay panel reachable from the session list
- **tui**: make the learnings panel conform to the overlay-panel contract
- **types-sessions**: add terminal done flag to ScanProgressEvent

### Fixed
- **fleet**: correct validate-fleet.sh assertions + teardown for real agents
- **learnings**: clip map render to the buffer to avoid get_mut panic
- **learnings**: kill orphaned qmd children on search timeout and supersede
- **learnings**: make ego representative-edge tiebreak total
- **plugin-runtime,sdk,cts**: clear strict-clippy bar on redraw/mouse paths
- **plugin-sdk**: dispatch handle_mouse inline to preserve event order
- **tui**: gate plugin render kicks to the focused screen

### Documentation
- **fleet**: add ainb-fleet plugin README
- **fleet**: hybrid enrich locus + token-efficiency roadmap
- **fleet**: reframe skills around tmux-first transport + toggle
- **learnings**: add radial ego local-graph spec
- **learnings-plugin**: add design spec + TDD phase plan
- **plugin-protocol,cts**: fix bytes_serde header + cts axis count
- **plugins**: add the learnings (memory browser) plugin page
- **skills**: capture tmux recording/tripwire gotchas from the map build

### Other
- Merge pull request #258 from stevengonsalvez/feat/issue-255-incremental-aggregate
- **learnings**: cache ego subgraph + layout, re-anchor map selection after hop/expand
- **session-reader**: size chunks per item instead of re-probing whole chunks
- **plugin-learnings**: apply review polish across the learnings plugin


## [1.6.0] - 2026-06-10
### Added
- Merge pull request #179 from stevengonsalvez/feat/multica
- Merge pull request #240 from stevengonsalvez/feat/abtop
- Merge pull request #244 from stevengonsalvez/docsite-image-zoom
- Merge pull request #249 from stevengonsalvez/f/overlay-panels
- Merge pull request #251 from stevengonsalvez/feat/session-reader-refresh-modes
- Merge pull request #252 from stevengonsalvez/feat/release-bundle-plugins
- docs(hangar): add hangar-parity epic execution goal-file
- **burndown**: gate hard refresh behind a confirm overlay on R
- **cli**: add 'ainb abtop' snapshot command
- **cli**: add --hard to ainb usage for a full source rebuild
- **hangar**: P0.1 — ainb-hangar-store crate + workspace/user/member migrations
- **hangar**: P0.2-P0.5 — ainb-hangar-store schema, pool, repos
- **hangar**: P0.6-P0.7 — core + proto scaffolds + daemon binary stub
- **hangar**: P1.1 — task lifecycle state enum + transition invariants
- **hangar**: P1.2-P1.5 — store task FSM services (claim/start/complete/fail/cancel/finalize/retry)
- **hangar**: P1.4/P1.6/P1.7 — daemon runtime: sweepers, per-task env, worktree, claude runner
- **hangar**: P2.1 — beads_mapping repo + sync schema (migration 0007)
- **hangar**: P2.2-P2.5 — beads sync engine (adapter, outbound, inbound, reconcile)
- **hangar**: P2.3 — polymorphic assignee crosswalk (hangar (actor_type,id) <-> bd string)
- **hangar**: P3.1 — host/event_stream_subscribe protocol + capability
- **hangar**: P3.2-P3.4 protocol — spawn_managed_subprocess + unix_socket_dial methods/params/caps
- **hangar**: P3.2-P3.4 runtime — event_stream / spawn_managed / unix_socket handlers
- **hangar**: P3.2-P3.4 sdk — host_client helpers for the 3 new caps
- **hangar**: P3.5 — host/secret_store_get cap (mac Keychain; linux stub)
- **hangar**: P3.6-P3.8 — hangar-tui plugin scaffold, daemon dial, connect tripwire
- **hangar**: P4.1-P4.3 — TUI routing/chrome, event-stream client, issue list screen
- **hangar**: P4.10 — daemon unix-socket JSON-RPC server + snapshot RPCs + seed
- **hangar**: P4.10 — host wiring: HANGAR plugin screen + 'g' nav
- **hangar**: P4.10 — plugin render dispatch + key routing + snapshot fetch
- **hangar**: P4.10 — proto snapshot RPC wire types
- **hangar**: P4.2 — HangarEvent wire types (proto) for TUI event stream
- **hangar**: P4.4-P4.7 — proto wire types for TUI screens
- **hangar**: P4.4-P4.8 — TUI screens (task detail, agent picker, skill manager, settings, banner state)
- **hangar**: P4.4-P4.8 — TUI widgets (transcript, sidebar, actor row, file tree, editor, key entry, banner, presence dot)
- **hangar**: P5.2 propagate secrets:read cap rename to SDK + discovery test
- **hangar**: P5.2 secret_store_get protocol — {scope,key} params + secrets:read cap
- **hangar**: P5.2 secret_store_get runtime handler wired to SecretBackend
- **hangar**: P5.3 'hangar config env.allow' CLI verbs
- **hangar**: P5.3 env.allow.toml loader + build_task_env seam
- **hangar**: P5.3 env_policy module — allowlist with hardcoded deny override
- **hangar**: P5.3 wire env policy into the daemon claim loop
- **hangar**: P5.6 'hangar config warnings reset' CLI verb
- **hangar**: P6.1 — SkillRepo typed CRUD + workspace scoping
- **hangar**: P6.1 — skill domain types (SkillName, SkillWithFiles, SkillId)
- **hangar**: P6.1 — unique index on skill(workspace_id, name)
- **hangar**: P6.2 — `ainb hangar skills sync|list` CLI
- **hangar**: P6.2 — toolkit-directory skills sync importer
- **hangar**: P6.3 — TemplateRegistry over embedded curated templates
- **hangar**: P6.3 — add 10 curated agent_template JSONs
- **hangar**: P6.3 — ainb hangar templates list|show|use CLI verbs
- **hangar**: P6.3 — build.rs guard for template skill refs
- **hangar**: P6.3 — transactional templates_use in daemon
- **hangar**: P6.4 — materialise agent skills into per-task provider layout
- **hangar**: P6.5 IO-free SkillService over a SkillBackend trait
- **hangar**: P6.5 daemon RPC handlers for skill get/sync/attach/detach
- **hangar**: P6.5 skill RPC method consts + wire envelopes
- **hangar**: P7.1 — cron parser + next-tick calculator
- **hangar**: P7.2 — AutopilotRepo sqlx queries (workspace-scoped)
- **hangar**: P7.2 — IO-free AutopilotService + workspace-scoped backend
- **hangar**: P7.2 — autopilot + autopilot_run schema (migration 0009)
- **hangar**: P7.3 — autopilot scheduler thread + cron tick loop
- **hangar**: P7.3 — spawn the autopilot scheduler in the daemon boot path
- **hangar**: P7.5 autopilot RPC method consts + wire shapes
- **hangar**: P7.5 autopilot manager screen + tab strip + keybindings
- **hangar**: P7.5 daemon RPC handlers for autopilot list/runs/fire/toggle
- **hangar**: P7.5 wire autopilot screen to live daemon RPCs
- **hangar**: P7.6 ainb hangar autopilot CLI verbs
- **hangar**: P7.6 scheduler wake hook + advanceable test clock
- **hangar**: P8.1 — install tracing subscriber + rolling JSONL sink in daemon main
- **hangar**: P8.2 — env-driven OTLP exporter behind optional `otlp` feature
- **hangar**: P8.4 Kanban board screen — 4 columns + card widget
- **hangar**: P8.4 daemon RPC — hangar/tasks_list + hangar/task_transition
- **hangar**: P8.4 proto — tasks_list + task_transition wire surface
- **hangar**: P8.4 store — TaskRepo list_by_workspace + transition_status
- **hangar**: P8.5 daemon-health screen + D hotkey wiring
- **hangar**: P8.5 daemon-health wire types + hangar/daemon_health method
- **hangar**: P8.5 dual-dim throughput sparkline widget
- **hangar**: P8.5 hangar/daemon_health RPC handler
- **hangar**: P8.5 in-memory health stats collector + finalize feed
- **hangar**: P9.1 — capture gh pr create URL into task result
- **hangar**: P9.1 — gh pr create URL parser + TaskResult shape
- **hangar**: SecretBackend trait, SecretError, SecretBytes, Scope
- **hangar**: add agent_task_queue.autopilot_run_id link column
- **hangar**: add autopilot_run_id to NewTask/Task + TaskRepo::insert_in_tx
- **hangar**: ainb hangar auth token + daemon-token CLI verbs
- **hangar**: ainb hangar logs tail CLI verb
- **hangar**: cascade autopilot run completion on task finalize
- **hangar**: fire_autopilot_tick single-tx run + task enqueue path
- **hangar**: instrument autopilot tick fire with a tracing span
- **hangar**: instrument beads sync push/pull with tracing spans
- **hangar**: instrument task FSM transitions with tracing spans
- **hangar**: issue create --assign enqueues a task for the agent
- **hangar**: mac keychain, linux stub, and in-memory secret backends
- **hangar**: pat + daemon_token repos with mint/verify/revoke
- **hangar**: scaffold ainb-hangar-secrets crate + workspace membership
- **hangar**: shared structured-log reader for daemon.<date> files
- **hangar**: token mint + verify primitives (sha256, constant-time)
- **hangar**: wire 'ainb hangar <verb>' CLI namespace into ainb binary
- **hangar**: wire skill materialisation into the dispatch path
- **hangar**: wire skill-manager screen to live daemon RPCs
- **hangar-core**: P5.6 danger-full-access warning ack keys + decision
- **hangar-daemon**: P5.6 warn danger-full-access at provider invocation
- **hangar-daemon**: surface latest completed-task pr_url in issues_list RPC
- **hangar-proto**: WorkspaceChanged event + WorkspaceRow slug/default fields
- **hangar-proto**: add additive pr_url field to IssueRow wire type
- **hangar-tui**: P5.6 danger-full-access modal widget
- **hangar-tui**: P5.6 first-run danger-full-access flow
- **hangar-tui**: PR badge on task detail + 'o' open-in-browser keybinding
- **hangar-tui**: Settings Workspace pane — s/d/n/r keys + active indicator
- **hangar-tui**: logs tail screen with level-filter chips (L hotkey)
- **hangar-tui**: wire Workspace switch intents to host/workspace_* caps
- **plugin**: add ainb-plugin-abtop crate
- **plugin-burndown**: Esc pops one level, asks host to close at root
- **plugin-protocol**: reserved ui.close_request topic + versioned snapshot read
- **plugin-protocol**: workspace:write cap + host/workspace_* methods + params
- **plugin-runtime**: P5.6 host-side warnings_ack state.toml IO
- **plugin-runtime**: host/workspace_* handlers + state.toml-backed store
- **plugin-sdk**: HostClient workspace_list/get_active/set_active/set_default
- **plugins**: seed host workspace store catalogue from hangar.db
- **release**: bundle first-party plugins into release artifacts
- **session-reader**: dispatch incremental vs hard refresh by payload
- **session-reader**: incremental scan path with watermark partition
- **session-reader**: persist the stable aggregate (cache schema v2)
- **session-reader**: read incremental_window_days from config
- **session-reader**: split aggregate into mergeable fold/emit stages
- **site**: click-to-zoom lightbox on all docsite images
- **tui**: add abtop (top-for-agents) menu item + full-screen embed
- **tui**: advertise panel keys on the session-list legend and help overlay
- **tui**: panels return to their origin screen; forward Esc to plugins
- **tui**: redesign session-page menu legend into three lines
- **xtask**: add ci-lint subcommand asserting hangar-e2e CI contract

### Fixed
- Merge pull request #237 from stevengonsalvez/worktree-legend-fixes
- Merge pull request #238 from stevengonsalvez/worktree-fix-startup-session-discovery
- Merge pull request #246 from stevengonsalvez/fix/remote-pick-branch-guard
- docs(tui): correct Inbox keybinding to b in keyboard-shortcuts
- **abtop**: canonical graykode install hints + reuse setup tmux session
- **abtop**: drop unused serde dep, move serde_json to dev-deps
- **ci**: ignore SDK false positive in hangar-tui machete scan
- **docs**: remove duplicate jump-over arc at line crossing in ecosystem diagram
- **hangar**: daemon resolves workspace slug->id for snapshot RPCs
- **hangar**: de-alias issue-list tripwire's settings-detection from tab strip
- **hangar**: implement csv + markdown output formats for hangar CLI
- **hangar**: reject list-form workspace:write at the cap gate (-32003)
- **hangar**: workspace-scope SkillRepo by-id methods (IDOR)
- **hangar-tests**: seed notifyd install.json in the tripwire harness
- **plugin-sdk**: add macOS parent-death watcher to prevent orphaned plugins
- **plugins**: honour ui.close_request only from the screen-owning plugin
- **plugins**: reject a plugin named 'host' on the register path too
- **session-reader**: harden refresh edges from code review
- **session-reader**: serialize concurrent cache writers with busy_timeout
- **test**: await the spawn render before send_key in fixture_e2e
- **test**: make tripwire_burndown_keys period/provider captures deterministic
- **test**: seed install.json so tripwires aren't blocked by the hooks popup
- **tui**: Hangar panel saves its origin so Esc doesn't pop a stale screen
- **tui**: align session-list keybindings with the menu legend
- **tui**: seed branch-collision guards from clone cache for remote picks
- **tui**: surface stopped sessions on startup without a manual refresh
- **tui**: undelivered Esc/q falls through on the plugin placeholder screen
- test(hangar): cover P9.2 PR badge render, 'o' keybinding, and e2e tripwire

### Documentation
- Merge pull request #242 from stevengonsalvez/fleet-readme-only
- Merge pull request #243 from stevengonsalvez/docs-ecosystem-architecture-svg
- Merge pull request #245 from stevengonsalvez/chore/standup-skill-restructure
- **abtop**: correct CLI dispatch, detection, and consent claims
- **abtop**: correct live-monitor keybindings to verified v0.4.7 keys
- **abtop**: goal tracker for the abtop plugin work
- **fleet**: add ainb-fleet plugin README
- **hangar**: 6 full-detail architecture diagrams (system, dataflow, FSM, schema ER, capabilities, scheduler)
- **hangar**: P4 Hangar TUI asciinema proof + capture script
- **hangar**: Starlight architecture & features page (full detail, SVG diagrams, coverage table)
- **hangar**: add P0 TDD plan — Schema + crates skeleton
- **hangar**: add P1 TDD plan — Daemon + task FSM
- **hangar**: add P2 TDD plan — Beads sync adapter
- **hangar**: add P3 TDD plan — Plugin host caps + hangar-tui scaffold
- **hangar**: add P4 TDD plan — Core 5 TUI screens
- **hangar**: add P5 TDD plan — Auth + workspace + secret store
- **hangar**: add P6 TDD plan — Skills + curated templates
- **hangar**: add P7 TDD plan — Autopilots + cron scheduler
- **hangar**: add P8 TDD plan — Kanban + Daemon health + observability
- **hangar**: add P9 TDD plan — gh integration + e2e pass + release
- **hangar**: add architecture explainer, diagrams, and index
- **hangar**: add hangar-parity epic execution goal-file
- **hangar**: add multica feature-parity review explainer
- **hangar**: add multica research findings
- **hangar**: add verify-hangar autonomous verification goal-file
- **hangar**: architecture + feature/test-coverage explainer (HTML)
- **hangar**: asciinema proof of ainb hangar CLI round-trip (174.11)
- **hangar**: document TUI keybindings incl. P6.5 skill actions
- **hangar**: lock build-plan via interview — 20 decisions across 5 rounds
- **hangar**: mark P0 + P1 done in phase tracker
- **hangar**: mark P2 done + flag CLI-wiring gap (174.11)
- **hangar**: mark P3 done — plugin host caps + hangar-tui scaffold
- **hangar**: mark P4 complete in build-plan
- **hangar**: mark P5 complete in build-plan
- **hangar**: mark P6 complete in build-plan
- **hangar**: mark P7 complete in build-plan
- **hangar**: mark P8 complete in build-plan
- **plugins**: document abtop (top-for-agents) plugin with recordings
- **plugins**: wire abtop into plugin index and Astro nav
- **screenshots**: add overlay-panels return-to-origin demo GIFs
- **site**: add Hangar sidebar group + exclude internal hangar build docs from the Starlight glob
- **standup**: restructure skill onto reader-facts + Bad/Good rules
- **tui**: correct Inbox keybinding to b in keyboard-shortcuts
- **tui**: sync help overlay with actual session-list keybindings
- add ecosystem architecture diagram to whole-system page
- embed ecosystem architecture diagram in README

### Other
- **hangar-scripts**: surface SKIPs in run_all_tripwires output
- **scripts**: add soak watch for the incremental-refresh contract
- drop abtop goal-tracker from the PR (work complete)
- drop nightly-only .rustfmt.toml
- ignore local here.now publish state
- **hangar**: adopt typed attach_to_agent in P4 seed fixture
- **hangar**: extract seed_runtime_and_agent from P4 seed fixture
- **hangar**: thread workspace through SkillRepo callers
- **tui**: build cache path once in cached_source_path
- **tui**: pass the runtime handle into tick_panel_close_requests
- **tui**: route sidebar panel selects through canonical GoTo events


## [1.5.0] - 2026-06-08
### Added
- Merge pull request #230 from stevengonsalvez/feat/statusline-quota-reset-time
- **statusline**: show quota reset times on the Claude Code statusline
- **tui**: add GitHub auth pre-check for remote URLs in pick-repo

### Fixed
- Merge pull request #155 from stevengonsalvez/worktree-fix-github-auth-tui
- Merge pull request #232 from stevengonsalvez/worktree-fix-worktree-create-error
- Merge pull request #233 from stevengonsalvez/worktree-fix-bulk-resume-sessions
- Merge pull request #234 from stevengonsalvez/fix/branch-exists-selection-guard
- **tui**: block base-off onto an existing branch at selection
- **tui**: bound the GitHub auth pre-check with a 5s timeout
- **tui**: handle new-session onto an already-checked-out branch
- **tui**: push ahead commits when there is nothing new to commit
- **tui**: remove the partial clone directory on clone failure
- **tui**: start all selected sessions on Enter/r, not just the highlighted one
- **tui**: suppress git credential prompts on all network-facing commands

### Documentation
- Merge pull request #231 from stevengonsalvez/reflect-sync-learnings-fixes
- **sync-learnings**: harden classification + diff guidance

### Other
- **tui**: dedup in_use_branch_names via HashSet


## [1.4.4] - 2026-06-07
### Added
- Merge pull request #224 from stevengonsalvez/worktree-codex-quota-reset
- **tui**: show per-window quota reset date/time in top bar

### Fixed
- Merge pull request #228 from stevengonsalvez/fix/statusline-resets-at-epoch
- **compress**: harden scripts against missing CLI and interrupts
- **tui**: parse Claude Code rate-limit resets_at as Unix epoch

### Other
- Merge pull request #225 from stevengonsalvez/chore/skills-root-and-scratch-cleanup
- Merge pull request #226 from stevengonsalvez/worktree-skills-into-dotclaude
- consolidate skills under repo-root .claude/skills
- drop caveman skill family
- drop committed scratch and ignore output dirs
- move skills to repo root and symlink tool dirs
- symlink ainb-tui/AGENTS.md to root AGENTS.md


## [1.4.3] - 2026-06-05
### Added
- **skills**: add agentmail disposable-inbox skill
- **skills**: add test-ainb 5-layer ainb test runner
- **skills**: swarm v2 watchdog, cross-provider, and attach-watchdog
- **statusline**: show reasoning effort and fast-mode on line 2
- **tui**: simplify the session-screen starter content

### Fixed
- Merge pull request #223 from stevengonsalvez/fix-onboarding-ux
- **tui**: advance the onboarding wizard with the right arrow on every step
- **tui**: keep the starter tip on one line for the per-line markdown styler

### Other
- Merge pull request #221 from stevengonsalvez/worktree-sync-learnings
- **bootstrap**: drop webapp-testing browser-tools compile step
- **deps**: track skill inventory changes
- **skills**: remove compound-docs
- **skills**: sync skill updates from user-level
- remove webapp-testing skill and stray agent yamls


## [1.4.2] - 2026-06-05
### Added
- Merge pull request #216 from stevengonsalvez/feat/diff
- **diff**: add 'ainb diff-review [path]' subcommand
- **diff**: add Code Review interactions — collapse, expand, hunk jump, file nav
- **diff**: add Dracula syntax-highlight bridge with word-emphasis merge
- **diff**: add structured Code Review diff model + git/similar parser
- **diff**: render unified Code Review surface as the default G view
- **diff**: tree-structured sidebar with arrow-key nav and mouse

### Fixed
- **code-review**: harden context expansion, drop dead code, fix docs

### Documentation
- **readme**: showcase the Warp-style Code Review diff
- **tui**: add Code Review page with diff GIFs
- **tui**: document tree sidebar, arrow nav, and mouse in Code Review

### Other
- **release**: prepare v1.4.1
- **release**: prepare v1.4.2
- **skills**: add tmux-verify TUI proof-loop skill
- **diff**: cap highlighting on pathological lines + large-diff render test


## [1.4.2] - 2026-06-05
### Added
- Merge pull request #216 from stevengonsalvez/feat/diff
- **diff**: add 'ainb diff-review [path]' subcommand
- **diff**: add Code Review interactions — collapse, expand, hunk jump, file nav
- **diff**: add Dracula syntax-highlight bridge with word-emphasis merge
- **diff**: add structured Code Review diff model + git/similar parser
- **diff**: render unified Code Review surface as the default G view
- **diff**: tree-structured sidebar with arrow-key nav and mouse

### Fixed
- **code-review**: harden context expansion, drop dead code, fix docs

### Documentation
- **readme**: showcase the Warp-style Code Review diff
- **tui**: add Code Review page with diff GIFs
- **tui**: document tree sidebar, arrow nav, and mouse in Code Review

### Other
- **release**: prepare v1.4.1
- **skills**: add tmux-verify TUI proof-loop skill
- **diff**: cap highlighting on pathological lines + large-diff render test


## [1.4.1] - 2026-06-05

## [1.4.0] - 2026-06-04
### Added
- Merge pull request #211 from stevengonsalvez/feat/new-session-base-branch-picker
- Merge pull request #217 from stevengonsalvez/worktree-reflect-one-step-install
- **ainb**: add doctor + reflect bootstrap one-step installer
- **git**: list repo branches and cut worktrees off explicit base refs
- **reflect-kb**: expose errors count/ack/append on the reflect CLI
- **tui**: base-branch picker on the Configure Branch row

### Fixed
- **ainb**: print the full plan in reflect bootstrap --print-only when uv missing
- **ainb**: require the reflect binary for reflect-kb detection
- **statusline**: gtimeout fallback + drop unpublished uv-with fallback
- **statusline**: self-bootstrap reflect error callers off bare python3 -m
- **tui**: mark the repo's own checked-out branches in-use in the base picker

### Documentation
- Merge pull request #212 from stevengonsalvez/docs/notifications-screenshots
- **reflect**: one-step install on the plugin docsite page
- **reflect**: rewrite install section for the one-step flow
- **reflect-kb**: document the `reflect errors` subcommand
- **tui**: assert modal exclusivity invariant in configure key routing
- **tui**: current screenshots — live markers, home sidebar, refreshed inbox
- **tui**: document `ainb doctor` and `ainb reflect` commands
- **tui**: document notifyd --format output option (text/json/csv/markdown)
- **tui**: embed the live-marker and home-screen screenshots
- **tui**: record own-checkout in-use edge in the base picker spec
- **tui**: spec for the new-session base-branch picker
- Codex notifications verified end-to-end — update agent-support wording


## [1.3.3] - 2026-06-03
### Added
- Merge pull request #205 from stevengonsalvez/feat/claude-plugin-install
- Merge pull request #209 from stevengonsalvez/worktree-star-remote-main-base
- **favorites**: derive remote indicator from origin + migrate legacy local stars
- **git**: branch worktree off remote default (origin/HEAD)
- **notifyd**: expose classify_attention + Store::recent_since
- **notifyd**: register Claude plugin via the claude CLI on install
- **session**: launch remote/star sessions off the remote default branch
- **tui**: enforce remote-or-refuse on both star entry points
- **tui**: first-run prompt states notifications work with Claude today
- **tui**: migrate legacy favorites at startup + worktree-base tests

### Fixed
- Merge pull request #206 from stevengonsalvez/feat/session-attention-marker
- Merge pull request #207 from stevengonsalvez/fix/new-session-pickrepo-paste
- Merge pull request #210 from stevengonsalvez/fix/attention-marker-launch-floor
- **favorites**: copy raw file for pre-migration backup
- **favorites**: reject non-shareable origins + back up before migration
- **git**: force-create branch on worktree checkout retry
- **session**: skip remote-worktree prep outside Interactive mode
- **tui**: correct star toggle matching + confirm only on success
- **tui**: drive session marker from hook events, not idle state
- **tui**: enable paste in the New Session repo picker
- **tui**: report favorite migration success only after it persists
- **tui**: surface pre-launch waiters — drop the marker app-start floor

### Documentation
- Merge pull request #208 from stevengonsalvez/feat/notifications-docs-claude-callout
- **plans**: add star-remote + main-base implementation plan
- **plugins**: correct ainb-hooks install to the claude CLI / marketplace
- **tui**: make inbox-notifications the full notifications reference
- **tui**: marker window is 6h, not floored at app start


## [1.3.2] - 2026-06-02
### Added
- Merge pull request #202 from stevengonsalvez/fix/config-popup-paste-hint
- Merge pull request #203 from stevengonsalvez/feat/idle-waiting-marker
- **marketplace**: publish ainb-hooks as an installable plugin
- **tui**: show [?] on any waiting session, not just box prompts
- **tui**: show greyed 'Ctrl+V to paste' hint in config text popups

### Fixed
- Merge pull request #201 from stevengonsalvez/fix/config-popup-ctrl-v-paste
- Merge pull request #204 from stevengonsalvez/feat/publish-ainb-hooks-plugin
- **tui**: add Ctrl+V clipboard paste to config text popups

### Other
- **tui**: reuse is_text_entry() for the paste-hint guard


## [1.3.1] - 2026-06-02
### Added
- Merge pull request #198 from stevengonsalvez/feat/session-alert-markers
- **notifyd**: classify hook events into AlertKind + per-cwd unread-state query
- **tui**: auto-save config edits to config.toml on popup confirm
- **tui**: color-coded per-session attention markers in session list
- **tui**: drive per-session marker from live pane state, not notifications

### Fixed
- Merge pull request #196 from stevengonsalvez/worktree-config-popup-paste-edit
- Merge pull request #197 from stevengonsalvez/fix/config-default-workspace-persist
- Merge pull request #199 from stevengonsalvez/feat/live-session-markers
- **tui**: enable paste and cursor editing in config text popups
- **tui**: write Default Workspace edit as primary scan path

### Other
- Merge pull request #195 from stevengonsalvez/chore/precommit-fmt-gate
- add pre-commit config (cargo fmt check + hygiene hooks)
- **notifyd**: drop superseded classify_event + unread_state_by_cwd


## [1.3.0] - 2026-06-01
### Added
- Merge pull request #189 from stevengonsalvez/fix/stats
- Merge pull request #194 from stevengonsalvez/worktree-notify-install-prompt
- **burndown**: adjustable columns and row copy in zoom tables
- **notifyd**: first-run prompt to install notification hooks + drift detection

### Fixed
- **burndown**: correct copy-flash lifecycle
- **burndown**: resolve zoom detail drawer through filtered records
- **burndown**: wire zoom-table fuzzy-search text input
- **notifyd**: rustfmt wraps + seed install.json in inbox tripwire

### Documentation
- Merge pull request #193 from stevengonsalvez/docs/plugin-screenshots
- add real in-ainb screenshots to the plugin + inbox pages


## [1.2.2] - 2026-06-01
### Added
- Merge pull request #159 from stevengonsalvez/feat/witr-plugin
- Merge pull request #176 from stevengonsalvez/worktree-hangar-standup-brief
- Merge pull request #177 from stevengonsalvez/worktree-hangar-resilience
- Merge pull request #183 from deepaks7n/feat/new-session-picker-show-path
- Merge pull request #188 from stevengonsalvez/worktree-inbox-actionable-only
- Merge pull request #192 from stevengonsalvez/worktree-burndown-filewatch
- **ainb-fleet**: hangar retries transient agent failures + surfaces read errors
- **ainb-fleet**: standup verb returns per-workspace briefing
- **notifyd**: only surface events that need the user; drop telemetry
- **reflect**: add /reflect:cost sub-skill for drain spend reporting
- **reflect**: cascade gate+slice before /reflect (W4)
- **reflect**: circuit breaker in drain script (W1)
- **reflect**: cost observability — envelope + reflect cost + backfill (W3)
- **reflect**: enqueue skip-gate + dedup (W2)
- **reflect**: structural rebuild — surfacer retire, graphml heal, re-gate, synthesis (W5)
- **tui**: live-refresh burndown usage snapshot on provider-dir changes
- **tui**: show repo path in new-session picker
- **witr**: add a Witr tile to the home sidebar
- **witr**: cfx.5 - main 4-tab TUI + key handling + LRU cache
- **witr**: detect.rs - which witr + version parse + min-version gate
- **witr**: embed witr's interactive browser instead of a plugin screen
- **witr**: event-bus publisher - witr.snapshot topic
- **witr**: model.rs + exec.rs - JSON parse + subprocess exec with timeout
- **witr**: register + navigate to the witr plugin screen in the host
- **witr**: render/detail.rs - process detail overlay
- **witr**: render/empty.rs - missing-witr + outdated empty state
- **witr**: scaffold ainb-plugin-witr crate + manifest
- **witr**: slash.rs + cli.rs - /witr + ainb witr CLI namespace

### Fixed
- Merge pull request #186 from stevengonsalvez/worktree-inbox-shortcut-rebind-to-b
- Merge pull request #187 from stevengonsalvez/worktree-ainb-notifyd-subcommand
- **cli**: add hidden 'ainb notifyd' subcommand for hook lazy-spawn
- **nav**: accept witr in is_known_screen_id
- **plugins**: let focused plugin screens receive the `:` key
- **plugins**: re-render plugin screens when their viewport changes
- **reflect**: build drain JSONL log lines with json.dumps
- **reflect**: gate reflect-on-reflect via machine markers, wider scan
- **tui**: rebind Inbox shortcut from Shift+I to plain 'b'
- **witr**: decode real witr output — non-zero-with-JSON exit + Go null slices
- **witr**: kill the witr child process on exec/detect timeout
- **witr**: target-prompt cancel hint says Backspace, not host-reserved Esc

### Documentation
- Merge pull request #173 from stevengonsalvez/docs/resync-audit
- Merge pull request #181 from stevengonsalvez/docs/per-plugin-docsite-pages
- Merge pull request #182 from stevengonsalvez/docs/fix-duplicate-titles
- Merge pull request #184 from stevengonsalvez/docs/claude-code-plugins
- **ainb-tui**: add project intro to README
- **ainb-tui**: correct architecture diagram to crates workspace layout
- **ainb-tui**: document ainb fleet subcommand family in README
- **ainb-tui**: fix component/widget paths to crates/ainb-core/src
- **contributing**: unfold ci-cd stub with the four real workflows
- **knowledge**: document reflect plugin skills and wiring in CLI reference
- **knowledge**: replace reflect-cli stub with real CLI reference
- **plugins**: add notifyd to reference plugins in README
- **plugins**: add v2 plugin architecture diagram + two-render-paths brief
- **plugins**: add witr to the bundled reference plugins
- **plugins**: dedicated per-plugin docsite pages + diagrams
- **plugins**: disambiguation lists all three Claude Code plugins
- **plugins**: document notifyd reference plugin in overview
- **plugins**: fix stale witr framing + link per-plugin pages + render images
- **plugins**: note cts-v2 and testkit crates in conformance section
- **product**: correct toolkit deploy-target count to 11 in what-is-ainb
- **product**: fix 'nine AI tools' deploy count to 11 in value 'what it costs'
- **product**: fix 'nine targets' deploy count to 11 in value 'what you get'
- **readme**: add ainb-fleet plugin and ainb-hooks to plugins/ in architecture tree
- **readme**: add deploy-pages workflow to architecture tree
- **readme**: add published website link to Links section
- **readme**: correct Skills section heading count to 91
- **readme**: correct skill count in What's Inside table to 91
- **readme**: correct skill count in header badge line to 91
- **readme**: correct toolkit skills/agents counts in architecture tree
- **readme**: fix CLI command count from 15 to 20 and list all subcommands
- **readme**: fix ainb-tui source tree to reflect Cargo workspace under crates/
- **readme**: fix stale Homebrew tap link in Links section
- **readme**: remove non-existent claude-developer-platform from Agent Architecture skill group
- **reference**: add knowledge-base terms to glossary
- **reference**: add tmux and runtime terms to glossary
- **reference**: add v2 plugin contract terms to glossary
- **reference**: replace glossary stub with core agent and toolkit terms
- **reflect**: add errors-ack to sub-skills table
- **reflect**: bump version refs in README to 3.6.0
- **reflect**: correct PreCompact auto-install claim to match plugin.json
- **reflect**: document all five lifecycle hooks in hooks README
- **reflect**: lock cost re-architecture decisions via interview
- **reflect**: plan cost re-architecture after 41M-token drain incident
- **reflect**: record W1-W5 implementation status in spec
- **reflect**: rich v4.0.0 cost-rearchitecture explainer + arch diagram
- **reflect**: show all five wired lifecycle hooks in architecture diagram
- **reflect**: update docs for v4.0.0 cost rearchitecture
- **reflect-kb**: nest 'metrics stats' under metrics group in subcommands table
- **site**: drop duplicate page titles (Starlight renders frontmatter title)
- **toolkit**: Claude Code plugins section with per-plugin pages + diagrams
- **toolkit**: add claude-langfuse and langfuse-setup to Security & Observability group
- **toolkit**: add explain-to-me to Research & Knowledge group
- **toolkit**: add git-history-surgery to Coding & GitHub group
- **toolkit**: add make-a-goal to Planning & Workflow group
- **toolkit**: add standup and tmux-message to Dev infra & tooling group
- **toolkit**: correct skill count in packages tree to 91
- **toolkit**: correct skill count in skills-at-a-glance heading to 91
- **toolkit**: document catalog.yaml in References
- **toolkit**: fix Design & UI group count to 13
- **toolkit**: fix Session & Learning group count to 8
- **toolkit**: fix skills count in title and intro
- **toolkit**: replace agents stub body with real category breakdown
- **toolkit**: replace bootstrap stub with sourced content
- **toolkit**: replace overview stub with sourced content
- **toolkit**: replace skills stub body with real grouped catalog
- **tui**: add ainb-core module tree to architecture
- **tui**: add claudecode, plugin, and fleet to command-reference TOC
- **tui**: add plugin runtime and testing sections to architecture
- **tui**: bump documented ainb version to 1.2.0
- **tui**: document the claudecode subcommand
- **tui**: document the fleet subcommand
- **tui**: document the plugin subcommand
- **tui**: replace architecture stub with crates workspace scaffold
- **tui**: replace install stub with curl and cargo methods
- **tui**: replace keyboard-shortcuts stub with verified keymap
- **tui**: replace overview stub with screen tour and session model
- **tui**: replace quickstart stub with first-session walkthrough
- mark legacy-layout migration complete in docs index
- move notifyd/Inbox out of Plugins → TUI (it's host code, not a plugin)

### Other
- Merge pull request #185 from deepaks7n/chore/ci-fmt-and-unused-deps
- **reflect**: bump to 4.0.0 + CHANGELOG for cost rearchitecture
- cargo fmt --all
- cargo fmt --all (post-merge)
- remove unused dependencies; ignore SDK false positives
- **tui**: cache home lookup, return Cow, native path separators
- **notifyd**: extract shared CLI bodies; dedupe two entrypoints


## [1.2.1] - 2026-05-29
### Added
- Merge pull request #172 from stevengonsalvez/worktree-hangar-enrich-model
- Merge pull request #175 from stevengonsalvez/worktree-cli-version-info
- **ainb-fleet**: make hangar enrich model configurable, default haiku
- **ainb-tui**: stamp git commit + build date into ainb --version

### Fixed
- Merge pull request #174 from stevengonsalvez/worktree-fix-delete-stale-session
- **ainb-tui**: purge sessions.json record even when worktree removal fails

### Documentation
- **ainb-tui**: document [plugins] enable/disable block in example config
- **ainb-tui**: note Analytics screen is backed by the burndown plugin
- add Plugins section covering toggle precedence and config


## [1.2.0] - 2026-05-29
### Added
- Merge branch 'worktree-goal-skill': /goal skill
- Merge pull request #101 from stevengonsalvez/swarm-1778540158-agent-1
- Merge pull request #102 from stevengonsalvez/swarm-1778540158-agent-2
- Merge pull request #103 from stevengonsalvez/swarm-1778540158-agent-1
- Merge pull request #104 from stevengonsalvez/swarm-1778540158-agent-1
- Merge pull request #106 from stevengonsalvez/feat/plugin-interactive-keys
- Merge pull request #109 from stevengonsalvez/worktree-session-number-shortcuts
- Merge pull request #119 from stevengonsalvez/feat/burndown-pivot-indicator
- Merge pull request #122 from stevengonsalvez/feat/usage-cli-global-format
- Merge pull request #123 from stevengonsalvez/feat/usage-export-folder-top-n
- Merge pull request #124 from stevengonsalvez/feat/usage-providers-and-models-by-task
- Merge pull request #131 from stevengonsalvez/feat/per-plugin-enable-disable
- Merge pull request #134 from stevengonsalvez/feat/disabled-plugin-friendly-placeholder
- Merge pull request #136 from stevengonsalvez/feat/session-list-loading-indicator
- Merge pull request #137 from stevengonsalvez/feat/plugin
- Merge pull request #147 from stevengonsalvez/feat/site-scaffold
- Merge pull request #149 from stevengonsalvez/worktree-reflect-codex-adapter
- Merge pull request #154 from stevengonsalvez/worktree-reflect-promptsubmit-hooks
- Merge pull request #160 from stevengonsalvez/worktree-tmux-rich-conf
- Merge pull request #161 from stevengonsalvez/worktree-brainstorm-ascii-revamp
- Merge pull request #163 from stevengonsalvez/worktree-ainb-hooks-plugin
- Merge pull request #164 from stevengonsalvez/worktree-ainb-hooks-plugin
- Merge pull request #166 from stevengonsalvez/worktree-new-session-redesign-spec
- Merge pull request #169 from stevengonsalvez/worktree-popa-skill
- Merge pull request #171 from stevengonsalvez/worktree-popa-skill
- Merge pull request #93 from stevengonsalvez/swarm-1778440729-agent-1
- Merge pull request #94 from stevengonsalvez/swarm-1778440729-agent-2
- Merge pull request #95 from stevengonsalvez/swarm-1778443661-agent-1
- Merge pull request #96 from stevengonsalvez/swarm-1778445635-agent-1
- Merge pull request #98 from stevengonsalvez/swarm-1778540158-agent-1
- Merge pull request #99 from stevengonsalvez/swarm-1778540158-agent-2
- **ainb-core**: add `ainb fleet` orchestration subcommand namespace
- **ainb-core**: add tmux-tests feature for opt-in TUI integration
- **ainb-core**: cli/usage.rs becomes plugin dispatch shim with exit-2 contract
- **ainb-core**: route Analytics screen through PluginHost.render
- **ainb-core**: wire PluginHost into App startup + load burndown.wasm
- **ainb-fleet**: add hangar multi-verb workflow
- **ainb-fleet**: fleet-needs cockpit skill (workflow-backed Jarvis)
- **ainb-fleet**: split into colon-namespaced sub-skills
- **ainb-fleet:standup**: auto-chain to /ainb-fleet:needs on ASK signals
- **ainb-plugin-cts**: publishable v1 conformance harness
- **ainb-tui**: numeric attach shortcuts on sessions screen
- **ainb-tui**: restart Idle session with its original CLI
- **app**: collapse plugin variants — AppEvent::Plugin + bridge + bus
- **burndown**: AINB_NOW env override on date_range_for_period
- **cli**: add Markdown variant to OutputFormat enum
- **cli**: add `reflect timeline --explain` subcommand
- **cli**: ainb tmux install|status subcommand
- **cli**: emit sidecar parse warnings to errors sink
- **cli**: handle Markdown in non-usage subcommand match arms
- **cli**: import retrieval stack from ai-coder-rules toolkit
- **cli**: real `ainb plugin` handlers replace Phase 2b stub
- **cli**: replace Commands enum with CliCommand registry
- **cli-usage**: 'usage models' + '--by-task' matrix subcommand
- **cli-usage**: --top N flag on UsageReportArgs
- **cli-usage**: expand --provider to cursor/copilot/gemini
- **cli-usage**: wire host CLI dispatcher to burndown plugin (Phase 7c)
- **cli/plugin**: add 'plugin lint' subcommand (Phase 7d-cli)
- **cli/plugin**: add 'plugin tail' subcommand (Phase 7d-cli)
- **cli/plugin**: add 'plugin watch' subcommand (Phase 7d-cli)
- **core**: PluginScreen translates crossterm keys + reserves host bindings
- **core**: add Screen trait + ScreenRegistry for screen dispatch
- **core**: add Screen::handle_key trait stub
- **core**: add built-in Screen impls for full-screen views
- **core**: route keys through focused plugin before global dispatch
- **core,plugin-runtime**: improve plugin lifecycle logging visibility
- **cts**: six axis-2-5,9,10 canaries
- **cts-v2**: add 14 canary plugin binaries for ABI v2 conformance
- **cts-v2**: scaffold conformance test suite crate for JSON-RPC ABI v2
- **errors**: structured pipeline error sink at errors.json
- **events**: wire AppEvent::NavigateTo through ScreenRegistry ids
- **explain-to-me**: add ADR + options-paper templates
- **explain-to-me**: publish via here.now + visual-first selection
- **fleet**: center control panel — `ainb fleet needs` v0.2
- **fleet**: synthesise session summary from JSONL transcript
- **init**: prompt for rich tmux conf in onboarding wizard
- **live-window**: emit tracing on tier transitions and error paths
- **marketplace**: seed first-party catalog at toolkit/.ainb-plugin/
- **metrics**: JSONL metrics writer with 10MB rotation
- **metrics+ci**: stats aggregator, dashboard sync, CI matrix, endpoint spec
- **nix**: flake with nano-graphrag dep chain override
- **notifyd**: add ainb-plugin-notifyd crate with daemon + install verbs
- **packaging**: pipx-installable pyproject.toml with dev/graph extras
- **plugin**: Phase 7 Wave 1+2 — subprocess runtime + host cutover (#90)
- **plugin**: friendly placeholder when plugin is disabled
- **plugin-api**: Request event variant + publish_reply host fn + cli_namespaces
- **plugin-api**: [subscribes] table for declarative event subscriptions
- **plugin-api**: add [paths] table to manifest schema
- **plugin-api**: add ainb-plugin-api crate
- **plugin-api**: bump ABI to 1.2.0 + catalogue Phase 6 host fns
- **plugin-api,plugin-host**: PluginEvent::Custom carries opaque bytes
- **plugin-burndown**: CLI handlers fetch UsageData via request_data
- **plugin-burndown**: Phase 7c — migrate to subprocess plugin (#91)
- **plugin-burndown**: _handle_event accepts Request{topic:sessions.usage_data}
- **plugin-burndown**: _render paints WireBuffer through ainb_render_buffer
- **plugin-burndown**: chunked ingest + refresh-request bootstrap
- **plugin-burndown**: declare cli_namespaces=["usage"] in plugin.toml
- **plugin-burndown**: drill-down on By Branch panel
- **plugin-burndown**: drop fs caps + subscribe to sessions.usage_data
- **plugin-burndown**: flash `↻ updated` chip-strip badge on pivot recompute
- **plugin-burndown**: handle sessions.usage_data via msgpack + converter
- **plugin-burndown**: implement SDK Plugin trait + main entry point
- **plugin-burndown**: manifest v2 with lazy lifecycle + snapshot subscribe
- **plugin-burndown**: markdown renderer for usage analytics
- **plugin-burndown**: move CLI layer (cli/usage.rs) into plugin
- **plugin-burndown**: move UI layer (components/usage.rs) into plugin
- **plugin-burndown**: move data + cache layers from ainb-core
- **plugin-burndown**: per-table CSV folder export with safety marker
- **plugin-burndown**: port Phase 6c analytics source to subprocess crate
- **plugin-burndown**: render scan-progress skeleton in cold-scan path
- **plugin-burndown**: scaffold cdylib crate + WASI build helpers
- **plugin-burndown**: subscribe to sessions.scan_progress in on_init
- **plugin-burndown**: thread --top through text and markdown renderers
- **plugin-burndown**: wire extern C ABI exports + plugin state singleton
- **plugin-burndown**: wire keys to UI state via handle_key
- **plugin-burndown,plugin-host**: real Analytics paint via ratatui Buffer
- **plugin-host**: add ainb-plugin-host with wasmi loader + capability gate
- **plugin-host**: add wasi-preview1 import stubs + fix wasm build script
- **plugin-host**: cache layout + global install flock (Phase 4)
- **plugin-host**: cross-plugin event bus + tick/render drivers + LoadOutcome
- **plugin-host**: host_fns/{fs,cache,request} foundation
- **plugin-host**: marketplace + lockfile schema (Phase 4)
- **plugin-host**: path_guard with allowlist canonicalisation + adversarial guards
- **plugin-host**: per-call fuel budget via PluginHost::with_fuel
- **plugin-host**: pump req:/rep: with correlation-id routing + publish_reply
- **plugin-host**: real ainb_fs_glob + ainb_data_read/write
- **plugin-host**: real ainb_fs_read with capability allowlist
- **plugin-host**: real ainb_render_buffer — decode + stash WireBuffer
- **plugin-protocol**: add Content-Length stdio framing
- **plugin-protocol**: add HandleKeyParams + KeyEvent wire types
- **plugin-protocol**: add JSON-RPC error codes + thiserror enum
- **plugin-protocol**: add JSON-RPC method-name constants
- **plugin-protocol**: add WireBuffer cell-based render output
- **plugin-protocol**: add manifest v2 schema (toml + serde)
- **plugin-protocol**: add request/response param structs
- **plugin-protocol**: register plugin/handle_key method
- **plugin-protocol**: scaffold ainb-plugin-protocol crate
- **plugin-protocol**: wire lib.rs re-exports + module map
- **plugin-runtime**: add slow fixture plugin for nonblocking validation
- **plugin-runtime**: scaffold ainb-plugin-runtime crate
- **plugin-runtime**: wire send_key through host → plugin pipeline
- **plugin-sdk-rust**: SdkError + Plugin trait + HostClient
- **plugin-sdk-rust**: Server stdio JSON-RPC dispatch + tests
- **plugin-sdk-rust**: add Plugin::handle_key trait method
- **plugin-sdk-rust**: scaffold crate (Cargo.toml + workspace registration)
- **plugin-sdk-rust**: wire plugin/handle_key inline dispatch
- **plugin-session-reader**: Phase 7c — migrate to subprocess plugin (#92)
- **plugin-session-reader**: Plugin trait impl + main.rs entry
- **plugin-session-reader**: SQLite usage cache module
- **plugin-session-reader**: cache-aware per-file parsers
- **plugin-session-reader**: cdylib scaffold + ABI + host wrappers
- **plugin-session-reader**: chunked publish gated on refresh_request
- **plugin-session-reader**: cursor parser scaffold + scanner hookup
- **plugin-session-reader**: emit host.log probes in on_init
- **plugin-session-reader**: handle sync sessions.usage_data Request events
- **plugin-session-reader**: open cache lazily and thread through scan
- **plugin-session-reader**: per-provider parsers + cost estimation
- **plugin-session-reader**: port FNV-1a hash + per-provider parsers
- **plugin-session-reader**: port scan + UsageData aggregator
- **plugin-session-reader**: publish sessions.scan_progress during scan
- **plugin-session-reader**: rate-limited scan ProgressReporter
- **plugin-session-reader**: scaffold subprocess crate (ABI v2)
- **plugin-session-reader**: scanner aggregator producing UsageData
- **plugin-testkit**: scaffold ainb-plugin-testkit crate with in-process Harness
- **plugin-types-sessions**: Provider::Cursor + WIRE_VERSION=3
- **plugin-types-sessions**: add ScanProgressEvent wire type
- **plugin-types-sessions**: chunked UsageDataEvent (WIRE_VERSION=2)
- **plugin-types-sessions**: wire schema for sessions.usage_data
- **plugins**: AINB_DISABLE_PLUGINS escape hatch
- **plugins**: per-plugin enable/disable via env + config.toml
- **plugins**: replace popa with docs-only `ainb-fleet` skill
- **plugins**: scaffold ainb-hooks plugin for claude + codex
- **providers, agents**: trait + registry replacing closed enums
- **reflect**: cache-aware token timeline with thrash detection
- **reflect**: codex adapter wires SessionStart + PreCompact hooks
- **reflect**: wire UserPromptSubmit recall + PostToolUse mini-learning + Stop enqueue
- **reflect-plugin**: add reflect_timeline.sh dashboard renderer
- **reflect-plugin**: drill-down via --explain mode + OSC 8 hyperlinks
- **reflect-plugin**: emit structured errors from drain failures
- **reflect-plugin**: show all 8 signals side-by-side (2 per row)
- **reflect-plugin**: three-letter acronym labels for sparkline rows
- **reflect-plugin**: warn at SessionStart when reflect-kb missing
- **reflect-recall**: switch recall.py from legacy learnings CLI to reflect
- **reflect:errors-ack**: wrap reflect_kb.errors ack as a slash skill
- **schema**: YAML frontmatter JSON Schema (v4)
- **schema**: pre-commit hook validating frontmatter
- **scripts**: add live-validate-ainb-hooks.sh host smoke
- **session-list**: spinner while workspaces are still scanning
- **session-reader,burndown**: F key wipes parse cache and republishes
- **session-reader,burndown**: pre-walk file count + N/M progress bar
- **skills**: /explain-to-me — rich HTML explainer generator
- **skills**: /goal — autonomous-run mega-prompt builder
- **skills**: add standup — branch-scoped read-only situation report
- **skills/brainstorm**: rewire as orchestrator delegating Q&A to /interview
- **skills/interview**: scan brainstorm-stub sections + diagram + template selection
- **statusline**: pass session_id + project_dir env to timeline helper
- **statusline**: reflect error badge
- **statusline**: side-channel feed to ainb-tui Live Window cache
- **statusline**: wire timeline dashboard + ack-hint on errors badge
- **team**: reflect team init/clone/sync commands
- **tests**: drive snapshot_baselines through PluginHost
- **tmux**: rich Catppuccin Mocha conf + git branch helper
- **toolkit**: remove legacy global-learnings skill
- **tui**: add Inbox screen for ainb-hooks notifications
- **tui**: add home sidebar mouse resizing
- **tui**: add sessions pane mouse controls
- **tui**: attach sessions on row double-click
- **tui**: cwd-based per-session badges + Enter-attaches-tmux
- **tui**: global inbox-unread badge on the menu bar
- **tui**: redesign new-session flow to 2-screen preset-driven wizard
- **tui**: slash-command palette stub at `:` key
- **website**: scaffold Astro Starlight site
- **write-flow**: confidence-gated routing for learning writes
- ainb usage CLI dispatches via plugin
- byte-identical tripwire — plugin render matches in-tree (4 tabs)

### Fixed
- Merge pull request #112 from stevengonsalvez/fix/decouple-event-poll-from-app-tick
- Merge pull request #113 from stevengonsalvez/fix/burndown-period-provider-filters
- Merge pull request #114 from stevengonsalvez/fix/burndown-activity-mcp-data
- Merge pull request #115 from stevengonsalvez/fix/burndown-drilldown
- Merge pull request #116 from stevengonsalvez/fix/burndown-project-chip-resolved-repo
- Merge pull request #125 from stevengonsalvez/fix/plugin-priority-key-channel
- Merge pull request #128 from stevengonsalvez/fix/burndown-esc-and-scan-indicator
- Merge pull request #129 from stevengonsalvez/fix/runtime-tokio-drop-panic
- Merge pull request #130 from stevengonsalvez/worktree-tui-text-input-shortcut-guard
- Merge pull request #133 from stevengonsalvez/worktree-address-gemini-review-130
- Merge pull request #135 from stevengonsalvez/worktree-decouple-docker-workspace-load
- Merge pull request #138 from stevengonsalvez/worktree-config-popup-text-input
- Merge pull request #139 from stevengonsalvez/worktree-narrow-config-popup-predicate
- Merge pull request #148 from stevengonsalvez/worktree-burndown-chunker-respawn-fix
- Merge pull request #150 from stevengonsalvez/worktree-reflect-silent-fail
- Merge pull request #151 from stevengonsalvez/worktree-precompact-codex-json
- Merge pull request #152 from stevengonsalvez/worktree-claude-adapter-skip-plugin
- Merge pull request #153 from stevengonsalvez/fix/hooks-svg-render
- Merge pull request #156 from stevengonsalvez/worktree-bootstrap-reflect-cli-fixes
- Merge pull request #157 from stevengonsalvez/fix/bootstrap-intel-mac-torch
- Merge pull request #158 from stevengonsalvez/worktree-fix-mouse-after-detach
- Merge pull request #165 from stevengonsalvez/worktree-ainb-hooks-plugin
- Merge pull request #170 from deepaks7n/fix/new-session-picker-scanner-cache
- chore(cli_burndown_tests): mark fixture-requiring tests as #[ignore]
- feat(ainb-core): add tmux-tests feature for opt-in TUI integration
- feat(metrics+ci): stats aggregator, dashboard sync, CI matrix, endpoint spec
- feat(plugin-types-sessions): wire schema for sessions.usage_data
- **ainb-core**: inject_session_reader_snapshot only touches sessions.usage_data
- **ainb-tui**: O(N+M) workspace dedup + raw-path fallback
- **ainb-tui**: address gemini-code-assist review on PR #130
- **ainb-tui**: cap Boss-mode load in manual refresh path
- **ainb-tui**: close help on Esc inside text inputs
- **ainb-tui**: decouple Boss + Interactive workspace loading
- **ainb-tui**: guard global char shortcuts in text inputs
- **ainb-tui**: include config_popup_state in text-input predicate
- **ainb-tui**: narrow config_popup gate to text-entry variants
- **bootstrap**: ensure ~/.local/bin on PATH and upgrade reflect on drift
- **bootstrap**: pin python 3.13 for reflect-kb install
- **bootstrap**: skip reflect-kb [graph] extra on Intel macOS
- **cli**: content-hash doc_id + --force + non-TTY guard for add
- **cli-usage**: inject host --format global into plugin argv
- **dashboard**: retry transport errors, MAC-derived id fallback, --window-days passthrough
- **entity-store**: tolerate null/missing fields in sidecar parser
- **host**: decouple event-poll cadence from app.tick() cadence
- **logging**: default filter to ainb=debug,warn so logs actually flow
- **logging**: exempt all short-lived CLI subcommands from JSONL file
- **logging**: skip JSONL file for high-frequency statusline hook
- **plugin**: reserve Esc for navigation, rebind burndown pop-state to Backspace
- **plugin-api**: remove duplicate serde_bytes line from merge auto-resolve
- **plugin-burndown**: bridge converter logs encode/decode failures
- **plugin-burndown**: drop dead enable_card_tests mod after rebase
- **plugin-burndown**: drop refresh_snapshot from cli_dispatch to break inline-event deadlock
- **plugin-burndown**: eager-spawn so usage_data subscription beats publisher
- **plugin-burndown**: empty-state copy reflects subscribe model
- **plugin-burndown**: project chip matches calls by resolved repo, not just raw folder
- **plugin-burndown**: rebuild activities + mcp_servers from raw calls on wire ingest
- **plugin-burndown**: rename FilterCacheEntry.filters_hash -> inputs_hash for clarity
- **plugin-burndown**: show scan-progress banner during mid-scan ingest
- **plugin-burndown**: surface wire-version mismatch via stderr
- **plugin-burndown**: sync ui.data before commit so Enter/X drill-down works
- **plugin-burndown**: wire period + provider filters into render (grafana-style global filter)
- **plugin-cts**: extend WASI floor to cover real-plugin imports
- **plugin-cts**: handle GatedBy::LogsRead in cap_declared
- **plugin-host**: add paths field to Manifest struct literals
- **plugin-host**: pass real allocated area to plugin render
- **plugin-host**: track inflight correlation-ids + drop late/sentinel replies
- **plugin-protocol**: encode bytes as base64 strings on the JSON wire
- **plugin-runtime**: auto-respawn eager plugins after exit
- **plugin-runtime**: cancel-safe stdout reader + plugin-publish fanout
- **plugin-runtime**: graceful shutdown to stop tokio drop-from-async panic
- **plugin-runtime**: honour SpawnMode::Eager at registration time
- **plugin-runtime**: priority key channel so Esc isn't queued behind chunked events
- **plugin-runtime**: re-sign staged plugin binaries on macOS
- **plugin-runtime**: route mark_render_dirty through inner.dirty + cover dirty-flag gate
- **plugin-sdk-rust**: dispatch plugin/handle_event inline to preserve order
- **reflect**: claude adapter detects plugin runtime to avoid dupe-fire
- **reflect**: clean dead conditional in filter_to_new + drop unused param
- **reflect**: harness-neutral log path + shared silent-fail helper + secret scrubbing
- **reflect**: hooks silent-fail on uncaught exception + breadcrumb to status line
- **reflect**: precompact hook emits empty stdout — codex schema compat
- **reflect**: resolve project dir via git-common-dir, not env-var cwd
- **reflect-discovery**: scan atomic memory files, not just MEMORY.md
- **reflect-plugin**: AGT tracks Agent spawn tool, drop false-positive TaskCreate
- **reflect-plugin**: ING parser accepts single AND double-quoted timestamps
- **reflect-plugin**: drop alpha-dimming, sparkline cells stay full-color
- **reflect-plugin**: re-enable auto-reflect after v3 state migration
- **reflect-plugin**: render sparklines with absolute height, not row-max
- **reflect-plugin**: resolve session JSONL via project root, not literal pwd
- **reflect-plugin**: use printf %s not %b to preserve \E bytes
- **reflect/adapters**: write full skill content, not pointer stub
- **scripts**: point search-learnings + reflect-status at reflect-kb CLI
- **session-reader,burndown**: review-pass — multi-spill chunker, robust flush, explicit pct cast
- **session-reader,burndown,types-sessions**: tail-chunk sessions and shell_commands across publish chunks
- **statusline**: resolve reflect plugin path dynamically
- **tui**: add other tmux multi-delete
- **tui**: expand sessions rail from visible control
- **tui**: fall back to workspaces when filtered cache is empty
- **tui**: handle Shift+i for Inbox on Linux tmux
- **tui**: honor checked rows on session delete
- **tui**: make Inbox discoverable — sidebar tile + always-on hint
- **tui**: restore mouse capture and bracketed paste on TUI resume
- **tui**: restore sessions mouse wheel scroll
- **tui**: source new-session repo picker from scanner cache
- **tui**: sync PresetManager in-memory cache on save_preset
- **usage-cli**: make timeout configurable + retain dispatch error
- **usage-export**: scrub stale CSVs + widen file-ext allow-list
- **usage-matrix**: snake_case CSV headers + wire-compat test + CR quote
- **usage-md**: truncate long project labels to 60 chars
- **workspace**: set lint group priority -1 to satisfy clippy
- test(cli-registry): update assertion 17->18 (claudecode + plugin)
- test(plugin-host): burndown subscribes to sessions.usage_data e2e
- test(tripwire): Phase 6f extension — full real two-plugin pipeline gate (4/4)

### Documentation
- Merge pull request #126 from stevengonsalvez/docs/plugin-spec-v2-subprocess
- Merge pull request #127 from stevengonsalvez/docs/screenshots-burndown-home
- Merge pull request #145 from stevengonsalvez/docs/website-brief-and-restructure
- chore(ainb-fleet): gitignore workflow runtime logs + local scratch
- **ainb-fleet**: fix fleet-needs skill refs to hangar workflow
- **ainb-fleet**: update needs skill + add dod5_needs runbook
- **compound-docs**: switch SKILL.md to reflect CLI
- **explain-to-me**: clarify here.now publish-slug semantics
- **knowledge**: add hooks-and-platform page with embedded SVGs
- **knowledge**: fix SVG diagrams rendering as raw XML on hooks page
- **plan**: Phase 7 — plugin runtime redesign (subprocess + JSON-RPC)
- **plans**: Phase 6 data-plane plan + interview-resolved spec
- **plugin-host**: document wasmi-sync deadlock in request_data rustdoc
- **plugin-session-reader**: clarify activity/mcp classification is consumer-owned
- **plugin-spec**: contract v1 + machine-readable contract.toml
- **plugins**: authoring guide for plugin developers
- **plugins**: consolidate plugin docs under docs/plugins/
- **plugins**: fix authoring trait example to match real SDK
- **plugins**: how to validate against ainb-plugin-cts
- **plugins**: refresh module docstring after usage_state removal
- **plugins**: rewrite for subprocess v2 contract, drop v1 wasm
- **plugins**: user-facing reference for the plugin family
- **reflect**: add mental-model section to README
- **reflect**: document live timeline dashboard in README
- **reflect**: drop legacy v1/v2 paths from canon skill instructions
- **reflect**: fix learnings dest path — flat documents/, not documents/learnings/
- **reflect**: standalone explainer + platform poster (here.now publishes)
- **reflect**: update timeline mockup for paired layout
- **reflect-kb**: clarify CLI vs plugin version streams
- **reflect-plugin**: drop legacy LEARNINGS_CLI refs from ingest SKILL.md
- **reflect-plugin**: shorten timeline drill-down hint to `reflect timeline`
- **screenshots**: add reproducible vhs tapes for home + burndown
- **screenshots**: regen home + burndown against real $HOME
- **skills**: add tmux-ui-tripwire project-local skill
- **toolkit**: drop global-learnings references
- add Starlight-compatible title frontmatter to every page
- add design brief for premium website
- capture mouse tui learnings
- relocate CLI, FAQ, and reflection docs into unified tree
- rewrite README for v0.1.1 + new docs/usage.md
- scaffold unified documentation tree
- swap stale README + plugin screenshots, drop orphans
- update README cross-links + architecture tree
- fix(plugin-burndown): rename FilterCacheEntry.filters_hash -> inputs_hash for clarity

### Other
- Merge pull request #110 from stevengonsalvez/perf/plugin-render-latency
- Merge pull request #117 from stevengonsalvez/perf/burndown-dimension-indices
- Merge pull request #118 from stevengonsalvez/perf/burndown-arc-data
- Merge pull request #162 from stevengonsalvez/worktree-fix-ainb-du-storm
- **ainb-core**: delete obsolete usage_event_bridge
- **ainb-core**: grep gate proves zero usage_data references in host
- **ainb-fleet**: gitignore workflow runtime logs + local scratch
- **catalog**: regenerate from filesystem
- **ci**: opt into Node 24 for GitHub Actions
- **cli_burndown_tests**: mark fixture-requiring tests as #[ignore]
- **deps**: add insta dev-dep for snapshot testing
- **explain-to-me**: drop /nano-banana-pro from augmentation list
- **fixtures**: deterministic generator for tripwire_keys
- **gitignore**: drop bare 'skills/' rule
- **logging**: janitor for stale empty JSONL files on startup
- **plugin**: bump reflect to 3.3.0
- **plugin**: bump reflect to 3.3.1
- **plugin**: bump reflect to 3.4.0
- **plugin-burndown**: TODO ref for legacy event arms tied to broker removal
- **plugin-host**: fix manifest_validate test fixture indent drift
- **plugin-session-reader**: trim dead helpers + gate with_roots to test
- **plugin-session-reader**: tune clippy lints + add toml dev-dep
- **preflight**: record monorepo consolidation pre-flight audit
- **reflect**: bump version 3.4.0 -> 3.4.1
- **reflect**: bump version 3.4.1 -> 3.4.2
- **reflect**: bump version 3.4.2 -> 3.4.3
- **reflect**: bump version 3.4.3 -> 3.5.0
- **reflect-plugin**: purge legacy LEARNINGS_CLI from config + docs
- **settings**: default permissions to bypassPermissions
- **settings**: forward-port live additions into toolkit
- **skill**: drop stale existing-tests.md manifest
- **skill**: tripwire return-path hard rule (#6)
- **sync-learnings**: orphans + plugin audit are informational, not gated
- **sync-learnings**: tidy output contract — one combined plan table
- **test_support**: silence post-6d unused warnings with TODO ref
- **tests**: drop obsolete test_reflect_workflow.py
- **tests**: retire dead in-tree analytics UI tests
- **usage**: expose report_json via test_support wrapper
- **workspace**: add xtask crate + cargo xtask alias
- **workspace**: register ainb-plugin-api + ainb-plugin-host members
- **workspace**: relocate ainb-tui sources to crates/ainb-core
- **workspace**: set explicit priority on clippy lint groups
- **workspace**: split Cargo.toml into workspace + ainb-core member
- remove dashboard track (no consumer exists)
- remove scratch/preflight-report.md (Phase 1 audit artifact)
- remove stale plans, design notes, issues, captured solutions
- remove team CLI track (deferred, dependent share command)
- scaffold reflect-kb repo
- sync learnings to packages
- update install URLs + plugin paths to monorepo form
- **host**: event-driven plugin render tick + 33 ms event-poll
- **plugin-burndown**: Arc<UsageData> between plugin and ui kills Enter-press clone
- **plugin-burndown**: cache filter_usage_data by (data_gen, filters)
- **plugin-burndown**: plug UsageIndices into the cached_filtered path
- **plugin-burndown**: pre-index calls by dimension + indexed filter path
- **plugin-burndown**: skip repo resolution when no project chip is active
- **plugin-runtime**: render-dirty flag on PluginHandle
- **tui**: remove du -sm storm from session recovery refresh
- **ainb-core**: drop dead in-tree usage CLI handlers
- **ainb-core**: drop unused analytics re-exports from models/mod.rs
- **cli/plugin**: split into module dir for 7d-cli subcommands
- **components**: delete in-tree usage UI + retire render parity test
- **core**: move plugin_runtime handle from App to AppState
- **core**: replace View enum with ScreenId across in-tree views
- **events**: drop AppEvent::Usage* variants and handlers
- **events**: drop handle_usage_keys + Analytics dispatcher
- **events**: stop firing host-side analytics load on screen entry
- **layout**: dispatch full-screen views through ScreenRegistry
- **live-window**: heartbeat at trace, transitions at debug
- **plugin**: move reflect from toolkit/packages/plugins/ to root plugins/
- **plugin**: update marketplace.json paths + in-plugin self-references
- **plugin-burndown**: drop rusqlite/blake3/bincode (item c)
- **plugin-burndown**: render to ratatui Buffer instead of Frame
- **plugin-host**: replace wasmi runtime with subprocess RuntimeHandle
- **plugin-runtime**: expose discover_filtered helper
- **reflect**: extract shared adapter helpers to base.py
- **session-reader**: drop dead popped_from tracking in chunker
- **state**: drop host-side analytics data load
- **state**: drop usage_state field + simplify tick_plugin_renders
- test(plugin-sdk-rust): assert handle_key ordering across 5-key burst


## [1.1.0] - 2026-05-10
### Added
- Merge pull request #80 from stevengonsalvez/feat/burndown-default-stats
- Merge pull request #81 from stevengonsalvez/feat/usage-aggregate-by-repo
- Merge pull request #82 from stevengonsalvez/feat/reflect-plugin-auto-wire-hooks
- Merge pull request #83 from stevengonsalvez/feat/burndown-branches-panel
- Merge pull request #84 from stevengonsalvez/feat/live-window-statusline
- Merge pull request #86 from stevengonsalvez/feat/statusline-discoverability
- Merge pull request #88 from stevengonsalvez/feat/statusline-cache-only
- **cli**: add ainb statusline subcommand for Claude Code hook
- **cli**: extract install_statusline() helper with idempotent settings.json merge
- **deps**: track reflect as a claude-plugins entry
- **init**: offer statusline install during ainb init wizard
- **layout**: global W shortcut to wire Claude Code statusline
- **layout**: top-bar live window display + red CTA when not wired
- **marketplace**: add Claude plugin marketplace manifest
- **reflect**: auto-wire SessionStart and PreCompact hooks via plugin.json
- **reflect**: stub v2 telemetry artifacts so they can't mislead future investigators
- **reflect**: vendor reflect-drain-bg.sh into plugin tree
- **scripts**: add update-externals.sh
- **scripts/update-externals**: wire reflect plugin, mcporter, graphify
- **skill/research**: consolidate prior-art check on recall preamble
- **skills**: sync-learnings filters orphans against filesystem-derived internal set
- **statusline**: add --cache-only flag for side-channel cache writes
- **statusline**: auto-migrate legacy ainb statusline command on install
- **toolkit**: add generate-catalog.sh + regenerate catalog.yaml
- **usage**: Budget panel live bars + W keybind triggers install
- **usage**: add render_branch_panel and ByBranch to UsagePanel enum
- **usage**: add repo_lookup helper to resolve cwd to upstream repo id
- **usage**: aggregate stats by upstream repo, fall back to folder
- **usage**: hoist enable card to top of Stats screen
- **usage**: live_window reader with three-tier fallback
- **usage**: make Burndown the default stats tab
- **usage**: wire ByBranch into Burndown grid/compact/stack/zoom layouts

### Fixed
- Merge pull request #75 from stevengonsalvez/fix/marketplace-skills-conflict
- Merge pull request #78 from stevengonsalvez/chore/release-tap-push
- Merge pull request #89 from stevengonsalvez/fix/usage-provider-switch
- **layout**: show 'r resume' instead of 'e restart' for stopped interactive sessions
- **layout**: show live widget when Tier1Cache flowing, regardless of source
- **live-window**: drop misleading $today price
- **manifest**: mcporter is openclaw/mcporter, not nanoclaw
- **marketplace**: canonical repo name is agents-in-a-box, not ai-coder-rules
- **marketplace**: drop skills array — plugin.json self-declares
- **reflect**: graceful skip + clear log when reflect-kb is missing
- **release**: push Homebrew formula to dedicated tap repo
- **skills**: re-templatize 16 synced skills that lost {{HOME_TOOL_DIR}}
- **skills**: use {{TOOL_DIR}} placeholder for tool-relative paths
- **statusline**: address PR #86 review findings
- **statusline**: drop chain-mode install path
- **statusline**: prune old settings.json backups
- **statusline**: write backup after settings.json lands
- **sync-learnings**: restore placeholders + fix reverse-interp regex
- **usage**: be honest about Gemini/Copilot stub state
- **usage**: force reparse on provider switch
- **usage**: split empty-state copy + drop budget cost render

### Documentation
- Merge pull request #74 from stevengonsalvez/docs/readme-homebrew-primary
- Merge pull request #77 from stevengonsalvez/docs/pr-b-reflect-readme
- **contributing**: qualify the test step — Jest suite has stale assertions
- **layout**: point top-bar CTA at the W shortcut
- **readme**: make Homebrew the primary install path
- **reflect**: add public-facing README with mermaid diagram
- **reflect**: mark settings-snippet.json as legacy/non-Claude fallback
- **reflect**: restructure settings-snippet around named opt-in variants
- **statusline**: clarify cache path is OS-specific
- clarify Claude Code scope in CTA copy and README

### Other
- Merge pull request #70 from stevengonsalvez/chore/sync-learnings-2026-05-04
- Merge pull request #76 from stevengonsalvez/chore/pr-a-hygiene
- Merge pull request #79 from stevengonsalvez/refactor/pr-c-toolkit-reorg
- Merge pull request #85 from stevengonsalvez/feat/cli-claudecode-namespace
- Merge pull request #87 from stevengonsalvez/chore/ci-greenup
- **ci**: remove Clippy and Cargo-Deny jobs
- **ci**: scope CI tests to library, skip Docker integration tests
- **claude-code-4.5**: add browser-harness + graphify references to CLAUDE.md
- **deps**: point external-dependencies.yaml at catalog.yaml
- **deps**: track kepano/obsidian-skills bundle (5 npx skills)
- **homebrew**: update formula to v1.0.0
- **manifest**: move caveman from npx-skills to claude-plugins
- **manifest**: track shape, mcporter, graphify; enumerate reflect plugin
- **repo**: add LICENSE, CONTRIBUTING, SECURITY; clean tracked junk
- **skills**: add tmux-message skill
- **skills**: sync home-newer skill edits back to packages
- **skills**: sync interview skill from user level
- cargo fmt across workspace
- **live-window**: move live_window::current() off the render thread
- **live-window**: tighten Tier 2 active-block parser to last 5h
- **cli**: namespace statusline under claudecode subcommand
- **externals**: switch reflect to claude marketplace install
- **toolkit**: rename clawdhub-skills→clawdhub, test→bootstrap.test.js, add per-dir READMEs


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
- Merge pull request #73 from stevengonsalvez/chore/drop-windows-release
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
- **release**: drop native Windows + Scoop from publishing pipeline
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
- **readme**: remove Scoop install path; mark native Windows unsupported
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
- **release**: prepare v1.0.0
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


## [1.0.0] - 2026-05-05
### Fixed
- Merge pull request #73 from stevengonsalvez/chore/drop-windows-release
- **release**: drop native Windows + Scoop from publishing pipeline

### Documentation
- **readme**: remove Scoop install path; mark native Windows unsupported


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
