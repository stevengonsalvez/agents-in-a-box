# CodeBurn CLI Parity Research

## Question

What does CodeBurn provide at the CLI layer, what does `optimize` do, and what must AINB implement in Rust to cover the same behavior while skipping only macOS menubar install/app support?

## Sources

- CodeBurn repository clone: `/tmp/codeburn-research`
- Main CLI: `/tmp/codeburn-research/src/cli.ts`
- Parser and providers: `/tmp/codeburn-research/src/parser.ts`, `/tmp/codeburn-research/src/providers`
- Optimize: `/tmp/codeburn-research/src/optimize.ts`
- Export: `/tmp/codeburn-research/src/export.ts`
- Plan and config: `/tmp/codeburn-research/src/config.ts`, `/tmp/codeburn-research/src/plans.ts`, `/tmp/codeburn-research/src/plan-usage.ts`
- Compare and yield: `/tmp/codeburn-research/src/compare-stats.ts`, `/tmp/codeburn-research/src/compare.tsx`, `/tmp/codeburn-research/src/yield.ts`
- AINB CLI/TUI: `ainb-tui/src/cli/mod.rs`, `ainb-tui/src/main.rs`, `ainb-tui/src/models/usage.rs`, `ainb-tui/src/components/usage.rs`, `ainb-tui/src/config/mod.rs`

## Executive Summary

Full CLI parity means more than a Burndown tab. CodeBurn provides a local analytics CLI with:

- Reports: interactive dashboard and JSON report.
- Status: compact terminal and JSON summaries.
- Export: CSV folder export and JSON export.
- Usage periods: today, last 7 days, last 30 days, current month, and all.
- Custom date range flags on report: `--from YYYY-MM-DD`, `--to YYYY-MM-DD`.
- Provider and project filters.
- Plan/budget tracking.
- Currency and model alias settings.
- Optimize findings.
- Model comparison.
- Git-correlated yield analysis.

Menubar install/app support is the only explicit exclusion. For AINB, the clean command shape is a new `ainb usage ...` namespace so existing top-level `ainb status <workspace>` keeps its current session-management meaning.

## CodeBurn CLI Surface

### Global

- Binary: `codeburn`.
- Global `--verbose` enables warning output through `CODEBURN_VERBOSE=1`.
- Startup loads config, model aliases, pricing, and currency before command execution.

### Periods

Preset periods:

- `today`
- `week` / "Last 7 Days"
- `30days`
- `month`
- `all`

CLI `all` is last six months in `cli.ts`. Dashboard internal `all` uses epoch/all-time. Rust implementation should choose one intentionally; for parity with CLI commands, use last six months for CLI and document TUI all-time if AINB wants the richer view.

### Filters

- `--provider <provider>` exact provider ID match.
- `--project <name>` repeatable include filter.
- `--exclude <name>` repeatable exclude filter.
- Project matching is case-insensitive substring against sanitized project name and raw path.
- Include is applied before exclude; exclude wins when both match.
- `--from` and `--to` accept local `YYYY-MM-DD`; missing `from` means earliest data, missing `to` means today; `to` is inclusive through `23:59:59.999`.

### Commands To Port

Recommended AINB namespace:

```bash
ainb usage report
ainb usage status
ainb usage today
ainb usage month
ainb usage export
ainb usage optimize
ainb usage compare
ainb usage yield
ainb usage plan
ainb usage currency
ainb usage model-alias
```

CodeBurn command inventory:

- `report`: dashboard by default, `--format json` for structured output. Supports period, custom range, provider, project include/exclude, refresh.
- `today`: dashboard pinned to today. Supports provider, project include/exclude, refresh, JSON.
- `month`: dashboard pinned to current month. Supports provider, project include/exclude, refresh, JSON.
- `status`: compact terminal/JSON summary for today and month. CodeBurn also has `menubar-json`; AINB can omit menubar schema unless useful for automation.
- `export`: writes Today, 7 Days, and 30 Days exports in CSV or JSON. Supports provider and project include/exclude.
- `plan`: show/set/reset budget plan. Supports JSON show output, provider scope, custom monthly USD, reset day 1-28.
- `currency`: show/set/reset display currency and optional symbol override.
- `model-alias`: list/add/remove pricing aliases.
- `optimize`: read-only waste analysis.
- `compare`: interactive model comparison; non-TTY prints an explanatory message.
- `yield`: experimental git correlation between AI spend and commits.
- `menubar`: excluded by request.

## Output Schemas

### Report JSON

`report|today|month --format json` outputs:

- `generated`
- `currency`
- `period`
- `periodKey`
- `overview`
- `daily`
- `projects`
- `models`
- `activities`
- `tools`
- `mcpServers`
- `shellCommands`
- `topSessions`
- optional `plan`

Overview includes cost, calls, sessions, cache hit, and input/output/cache token totals. Activity rows include turns, cost, edit turns, one-shot turns, and one-shot rate.

### Status JSON

`status --format json` outputs:

- `currency`
- `today { cost, calls }`
- `month { cost, calls }`
- optional `plan`

### Export

CSV export writes a directory, not one flat CSV. Files:

