# ainb Capability Inventory + Multica Gap Analysis

**Generated:** 2026-05-22  
**Branch:** feat/multica  
**Scope:** Map ainb's current capability surface against Multica's control plane to identify integration gap and leverage points.

---

## 1. ainb-as-it-stands: Shipped Capability Surface

### 1.1 TUI / CLI Binary (`ainb-tui/`)

Rust workspace at `/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_multica/ainb-tui/` containing 9 crates:

| Crate | Role | Status |
|---|---|---|
| `ainb-core` | 115-module main binary: TUI, CLI, session mgmt, git, tmux, models | Shipped |
| `ainb-plugin-runtime` | Tokio-backed plugin host: spawns subprocesses, routes JSON-RPC, manages lifecycle | Shipped |
| `ainb-plugin-protocol` | Wire protocol v2: Content-Length framing, manifest schema, method catalogue | Shipped |
| `ainb-plugin-sdk-rust` | Rust SDK for plugin authors | Shipped |
| `ainb-plugin-protocol` | Manifest TOML types + ABI versioning | Shipped |
| `ainb-plugin-burndown` | First-party usage analytics plugin | Shipped |
| `ainb-plugin-cts-v2` | Plugin compatibility test suite (host-side axes) | Shipped |
| `ainb-plugin-testkit` | Shared test harness utilities | Shipped |
| `ainb-plugin-types-sessions` | Wire types for session data passed to plugins | Shipped |
| `ainb-plugin-session-reader` | Snapshot reader for session data | Shipped |

**CLI commands** (`ainb-tui/crates/ainb-core/src/cli/`): `run`, `list`, `logs`, `attach`, `status`, `kill`, `auth`, `recover`, `config`, `git`, `favorites`, `init`, `presets`, `completion`, `tui` — all with `--format json` output.

**TUI screens** (from README + source): Home/dashboard, Session list, New session flow (local/clone/SSH/favorites), Agent picker, Model selector (Sonnet/Opus/Haiku), Log viewer, Stats/burndown analytics, Skills catalog viewer, Recovery, Setup wizard, Plugins panel.

**Providers** (`ainb-tui/crates/ainb-core/src/providers/`): `claude.rs`, `codex.rs`, `copilot.rs`, `gemini.rs` — plus `Kiro` variant in `SessionAgentType` enum.

**Config persistence**: `~/.agents-in-a-box/sessions.json` (session state), `~/.agents-in-a-box/plugins/` (plugin directory), `~/.claude/` (tool configs).

### 1.2 Toolkit (`toolkit/`)

Deployed to 9 AI tools (Claude Code, Codex, Copilot, Gemini, Amazon Q, Cursor, Cline, Roo, Clawdhub) via `bootstrap.js` + `create-rule.js`.

**Skills** (`toolkit/packages/skills/`): 71+ SKILL.md files. Full list in `toolkit/catalog.yaml`. Key clusters:

- Swarm orchestration: `swarm-create`, `swarm-join`, `swarm-inbox`, `swarm-status`, `swarm-shutdown`, `swarm-orchestration`, `swarm-agent-troubleshooting`
- Session management: `spawn-agent`, `health-check`, `session-info`, `session-metrics`, `session-summary`, `handover`, `recover-sessions`
- Issue/GitHub: `gh-issue`, `make-github-issues`, `do-issues`, `merge-agent-work`, `list-agent-worktrees`, `attach-agent-worktree`, `cleanup-agent-worktree`
- Knowledge: `reflect`, `research`, `research-cache`, `instincts`, `compound-docs`, `prime`, `sync-learnings`
- Planning: `plan`, `plan-tdd`, `plan-gh`, `implement`, `validate`, `workflow`
- Agent lifecycle: `autonomous-loops`, `cost-aware-pipeline`, `agent-ops`, `skill-creator`

**Agents** (`toolkit/packages/agents/`): 37 specialized agents across 7 categories:
- Universal: `backend-developer`, `frontend-developer`, `superstar-engineer`
- Orchestrators: `tech-lead-orchestrator`, `project-analyst`, `team-configurator`
- Engineering: 19 specialists (api-architect, code-reviewer, security-agent, etc.)
- Swarm: `swarm-leader`, `swarm-worker`
- Meta: `agentmaker`, `reflect`

**Workflows** (`toolkit/packages/workflows/single-agent/`): Structured delivery workflow definitions.

**Utilities** (`toolkit/packages/utilities/`): Claude Code hooks (`hooks/`), config templates, output styles, OpenClaw agent adapters.

