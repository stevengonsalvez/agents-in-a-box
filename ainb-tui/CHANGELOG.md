# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