- `.codeburn-export`
- `README.txt`
- `summary.csv`
- `daily.csv`
- `activity.csv`
- `models.csv`
- `projects.csv`
- `sessions.csv`
- `tools.csv`
- `shell-commands.csv`

CSV escapes formula-injection prefixes by adding a leading apostrophe for cells starting with tab, carriage return, `=`, `+`, `-`, or `@`.

JSON export writes:

- `schema: "codeburn.export.v2"`
- `generated`
- `currency`
- `summary`
- `periods`
- `projects`
- `sessions`
- `tools`
- `shellCommands`

AINB should port the shape but can use `ainb.export.v1` if exact CodeBurn schema compatibility is not a goal.

## Usage Derivation

CodeBurn parses local provider histories and normalizes them into projects, sessions, turns, calls, tools, shell commands, MCP calls, model stats, and cost.

Core providers in registry include Claude, Codex, Copilot, Droid, Gemini, Kilo Code, Kiro, OpenClaw, Pi, OMP, Qwen, Roo Code, plus lazy Cursor/OpenCode/Cursor Agent providers.

AINB already shows Claude/Codex/Gemini/Copilot provider labels, but only Claude currently has real data. A practical Rust port should implement Claude and Codex first, then add other providers behind the same trait.

Important parse behavior:

- Claude reads `~/.claude/projects` or `CLAUDE_CONFIG_DIR`.
- Codex reads `CODEX_HOME` or `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
- Codex sources are validated by `session_meta` and `originator` starting with `codex`.
- Codex tool normalization maps shell/file/agent operations into CodeBurn's common tool names.
- Codex token counts may be cumulative; when `last_token_usage` exists use it, otherwise diff cumulative totals and skip duplicates.
- OpenAI cached tokens are included in input; CodeBurn subtracts cached tokens from input for cost math.

Activity classification is deterministic:

- Tool-first categories: planning, delegation, coding, exploration, git, build/deploy, MCP, task management.
- Prompt keyword refinement detects debugging, feature work, refactoring, testing, brainstorming, research, and conversation.
- Retry counting treats edit -> bash -> edit cycles as retries.
- One-shot rate is edit turns with zero retries.

## Optimize Findings

`codeburn optimize` is a read-only waste analyzer. It scans Claude Code sessions and local Claude configuration, emits ranked findings, estimates token/USD savings, and prints copy-paste fixes. It does not mutate files or run fixes.

### Inputs

- Parsed project summaries for period cost, calls, sessions, cache-write totals, and MCP usage.
- Claude JSONL sessions discovered through `discoverAllSessions('claude')`.
- `.mcp.json`, user/project Claude settings, `CLAUDE.md` and `.claude/CLAUDE.md`, `~/.claude/agents`, `~/.claude/skills`, `~/.claude/commands`, and shell profiles.

Important caveat: CodeBurn's optimize scanner is Claude-only even when `--provider` filters the project summary data. AINB should either keep that explicit or fix it by adding provider-specific optimize detectors.

### Findings

- Junk reads: flags reads in `node_modules`, `.git`, `dist`, `build`, `.next`, `coverage`, caches, virtualenvs, and similar generated paths.
- Duplicate reads: flags repeated reads of the same non-junk file in the same project/session.
- Unused MCP servers: flags configured MCP servers with no matching `mcp__server__tool` usage and no session MCP breakdown usage, ignoring configs modified in last 24h.
- Bloated `CLAUDE.md`: expands `@./...` imports to depth 5 and flags expanded memory over 200 lines.
- Low read/edit ratio: flags edit-heavy sessions with fewer than 4 read/search tools per edit once there are at least 10 edits.
- Cache bloat: compares median cache creation tokens to a budget-aware baseline and flags high excess.
- Ghost agents: flags `~/.claude/agents/*.md` files not invoked by Agent/Task tool metadata.
- Ghost skills: flags `~/.claude/skills/*/SKILL.md` directories not invoked by the Skill tool.
- Ghost commands: flags `~/.claude/commands/*.md` files not referenced by slash command/user-message markers.
- Bash output bloat: flags unset/default or high `BASH_MAX_OUTPUT_LENGTH`, recommends 15000.

### Ranking And Health

- Each finding has `impact` high/medium/low, explanation, estimated tokens saved, fix action, optional trend.
- Trend compares recent 48h waste against older baseline. Recent zero suppresses resolved findings; <50% recent rate marks improving.
- Health score starts at 100. Penalties: high 15, medium 7, low 3; minimum score 20.
- Grades: A >=90, B >=75, C >=55, D >=30, F below 30.
- Urgency sort weights impact 70% and normalized token savings 30%.

### Rust Port Shape

Port `optimize` as pure detectors with injected scan/config data:

- `usage::optimize::detectors::*`
- `usage::optimize::scanner`
- `usage::optimize::render`

Keep all actions as strings. Do not execute `mv`, `claude mcp remove`, or file edits.

## Plan / Usage Budget

CodeBurn does not appear to call `claude -p /usage`. It stores plan settings in `~/.config/codeburn/config.json` and compares API-equivalent local spend against a monthly budget.

Plan IDs:

- `claude-pro`: $20/month, provider `claude`
- `claude-max`: $200/month, provider `claude`
- `claude-max-5x`: $100/month, provider `claude`
- `cursor-pro`: $20/month, provider `cursor`
- `custom`: caller supplies `--monthly-usd`
- `none`: disabled

Plan usage:

- Billing period comes from reset day 1-28.
- Spend is summed from parsed project cost.
- Status is `under`, `near` at >=80%, or `over` at >100%.
- Projection uses trailing 7-day median daily cost times remaining days.

AINB should support manual plan settings first. A `plan detect` or `plan import-claude` command can run `claude -p /usage` only if the installed Claude CLI returns stable parseable output. That path must be optional; manual config remains authoritative.

## Compare

CodeBurn compare is model-centric and interactive.

Metrics:

- Calls
- Cost
- Input/output/cache tokens
- Total turns
- Edit turns
- One-shot turns/rate
- Retries/rate
- Self-corrections/rate
- Cost per call
- Cost per edit
- Output tokens per call
- Cache hit rate
- Category head-to-head one-shot rate
- Working style: delegation rate, planning rate, average tools per turn, fast-mode usage

Self-corrections are scanned from assistant text with correction/apology regexes. Non-TTY compare exits with a message. AINB can offer both an interactive TUI subview and a non-interactive `--format json` enhancement if desired, but CodeBurn itself is interactive-only.

## Yield

CodeBurn yield correlates session spend to git commits in the current repo.

Flow:

- Detect git repo.
- Resolve main branch from `origin/HEAD`, then `main`, then `master`.
- Read commits from all branches in selected period.
- Mark revert commits by subject containing `revert`.
- For each session, look for commits between first session timestamp and last timestamp + 1 hour.
- Categorize session spend:
  - Productive: commit landed on main.
  - Reverted: revert commits are at least half of in-main commits.
  - Abandoned: no commits or no in-main commits.

Output prints Productive/Reverted/Abandoned cost, percent of total, session counts, and total. CodeBurn hardcodes `$` here instead of active currency.

## Currency And Model Aliases

Currency:

- Stored config field: `currency { code, symbol? }`.
- Validated through ISO 4217 formatting.
- Exchange rates fetched from Frankfurter, cached 24h under `~/.cache/codeburn/exchange-rate.json`.
- Invalid/fetch failure falls back to USD rate 1.

Model aliases:

- Stored as `modelAliases`.
- User aliases override built-ins.
- Used to map provider-emitted names to LiteLLM pricing keys and display short names.

Pricing:

- LiteLLM model price snapshot is bundled.
- Live LiteLLM price JSON is fetched and cached 24h under `~/.cache/codeburn/litellm-pricing.json`.
- Unknown model costs are zero in CodeBurn; AINB should make unknown pricing explicit in UI.

## AINB Integration

Current AINB:

- CLI commands live in `ainb-tui/src/cli/mod.rs`; dispatch is manual in `ainb-tui/src/main.rs`.
- Top-level `Status` already means session status, so CodeBurn status belongs under `ainb usage status`.
- Usage model is currently token-only Claude JSONL parsing in `ainb-tui/src/models/usage.rs`.
- Usage UI has provider labels but only Claude data, and only Daily/Weekly/Projects tabs.
- Config is layered TOML through `AppConfig` and user config path `~/.agents-in-a-box/config/config.toml`.

Recommended Rust modules:

- `ainb-tui/src/models/usage/mod.rs`
- `ainb-tui/src/models/usage/query.rs`
- `ainb-tui/src/models/usage/providers/{claude,codex}.rs`
- `ainb-tui/src/models/usage/pricing.rs`
- `ainb-tui/src/models/usage/report.rs`
- `ainb-tui/src/models/usage/export.rs`
- `ainb-tui/src/models/usage/optimize.rs`
- `ainb-tui/src/models/usage/compare.rs`
- `ainb-tui/src/models/usage/yield_analysis.rs`
- `ainb-tui/src/cli/usage.rs`

Recommended command shape keeps existing session commands stable:

```bash
ainb usage report --period week --format text
ainb usage report --from 2026-04-01 --to 2026-04-10 --provider codex --format json
ainb usage status --format json
ainb usage export --format csv --output ./usage-export
ainb usage optimize --period 30days
ainb usage compare --period all
ainb usage yield --period week
ainb usage plan show --format json
ainb usage plan set claude-pro --reset-day 12
ainb usage plan set custom --monthly-usd 75 --provider claude
ainb usage currency GBP
ainb usage model-alias cursor-auto claude-sonnet-4-5
```

## Gaps In Existing Plan

The previous local plan intentionally excluded optimize, plan tracking, compare, yield, currency, and model aliases. Stevie now requested comprehensive CLI coverage except menubar. The implementation plan must be expanded from Burndown MVP to a usage analytics subsystem.

## Interview Need

No interview needed for scope. Stevie clarified:

- Comprehensive CLI parity.
- No menubar.
- Rust port of the logic.
- Plan setting required.
- Optional `claude -p /usage` can be researched/attempted later, but CodeBurn itself uses local plan config and parsed spend.