### 1.3 Knowledge System

**reflect-kb** (`reflect-kb/`): Python package (`reflect-kb 0.1.x`) + `reflect` CLI. Stack: GraphRAG (nano-graphrag) + vector embeddings + QMD (Quick Markdown Documents). Subcommands: `init`, `add`, `search`, `reindex`, `stats`, `critical-patterns`, `generate-sidecars`, `metrics stats`, `timeline`. Installed via `uv tool install`. Entity sidecars in `.entities.yaml` files.

**reflect plugin** (`plugins/reflect/`): Claude Code plugin that wires `reflect-kb` CLI into the agent harness. Drains `~/.learnings/ingest/` queue, calls `reflect add`, rebuilds graph index. Version `3.x.x` (separate from CLI semver).

**Ingest path**: `~/.learnings/ingest/` queue → plugin hook → `reflect add` → GraphRAG index at `~/.claude/global-learnings/`.

### 1.4 Swarm Orchestration

`/swarm-create` skill backed by `swarm-lib.sh` in `toolkit/packages/utilities/`. Protocol:

- Reads Beads epic (`bd show <id>`)  
- Calls `bd swarm create <epic>` to generate task DAG  
- Creates `~/.agents-in-a-box/swarm/<team-id>/` directory with `team.json`, `inbox/`, `shared/`  
- Spawns tmux sessions: one leader (`<team-id>-leader`) + N workers (`<team-id>-agent-N`)  
- Two isolation modes: shared branch (default) or git worktrees per agent  
- Messaging via JSONL inbox files (not sockets, not HTTP)  
- Max 4 workers per swarm  
- Companion skills: `swarm-status`, `swarm-shutdown`, `swarm-inbox`, `swarm-orchestration`

### 1.5 Beads Issue Tracker

`bd` CLI: git-backed issue tracker. Key commands: `bd show`, `bd swarm create`, `bd ready --unassigned`, `bd update <id> --assignee <agent>`, `bd close <id>`, `bd dep`. State stored in git (not a server). BEADS_DIR environment variable for worktree isolation. No web UI.

### 1.6 Hooks System

Claude Code hooks at `toolkit/packages/utilities/hooks/`: `session_start.py`, `stop.py`, `subagent_stop.py`, `user_prompt_submit.py`, `pre_tool_use.py`, `post_tool_use.py`, `pre_compact.py`, `combined-statusline.js`, `statusline.py`, `cost_tracker.py`, `notification.py`.

These fire at Claude Code lifecycle events and feed data into ainb-tui's live window panel + reflect-kb's ingest queue.

### 1.7 MCP Integration

`mcporter` (external dependency): MCP server registry. `claude-peers`: inter-Claude-Code-instance messaging on same machine via the MCP protocol.

---

## 2. ainb's Mental Model

### Entities and where they live

```
~/.agents-in-a-box/
├── sessions.json           ← Workspace[] → Session[] (persisted state)
├── plugins/                ← Plugin binaries + manifest.toml per plugin
└── swarm/<team-id>/        ← Ephemeral swarm state (team.json, inbox/, shared/)

~/.claude/
├── skills/                 ← Skill definitions (SKILL.md per skill)
├── agents/                 ← Agent definitions (.md per agent)
├── global-learnings/       ← GraphRAG KB (reflect-kb's home)
├── CLAUDE.md               ← Global agent instructions
└── hooks/                  ← Lifecycle hooks

~/.learnings/ingest/        ← Ingest queue for reflect-kb
```

**Session** (`ainb-tui/crates/ainb-core/src/models/session.rs`): UUID-keyed, carries `SessionAgentType` (Claude/Codex/Gemini/Copilot/Shell/SSH/Kiro), `SessionMode` (Interactive/Boss), git worktree path, tmux session name, status, model, timestamps.

**Workspace** (`ainb-tui/crates/ainb-core/src/models/workspace.rs`): Name + path + Vec<Session> + optional ShellSession. Workspace = git repository on disk. No network identity, no user membership.

**Skill** (`ainb-tui/crates/ainb-core/src/models/skills.rs`): Name + description + `user_invocable` flag + source path. Parsed from SKILL.md frontmatter. No DB. No versioning. No server-side persistence.

**Plugin** (`ainb-tui/crates/ainb-plugin-protocol/src/manifest.rs`): TOML manifest with `[plugin]` (name, version, abi_version), `[capabilities]` (read_sessions, network allow-list, etc.), `[provides]` (screens, commands, CLI namespaces, snapshot topics), `[subscribes]`, `[lifecycle]` (spawn, idle_reap_secs).

**Usage** (`ainb-tui/crates/ainb-core/src/models/usage.rs`): `ProviderCall` parsed from Claude/Codex local JSONL histories. Aggregated into `UsageData` → `ActivityUsage`, `ModelUsage`, `ProjectUsage`, `SessionUsage` for TUI panels + CLI reports.

**Key observation**: ainb has NO unified server-side data model. All state is:
1. Local filesystem (sessions.json, worktrees, JSONL histories)  
2. tmux session names (runtime identity)  
3. JSONL inbox files (swarm messaging)  
4. Git commits (Beads issues)  
5. ~/.claude/global-learnings/ (knowledge graph)

There is no database, no network identity for workspaces, no user/team model, no real-time event bus. The TUI reads from filesystem + tmux, the CLI emits JSON from the same reads. Agents are distinguished only by their tmux session name and the AI tool type — they have no persistent server-side identity.

---

## 3. Multica's Mental Model

### Architecture

```
┌──────────────┐   WebSocket    ┌───────────────────────────────────┐
│ Web/Desktop/ │◄──────────────►│  Go HTTP Server (Chi + sqlc)      │
│ Mobile App   │   REST/JSON    │  server/internal/handler/         │
└──────────────┘                │                                   │
                                │  ┌─────────────────────────────┐  │
┌──────────────┐   Poll/3s      │  │  Postgres (pgvector/pg17)   │  │
│ Daemon       │◄──────────────►│  │  workspace, agent, issue,   │  │
│ (Go binary)  │                │  │  skill, squad, runtime,     │  │
└──────────────┘                │  │  comment, autopilot ...     │  │
       │                        │  └─────────────────────────────┘  │
       │ spawns                 │                                   │
       ▼                        │  realtime/hub.go (WS broadcast)   │
┌──────────────┐                └───────────────────────────────────┘
│ Agent CLI    │
│ (claude,     │
│  codex, etc) │
└──────────────┘
```

**Core entities** (Postgres-backed, multi-tenant via `workspace_id`):

- **Workspace** — slug-keyed namespace; all other entities scoped to it; settings, repos, issue prefix
- **Agent** — first-class DB record: UUID, name, description, instructions (system prompt), model, `runtime_id`, `custom_env`, `custom_args`, `mcp_config`, `skills[]`, visibility, thinking_level
- **AgentRuntime** — represents a daemon instance's detected CLI capability: provider, status, `last_seen_at`, heartbeat-driven alive/dead
- **Issue** — Linear-style: `status`, `priority`, `assignee_type` + `assignee_id` (polymorphic — can be agent OR human), parent_issue_id, project_id, labels
- **Skill** — server-stored SKILL.md: name, description, content (full markdown body), `created_by`, versioned by UpdatedAt; agents carry `[]AgentSkillSummary`
- **Squad** — group of agents (and humans) under a `leader_id`; work assigned to squad → leader routes to member
- **Comment** — threaded on issues; `author_type` (human or agent), reactions, attachments
- **Autopilot** — scheduled/webhook-triggered: `execution_mode`, `assignee_type` (agent|squad), cron trigger, issue_title_template
- **ChatSession** — direct human↔agent conversation channel (separate from issue comments)
- **Dashboard** — aggregate usage queries: per-(date,model), per-(agent,model), per-agent runtime + task counts

**Real-time layer** (`server/internal/realtime/`): Redis-backed sharded stream relay + WebSocket hub. All mutations broadcast events to connected clients. No polling required from UI.

**Daemon lifecycle**: Polls server every 3s for claimed tasks → creates isolated workspace dir → spawns agent CLI → streams output back → heartbeats every 15s. Daemon detects installed CLIs at startup, registers `AgentRuntime` per (workspace × CLI). GC cleans up task directories (done/cancelled TTL 24h, orphan TTL 72h, artifact-only cleanup for node_modules/.next/.turbo).

**Skills** are stored server-side in Postgres (not just on-disk markdown). They can be attached to agents. The skills-lock.json in the repo root records external skill hashes for reproducibility.

**Autopilot** = scheduled or webhook-triggered issue creation + agent dispatch. Handles cron + webhook triggers, squad routing.

**Squads** = stable named groups. Assign work to `@FrontendTeam` → leader agent decides which member picks it up. Persistent across sessions.

---

## 4. The Mapping Table

| Multica capability | ainb equivalent | Coverage % | Gap |
|---|---|---|---|
| **Daemon** — long-running background process that polls for tasks, spawns agent CLIs | `swarm-lib.sh` + tmux sessions via `/swarm-create` | 25% | ainb has no persistent daemon process; swarms are ad-hoc tmux sessions spawned per-epic, not persistent pollers. No heartbeat, no server registration |
| **CLI↔server auth** — `multica login` OAuth + PAT, 90-day tokens, `multica auth status` | `ainb auth` (claude OAuth, API key config) | 40% | ainb auth is per-provider (Anthropic-only). No multi-user workspace auth, no PAT concept |
| **AgentRuntime registry** — daemon registers detected CLIs; server tracks alive/dead via heartbeat | None native; swarm team.json carries agent metadata | 5% | ainb has no concept of registered runtimes. Tool availability is checked ad-hoc at session launch |
| **Issue tracker** — Linear-style issues: status, priority, polymorphic assignee, parent_issue, project, labels | Beads (`bd` CLI) — git-backed issues with similar lifecycle | 55% | Beads: no web UI, no real-time sync, no polymorphic assignee (no "agent" assignee type baked in), no reactions/attachments |
| **Agent-as-assignee** — assign issue to an agent identity, not a human | None (Beads assigns to string names, not typed entities) | 10% | No first-class agent identity in ainb's issue tracker |
| **Agent profiles** — DB record: instructions, model, custom_env, custom_args, mcp_config, skills list, visibility | Session presets + agent YAML definitions in toolkit | 20% | ainb "agents" are .md files (instructions only). No runtime env config, no attached skills list, no visibility control per agent |
| **Squads** — named group with leader agent; `@FrontendTeam` dispatch | `/swarm-create` leader+worker model | 35% | ainb swarms are ephemeral per-epic. No persistent named squad. No stable routing. Squad = "team for this epic only" |
| **Multi-workspace** — workspace-scoped isolation, slug-based, team membership | Workspace = git repo on disk (no network identity) | 10% | ainb workspaces are local paths. No user membership, no slug, no multi-user sharing |
| **Web dashboard** — Next.js board view: issues, agents, activity, settings | None (TUI only) | 0% | Full gap. ainb is terminal-only |
| **Desktop app** — Electron with tab isolation per workspace | None | 0% | Full gap |
| **Mobile app** — Expo/React Native iOS | None | 0% | Full gap |
| **WebSocket real-time events** — all mutations broadcast to UI instantly | None | 0% | ainb has no event bus. TUI polls filesystem + tmux |
| **Skill server storage** — skills stored in Postgres, attached to agents, versioned | Skills are on-disk SKILL.md files scanned at startup | 15% | ainb skills live in `~/.claude/skills/`. No DB, no versioning, no agent-attachment concept in data layer |
| **Skills compound / reuse** — every solved task can be published as a reusable skill | `/sync-learnings` + reflect-kb for knowledge; skill files are static markdown | 30% | ainb captures learnings in GraphRAG KB (deeper than Multica). No per-task→skill promotion workflow |
| **Autopilot / scheduled runs** — cron + webhook triggers that create issues + dispatch agents | None | 0% | ainb has no scheduling primitives. Closest: autonomous-loops skill (manual loop, not cron) |
| **Webhook triggers** — POST endpoint creates issue + assigns to agent automatically | None | 0% | Full gap |
| **Comment threading** — agent posts comments on issues, reactions, attachments | None (Beads has no comments) | 0% | Beads issues have status/description only. No threaded discussion |
| **@mention routing** — `@agent-name` in comments triggers agent response | `claude-peers` MCP (same-machine inter-instance messaging) | 15% | claude-peers is machine-local, not server-routed. No @mention in issue tracker |
| **Task lifecycle management** — enqueue → claim → start → complete/fail with retries | Beads: `bd update --status` manually; swarm workers call `bd close` | 25% | ainb lifecycle is convention-based (skill instructions). No automatic retry, no orphan recovery, no formal FSM enforced by server |
| **Cloud runtime** — run agents on Multica-hosted infra, not local machine | None | 0% | ainb agents always run locally (tmux on local or SSH-remote box) |
| **Runtime profiles** — multiple daemon configs for different server environments | `ainb config` profiles (per-tool auth), `--profile` flag concept | 20% | ainb profiles are tool-auth oriented, not "which server environment" |
| **GC / workspace cleanup** — automatic cleanup of task dirs after TTL | Manual via `cleanup-agent-worktree` skill | 20% | ainb has a skill that cleans up; no automated daemon-driven GC |
| **Usage analytics dashboard** — per-(date,model,agent) token + cost; scoped to workspace | `ainb` TUI burndown panel + `ainb usage` CLI — deep token analytics | 70% | ainb analytics is richer per-session (5h window, 7d window, cost, cache hits, model breakdown). Gap: no workspace/team-level rollup across multiple users |
| **Multi-provider token tracking** — cost attribution per agent across providers | Claude + Codex JSONL parsing; Copilot/Gemini partial | 50% | ainb tracks Claude deeply (OAuth-grade), Codex via JSONL. No Gemini/Copilot cost tracking |
| **Agent activity feed** — timeline of agent actions visible to team | None | 0% | Full gap. No shared activity timeline |
| **Issue comments from agents** — agent posts progress updates to issue thread | None | 0% | Agents have no issue-comment write path in ainb |
| **Blocker reporting** — agent can raise a blocker on an issue | `/swarm-inbox` JSONL messages to leader | 20% | ainb swarm workers can send messages to leader via inbox files. Not stored in issue tracker, not visible to humans outside tmux |
| **Dashboard analytics API** — `/api/dashboard/usage/daily`, `/by-agent`, `/agent-runtime` | `ainb usage --format json` | 45% | ainb has per-session JSON output. No workspace aggregation API endpoint |
| **Workspace repos config** — workspaces declare which git repos agents can clone | None (agents run in existing local dirs) | 0% | ainb does not have a "workspace declares repos" concept. Git worktrees are created from any local or clonable repo |
| **Semantic inactivity detection** — Codex timeout based on output changes, not wall time | None | 0% | ainb sessions use fixed timeouts. No semantic output monitoring |
| **Agent thinking level** — persist reasoning effort per agent per model | `--model` flag at session launch | 10% | ainb selects model but no per-agent persisted thinking level configuration |
| **Docker self-host** — full docker-compose.yml + Dockerfile for server deployment | `ainb-tui/docker/` for local container sessions | 20% | ainb's docker is for running sessions inside containers, not for hosting ainb itself |
| **Skills marketplace / lock file** — `skills-lock.json` with source + hash for external skills | `toolkit/external-dependencies.yaml` + `catalog.yaml` | 40% | ainb has catalog + external deps list but no hash-verified lock file mechanism for skill versions |

---

## 5. Where ainb is AHEAD of Multica

### 5.1 GraphRAG Knowledge Base (reflect-kb)
Multica has no knowledge persistence layer. ainb's `reflect-kb` provides:
- GraphRAG with community detection over accumulated learnings
- Entity sidecars (`.entities.yaml`) for structured knowledge extraction
- Hybrid vector + graph search (`reflect search`)
- Cross-session, cross-project recall
- `reflect timeline` for per-metric drill-down
- `/prime` + `/research` skills that inject recalled knowledge at session start

This is a significant moat. Multica skills are per-workspace CRUD (create, attach to agent, invoke). ainb's knowledge compounds across every session ever run, across projects, and is retrievable semantically.

### 5.2 Plugin Host v2 (ainb-plugin-runtime)
Multica has no plugin architecture. ainb ships:
- Native subprocess + JSON-RPC 2.0 over Content-Length stdio
- TOML manifest with ABI versioning (`abi_version`)
- Capability gating at runtime (not build time) via `-32001` error codes
- Snapshot pub/sub between host and plugins (`[subscribes]`/`[provides]`)
- Lazy/eager spawn + idle reap policy in manifest
- Rust SDK for plugin authors
- Full CTS (compatibility test suite) with both plugin-side and host-side axes
- First-party burndown plugin (`ainb-plugin-burndown`)

### 5.3 Deep Token Analytics
ainb's usage analytics is far deeper than Multica's dashboard:
- Reads Claude OAuth-grade rate-limit windows (5h burn, 7d window) when statusline is wired
- Per-session token attribution: input, cache read/write, output, cost
- Per-project, per-model, per-day breakdowns in TUI
- `UsagePeriod` variants: Today, Week, 30d, LastNDays(n), SpecificMonth, SpecificQuarter, YearToDate
- Inline optimization hints in burndown panel
- CLI `ainb usage` with `--format json` for all periods

### 5.4 Swarm Orchestration (toolkit)
Multica Squads are a routing layer (assign to squad → leader picks member). ainb swarms are full execution orchestrators:
- `bd swarm create <epic>` generates a topological task DAG with dependency tracking
- Leader agent actively orchestrates: reads `bd ready --unassigned`, dispatches workers, monitors progress, handles blockers
- Worker agents are real Claude Code instances in tmux, not API calls
- JSONL inbox protocol for structured inter-agent messaging
- Two isolation modes: shared branch (fast, no merge) or git worktrees (full isolation)
- Watchdog daemon concept (`swarm-status`, `swarm-agent-troubleshooting`)
- Complete companion skill set (7 skills)

### 5.5 Multi-Tool Portability (toolkit)
Multica supports multiple agent CLIs at the daemon level (Claude, Codex, Copilot, etc.) but the platform logic is agnostic. ainb's toolkit is **written once, deploys to 9 AI tools** via `bootstrap.js`:
- Skills, agents, workflows deploy identically to Claude Code, Codex, Copilot, Gemini, Amazon Q, Cursor, Cline, Roo, Clawdhub
- `create-rule.js --tool=<name>` handles per-tool format differences
- Config hooks adapt to each tool's lifecycle events

Multica is a platform that runs agents. ainb augments the agents themselves regardless of which platform they run on.

### 5.6 TUI Depth and CLI Parity
ainb's Rust TUI ships 15 top-level CLI commands with `--format json` parity on every subcommand. Every TUI feature is scriptable. Multica's CLI (`multica`) is focused on daemon/auth/workspace management — it has no analytics, no session log streaming, no worktree commands.

### 5.7 Git Worktree Isolation
ainb has first-class git worktree management (`ainb-tui/crates/ainb-core/src/git/`): `diff_analyzer.rs`, `operations.rs`, `remote_repo_manager.rs`, `repo_source.rs`, `workspace_scanner.rs`, `worktree_manager.rs`. Multica's daemon creates a directory per task; it does not manage git worktrees natively.

### 5.8 Claude Streaming / Direct API Integration
`ainb-tui/crates/ainb-core/src/claude/`: `client.rs`, `streaming.rs`, `types.rs` — ainb has a direct Anthropic API client with streaming support, separate from spawning the Claude Code CLI. This enables "Boss mode" (non-interactive prompt execution) and future programmatic agent invocation.

---

## 6. Where Multica is AHEAD of ainb

| Gap | Multica has | ainb gap | Effort to close |
|---|---|---|---|
| **Persistent daemon** | `multica daemon` — Go binary, background poll/heartbeat, runtime registration, GC, orphan recovery | No daemon; swarms are manual tmux sessions | L |
| **Server-side agent identity** | `Agent` DB record: instructions, model, skills[], custom_env, mcp_config, visibility | Agent = .md file with instructions only | M |
| **Web UI** | Next.js board: issues, agents, activity, settings, comments | No web UI | XL |
| **Desktop app** | Electron with tab isolation | No desktop app | XL |
| **Mobile app** | Expo/React Native | No mobile | XL |
| **WebSocket real-time** | Redis-backed sharded stream relay + WS hub | Filesystem polling | L |
| **Multi-user / team** | Workspace membership, roles, invites | Single-user only | L |
| **Agent issue comments** | Agent posts progress comments to issue thread, visible to team | No agent→issue write path | M |
| **Squads (stable routing)** | Named squads persist across tasks; `@SquadName` dispatch | Swarms are ephemeral per-epic | M |
| **Autopilot / cron triggers** | Cron + webhook → auto-create issue + dispatch agent | No scheduling | M |
| **Skill server storage** | Skills in Postgres, attach to agent, version history | Skills are local .md files | M |
| **Cloud runtimes** | Multica-hosted agent execution | Local/SSH only | XL |
| **@mention routing** | `@agent-name` in comment triggers agent | No server-side mention resolution | M |
| **Task orphan recovery** | Daemon reports orphaned tasks on restart; server retries | No orphan detection | S |
| **Workspace repo config** | Workspace declares allowed repos for agent cloning | No workspace-level repo policy | S |
| **Semantic inactivity timeout** | Codex output-change detection, not wall time | Wall-clock timeouts only | S |
| **Multi-user analytics** | Dashboard aggregates across all agents in workspace | Per-user local analytics only | M |
| **Issue reactions/attachments** | Full emoji reactions + file attachments on issues | Beads has none | S |

---

## 7. Hybrid Model Proposal

**Concept**: ainb absorbs Multica's control plane as an optional layer, keeping its terminal-native core intact while gaining server-side identity, real-time coordination, and web visibility.

```
┌───────────────────────────────────────────────────────────────────┐
│                        ainb hybrid                                │
│                                                                   │
│  ┌─────────────────┐    ┌──────────────────────────────────────┐  │
│  │   ainb TUI/CLI  │    │   ainb server (new, optional)        │  │
│  │   (Rust, local) │◄──►│   Go or Rust HTTP + WS              │  │
│  │                 │    │   Postgres (multi-tenant)            │  │
│  │  Sessions       │    │   Entities: Agent, Issue, Skill,     │  │
│  │  Worktrees      │    │   Squad, Runtime, Workspace, Comment │  │
│  │  Burndown       │    │                                      │  │
│  │  Plugin host    │    │   Web UI (Next.js) — optional        │  │
│  └─────────────────┘    └──────────────────────────────────────┘  │
│           │                          ▲                            │
│           ▼                          │                            │
│  ┌─────────────────┐    ┌────────────┴─────────────────────────┐  │
│  │  ainb daemon    │    │  reflect-kb (knowledge layer)        │  │
│  │  (new, optional)│    │  GraphRAG + QMD — unique to ainb     │  │
│  │  polls server   │    │  cross-session, cross-workspace      │  │
│  │  spawns agents  │    │  skills compound via real learning   │  │
│  │  heartbeats     │    └──────────────────────────────────────┘  │
│  └─────────────────┘                                              │
│           │                                                       │
│  ┌────────┴──────────────────────────────┐                        │
│  │  Agent CLIs (Claude, Codex, Gemini,   │                        │
│  │  Copilot, Kiro, ...)                  │                        │
│  └───────────────────────────────────────┘                        │
└───────────────────────────────────────────────────────────────────┘
```

**Key design decisions for the hybrid:**

1. **Daemon is additive, not required.** `ainb` today works without a server. Adding `ainb daemon start` optionally registers with a server and enables team features. Solo devs keep the zero-config local experience.

2. **Server entities extend, not replace, local models.** `Session` (local) maps to `AgentRuntime` (server) by daemon registration. `Beads issue` (git-backed) maps to `Issue` (server DB) by sync. Skills stay on-disk but gain server-side indexing.

3. **reflect-kb becomes the skills compounding layer that Multica lacks.** When an agent solves a problem, the reflect hook promotes the learning to both: (a) the GraphRAG KB (ainb's current path) AND (b) a server-side Skill record visible to the team. ainb gets Multica's skill-compound feature while retaining the GraphRAG depth Multica has no equivalent for.

4. **Swarm ↔ Squad mapping.** An ainb swarm becomes a Squad when a server is present: `bd swarm create` creates both a local team.json AND a `POST /api/squads` record. The leader agent is registered as the Squad's `leader_id`. This gives team members visibility into swarm progress without requiring a web UI.

5. **Plugin host as the server extension point.** Instead of shipping a full Go server clone, ainb could expose a plugin capability that bridges to Multica's server API. The plugin manifest's `[capabilities]` / `network` allow-list gates the API endpoint; the plugin handles auth, entity sync, and WS connection. Multica becomes a runtime dependency, not something ainb reimplements.

6. **Web UI strategy: leverage, not build.** Given Multica is open-source (MIT, 31k stars), ainb could embed Multica's web frontend rather than building from scratch, contributing upstream for ainb-specific views (reflect-kb dashboard, plugin panel, burndown with OAuth-grade windows).

---

## 8. Top 5 Leverage Points

### L1 — Thin Daemon Wrapper Over Existing tmux/swarm Infrastructure (20% work, 60% value)

ainb already has:
- `swarm-lib.sh` that spawns tmux sessions
- `swarm-create` skill that reads Beads epics, creates team.json, spawns leader + workers
- Session state in `sessions.json`

**What to add**: A minimal Go or Rust daemon that: (a) starts on `ainb daemon start`, (b) reads `sessions.json` + team.json as its runtime state, (c) polls a lightweight HTTP endpoint (or git repo) for new Beads issues assigned to "agents", (d) calls existing `/swarm-create` logic to dispatch. No new data model. No Postgres. Reuses all existing primitives.

This gives: background execution, restart-safe sessions, visible to team via the polling endpoint. Estimated effort: **M** (2-3 weeks for a working v0).

### L2 — Beads → Issue Sync (10% work, 50% value)

Beads already has all the issue primitives (status, priority, assignee, parent, labels). What it lacks is: web visibility and real-time sync.

**What to add**: A `bd sync` command that pushes Beads issues to a Multica-compatible server endpoint (or a lightweight HTTP server ainb ships). Since Multica is self-hostable, an `ainb setup server` could spin up a Multica instance locally, and `bd sync` would POST issues to it. Team gets the web board for free. Beads keeps its git-backed source of truth (offline-first, no DB dep for devs who don't want it).

Estimated effort: **M** (write Beads→Multica sync adapter).

### L3 — Agent Profiles Backed by Plugin Manifests (15% work, 40% value)

ainb already has the plugin manifest system with `abi_version`, `capabilities`, `provides`, `[lifecycle]`. These are richer than Multica's agent profiles in some ways (capability gating, snapshot pub/sub).

**What to add**: Extend the plugin manifest (or create a sibling `agent.toml`) to carry: `instructions`, `model`, `custom_env`, `custom_args`, `mcp_config`, `skills[]`. The TUI's agent picker reads from `~/.agents-in-a-box/agents/` instead of just `~/.claude/agents/`. This makes agent profiles first-class persisted objects without requiring a server.

When a server IS present, the daemon syncs `agent.toml` → `POST /api/agents`.

Estimated effort: **S** (1 week — mostly schema + TUI picker extension).

### L4 — reflect-kb as Multica Skills Layer (15% work, 80% value for knowledge)

Multica's skill compound is "every solution becomes a reusable skill for the team." ainb has a deeper version: reflect-kb captures learnings with entity extraction and graph search.

**What to add**: A post-task hook that:
1. Calls existing `reflect add <learning>` (already works)
2. Optionally calls `POST /api/skills` to publish the learning as a server-side skill attached to the agent that ran the task

This closes Multica's skill-compound story entirely while keeping ainb's GraphRAG depth. The hook can be a 20-line addition to `plugins/reflect/`.

Estimated effort: **S** (1 week — hook extension + API call).

### L5 — ainb CLI as Multica CLI Front-End (5% work, 30% value)

Multica's CLI (`multica daemon`, `multica issues`, `multica agents`) overlaps significantly with `ainb`'s existing CLI surface. Rather than replacing one with the other, ainb could add a `ainb server` subcommand namespace that wraps Multica API calls with ainb's `--format json` idiom.

```
ainb server issues list            → GET /api/issues (workspace-scoped)
ainb server agents list            → GET /api/agents
ainb server daemon status          → GET /daemon/status
```

Since ainb already has 15 CLI commands with JSON output, adding server-scoped subcommands is mechanical and reuses the existing CLI registry (`ainb-tui/crates/ainb-core/src/cli/registry.rs`). This is the 20% work that unlocks scripting against the Multica server with ainb's established CLI UX.

Estimated effort: **S** (1-2 weeks — API client + command wiring, no new data model).

---

## Key File Paths Reference

| Component | Path |
|---|---|
| Main crate | `/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_multica/ainb-tui/crates/ainb-core/` |
| Session model | `ainb-tui/crates/ainb-core/src/models/session.rs` |
| Workspace model | `ainb-tui/crates/ainb-core/src/models/workspace.rs` |
| Skills model | `ainb-tui/crates/ainb-core/src/models/skills.rs` |
| Usage model | `ainb-tui/crates/ainb-core/src/models/usage.rs` |
| Plugin protocol | `ainb-tui/crates/ainb-plugin-protocol/src/manifest.rs` |
| Plugin runtime | `ainb-tui/crates/ainb-plugin-runtime/src/runtime.rs` |
| Plugin registry | `ainb-tui/crates/ainb-plugin-runtime/src/registry.rs` |
| Providers | `ainb-tui/crates/ainb-core/src/providers/` |
| CLI commands | `ainb-tui/crates/ainb-core/src/cli/` |
| Swarm skill | `toolkit/packages/skills/swarm-create/SKILL.md` |
| reflect-kb | `reflect-kb/src/reflect_kb/` |
| reflect plugin | `plugins/reflect/` |
| Hooks | `toolkit/packages/utilities/hooks/` |
| Toolkit catalog | `toolkit/catalog.yaml` |
| Multica daemon | `.agents/research/multica/server/internal/daemon/daemon.go` |
| Multica agent handler | `.agents/research/multica/server/internal/handler/agent.go` |
| Multica issue handler | `.agents/research/multica/server/internal/handler/issue.go` |
| Multica skill handler | `.agents/research/multica/server/internal/handler/skill.go` |
| Multica squad handler | `.agents/research/multica/server/internal/handler/squad.go` |
| Multica realtime | `.agents/research/multica/server/internal/realtime/` |
| Multica autopilot | `.agents/research/multica/server/internal/handler/autopilot.go` |

