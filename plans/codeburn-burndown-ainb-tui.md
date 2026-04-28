# CodeBurn-Style Burndown Implementation Plan

## Overview

Add a CodeBurn-inspired `Burndown` sub-tab to AINB's existing `Usage Analytics` screen, plus report/export CLI support for the same data. The first release should show local AI coding usage by period, provider, project, model, activity, tool, shell command, and MCP server while keeping parsing read-only and off the UI thread.

## Current State Analysis

AINB already has a full-screen usage route: `View::Analytics` in `ainb-tui/src/app/state.rs:397`, sidebar `Stats` in `ainb-tui/src/components/sidebar.rs:27`, and layout dispatch in `ainb-tui/src/components/layout.rs:193`.

Current usage support is narrow:

- `UsageViewState` tracks provider, sub-tab, data, loading, and scroll offset in `ainb-tui/src/components/usage.rs:121`.
- Existing sub-tabs are only `Daily`, `Weekly`, and `Projects` in `ainb-tui/src/components/usage.rs:82`.
- `UsageProvider` lists Claude, Codex, Gemini, and Copilot, but `has_data()` only enables Claude in `ainb-tui/src/components/usage.rs:59`.
- `parse_usage()` reads `~/.claude/projects/**/*.jsonl` and produces token totals only in `ainb-tui/src/models/usage.rs:85`.

CodeBurn shows a richer usage dashboard: period tabs, provider switching, cost/calls/sessions/cache-hit overview, daily/project/model/activity/tool/shell/MCP panels, deterministic activity classification, and one-shot edit-cycle rates.

## Desired End State

AINB's Stats screen includes a `Burndown` tab that looks and behaves like a native AINB Ratatui screen while borrowing CodeBurn's information architecture:

- Period selector: Today, 7 Days, 30 Days, Month, All.
- Custom date range support: inclusive `from` / `to` dates in `YYYY-MM-DD` format.
- Provider selector: All, Claude, Codex for MVP; existing unsupported providers remain visibly unavailable.
- Include/exclude project filters by case-insensitive substring.
- Overview metrics: estimated cost, calls, sessions, cache hit, input/output/cache tokens.
- Panels: Daily Activity, By Project, Top Sessions, By Activity, By Model, Core Tools, Shell Commands, MCP Servers.
- CLI report/export commands for text, JSON, and CSV output using the same summary model as the TUI.
- Existing Daily/Weekly/Projects tabs keep working.
- Parsing remains local, read-only, asynchronous, and safe for large history folders.

### Key Discoveries

- `start_background_usage_load()` already keeps expensive JSONL parsing off the event thread in `ainb-tui/src/app/state.rs:3245`.
- Usage keyboard handling already supports tab switching, provider switching, scroll, and refresh in `ainb-tui/src/app/events.rs:1540`.
- CodeBurn's normalized model and deterministic classifier are portable: `/tmp/codeburn-research/src/parser.ts:359` and `/tmp/codeburn-research/src/classifier.ts:56`.

## What We're NOT Doing

- Not vendoring CodeBurn's TypeScript implementation into AINB.
- Not adding a new top-level `Burndown` screen for MVP.
- Not writing to Claude, Codex, Cursor, Copilot, or other agent session files.
- Not implementing Cursor/OpenCode/Gemini/Copilot parsing in the first pass unless the provider abstraction makes it trivial later.
- Not implementing CodeBurn optimize findings, subscription plan tracking, model comparison, model aliases, currency conversion, menubar, or yield analysis in MVP.
- Not adding API calls to price usage. MVP can use built-in/fallback pricing and show token-only values when price is unknown.

## Implementation Approach

Build the data model first, then render it inside the existing Usage screen. Keep the current `UsageData` fields or equivalent adapters so existing Daily/Weekly/Projects tables do not regress. Add richer summaries behind the same background load path and only then add the `Burndown` tab UI.

## Phase 1: Domain Model And Provider Adapters
<!-- wave: 1 | depends_on: [] | files: [ainb-tui/src/models/usage.rs, ainb-tui/src/models/mod.rs] -->

### Overview

Reshape usage parsing around normalized provider events while preserving the existing token aggregates.

### Changes Required

#### 1. Usage Domain Types
**File**: `ainb-tui/src/models/usage.rs`
**Changes**:

Add types equivalent to:

```rust
pub enum UsagePeriod {
    Today,
    Week,
    ThirtyDays,
    Month,
    All,
    Custom { from: chrono::NaiveDate, to: chrono::NaiveDate },
}

pub enum UsageProviderFilter {
    All,
    Claude,
    Codex,
}

pub enum ActivityCategory {
    Coding,
    Debugging,
    Feature,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Brainstorming,
    Conversation,
    General,
}

pub struct ProviderCall {
    pub provider: String,
    pub model: String,
    pub session_id: String,
    pub project: String,
    pub timestamp: chrono::DateTime<chrono::Local>,
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: f64,
    pub tools: Vec<String>,
    pub bash_commands: Vec<String>,
    pub user_message: String,
}

pub struct UsageQuery {
    pub period: UsagePeriod,
    pub provider_filter: UsageProviderFilter,
    pub include_projects: Vec<String>,
    pub exclude_projects: Vec<String>,
}
```

Add dashboard summary structs for daily, project, session, model, activity, tool, shell, and MCP panels. Keep or derive current `daily`, `weekly`, `projects`, and `grand_total` so old tabs remain backed by the new summary.

#### 2. Provider Parsing
**File**: `ainb-tui/src/models/usage.rs`
**Changes**:

Split the current Claude parser into:

- `discover_claude_sources()`
- `parse_claude_source()`
- `discover_codex_sources()`
- `parse_codex_source()`
- `parse_usage_for(query: UsageQuery)`

For Codex, read `CODEX_HOME` or `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, validate `session_meta`, extract token count events, normalize cached tokens, and map tools:

- `exec_command` -> `Bash`
- `apply_patch` / `apply_diff` / `write_file` -> `Edit`
- `spawn_agent` / `wait_agent` / `close_agent` -> `Agent`
- `read_file` -> `Read`
- `read_dir` -> `Glob`

#### 3. Cost Formatting
**File**: `ainb-tui/src/models/usage.rs`
**Changes**:

Add simple built-in pricing lookup for known Claude and GPT model names. Return `None` for unknown pricing and render token/call metrics without claiming exact cost.

### Success Criteria

#### Automated Verification
- [ ] Unit tests parse small Claude fixture and preserve daily/project totals.
- [ ] Unit tests parse small Codex fixture and normalize cached input tokens.
- [ ] Unit tests validate date filtering uses assistant-call timestamp.
- [ ] Unit tests validate custom from/to ranges are inclusive and reject inverted ranges.
- [ ] Unit tests validate include and exclude filters apply after provider parsing and before aggregation.
- [ ] `cargo test usage --all-targets` passes from `ainb-tui`.

#### Manual Verification
- [ ] Opening Stats still shows existing Daily/Weekly/Projects data.
- [ ] Codex provider no longer shows "not yet available" when local Codex sessions exist.

## Phase 2: Activity Classifier And Burndown Aggregation
<!-- wave: 2 | depends_on: [Phase 1] | files: [ainb-tui/src/models/usage.rs] -->

### Overview

Port CodeBurn's deterministic classifier and aggregate normalized calls into burndown panels.

### Changes Required

#### 1. Classifier
**File**: `ainb-tui/src/models/usage.rs`
**Changes**:

Implement deterministic classification:

- Tool-first categories: edit -> coding, agent -> delegation, plan/todo -> planning, read/search -> exploration.
- Keyword refinement for debugging, feature, refactoring, testing, git, build/deploy, brainstorming.
- One-shot metric via edit -> bash -> edit retry count inside a turn/session window.

#### 2. Aggregation
**File**: `ainb-tui/src/models/usage.rs`
**Changes**:

Aggregate:

- Overview totals.
- Daily cost/calls/session/token rows.
- Project totals, average cost per session, session count.
- Top sessions.
- Model totals with calls, cost, cache hit.
- Activity totals with turns, cost, one-shot percent.
- Tool, shell, and MCP call counts.

### Success Criteria

#### Automated Verification
- [ ] Classifier tests cover coding, debugging, feature, refactoring, testing, exploration, planning, delegation, git, build/deploy, brainstorming, conversation, general.
- [ ] Retry tests cover edit-only one-shot and edit -> bash -> edit retry.
- [ ] Aggregation tests verify project/model/activity/tool totals from mixed Claude/Codex fixture calls.

#### Manual Verification
- [ ] Generated summary values are plausible compared with a known small fixture.

## Phase 3: Burndown UI Tab
<!-- wave: 3 | depends_on: [Phase 2] | files: [ainb-tui/src/components/usage.rs] -->

### Overview

Add `UsageTab::Burndown` and render a CodeBurn-style Ratatui dashboard inside the existing Usage screen.

### Changes Required

#### 1. State And Tab Wiring
**File**: `ainb-tui/src/components/usage.rs`
**Changes**:

Add:

```rust
pub enum UsageTab {
    Daily,
    Weekly,
    Projects,
    Burndown,
}
```

Extend `all()`, `title()`, `next()`, `prev()`, `row_count()`, and render branching.

#### 2. Burndown Panels
**File**: `ainb-tui/src/components/usage.rs`
**Changes**:

Add `render_burndown()` with responsive layout:

- Header summary.
- Period selector row.
- Two-column panels when width allows; stacked panels on narrow terminals.
- Reusable horizontal bar renderer with stable width and gradient-like thresholds.
- Panels: Daily Activity, By Project, Top Sessions, By Activity, By Model, Core Tools, Shell Commands, MCP Servers.

Use AINB palette, restrained borders, and no nested cards.

#### 3. Empty And Partial States
**File**: `ainb-tui/src/components/usage.rs`
**Changes**:

Render clear empty states for:

- No usage data.
- Provider unsupported.
- Costs unavailable but tokens/calls available.
- Narrow terminal fallback.

### Success Criteria

#### Automated Verification
- [ ] Render test with `TestBackend` sees `Burndown`, `Daily Activity`, `By Project`, `By Activity`, and `By Model`.
- [ ] Narrow render test confirms no panic and shows stacked fallback content.
- [ ] Existing usage render tests still pass.

#### Manual Verification
- [ ] Stats screen feels like one coherent screen, not a bolted-on clone.
- [ ] Text fits at 80x24 and 120x40.
- [ ] Tab cycling reaches Burndown and returns to Daily.

## Phase 4: Period, Range, Provider, And Filter Controls
<!-- wave: 4 | depends_on: [Phase 3] | files: [ainb-tui/src/components/usage.rs, ainb-tui/src/app/events.rs, ainb-tui/src/app/state.rs] -->

### Overview

Add CodeBurn-like period, custom range, provider, and project filter interaction without disrupting existing Usage keys.

### Changes Required

#### 1. Usage State
**File**: `ainb-tui/src/components/usage.rs`
**Changes**:

Add period and provider filter state:

```rust
pub period: UsagePeriod,
pub provider_filter: UsageProviderFilter,
pub include_projects: Vec<String>,
pub exclude_projects: Vec<String>,
```

Keep existing `provider` behavior if needed for compatibility, or replace it with the richer filter if all call sites can move cleanly.

#### 2. Events
**File**: `ainb-tui/src/app/events.rs`
**Changes**:

Add or reuse events for:

- Left/right: period switch while Burndown is active.
- `p`: provider filter cycle while Burndown is active.
- `1`-`5`: direct period shortcuts.
- `/`: open project include filter input.
- `x`: open project exclude filter input.
- `c`: clear include/exclude filters.
- `d`: open custom date range input.
- `r`: reload current period/provider.

Preserve current left/right provider switching on Daily/Weekly/Projects if changing it would be too disruptive; otherwise make provider cycling consistently use `p`.

#### 3. Background Load Parameters
**File**: `ainb-tui/src/app/state.rs`
**Changes**:

Update background usage loading to pass selected period, custom range, provider filter, and project filters into the parser. Cache raw parsed provider calls by provider/range where possible, then apply include/exclude filters cheaply in memory.

### Success Criteria

#### Automated Verification
- [ ] Event tests cover `1`-`5`, custom range entry, `p`, include filter, exclude filter, tab cycling, filter clearing, and refresh in Analytics.
- [ ] State tests verify reloads use selected period/provider/filter query and do not spawn duplicate loads.
- [ ] Filter tests verify include/exclude can be combined and exclusion wins when both match.

#### Manual Verification
- [ ] Period changes update data and loading state visibly.
- [ ] Custom from/to dates update the dashboard and reject invalid or inverted dates.
- [ ] Provider cycling works for All, Claude, and Codex.
- [ ] Include/exclude project filters visibly change project, daily, and overview totals.

## Phase 5: Report And Export CLI
<!-- wave: 5 | depends_on: [Phase 2] | files: [ainb-tui/src/cli/mod.rs, ainb-tui/src/cli/usage.rs, ainb-tui/src/main.rs, ainb-tui/src/models/usage.rs] -->

### Overview

Add non-interactive usage report/export commands powered by the same parser and summary types as the TUI.

### Changes Required

#### 1. CLI Command Shape
**File**: `ainb-tui/src/cli/mod.rs`
**Changes**:

Add usage commands:

```rust
Usage(UsageArgs),
Export(ExportArgs),
```

Recommended CLI:

```bash
ainb usage report --period week
ainb usage report --from 2026-04-01 --to 2026-04-10
ainb usage report --provider codex --include agents-in-a-box
ainb usage report --exclude scratch --format json
ainb usage export --period 30days --format csv --output usage.csv
ainb usage export --from 2026-04-01 --to 2026-04-10 --format json
```

Support repeatable `--include` and `--exclude` flags. `--from` and `--to` must accept `YYYY-MM-DD`; either flag alone is valid, with missing bound defaulting to earliest data or today.

#### 2. CLI Implementation
**File**: `ainb-tui/src/cli/usage.rs`
**Changes**:

Implement:

- Text report: same high-level sections as Burndown, compact for terminal output.
- JSON report: complete structured summary for automation.
- CSV export: stable rows for daily, project, model, activity, tool, shell, and MCP sections.
- `--output` support. Without `--output`, write to stdout.

#### 3. Main Routing
**File**: `ainb-tui/src/main.rs`
**Changes**:

Route `Commands::Usage` and `Commands::Export` to the new implementation without entering TUI mode.

### Success Criteria

#### Automated Verification
- [ ] CLI argument tests cover period, custom dates, provider, include/exclude, format, and output path.
- [ ] JSON output test validates stable keys for overview, daily, projects, models, activities, tools, shell commands, and MCP servers.
- [ ] CSV output test validates headers and row sections.
- [ ] Invalid date format and inverted range return clear errors.

#### Manual Verification
- [ ] `ainb usage report --period week` prints useful text.
- [ ] `ainb usage report --from 2026-04-01 --to 2026-04-10 --format json | jq '.overview'` works.
- [ ] `ainb usage export --format csv --output /tmp/ainb-usage.csv` creates a usable CSV.

## Phase 6: Quality Gates And Documentation
<!-- wave: 6 | depends_on: [Phase 4, Phase 5] | files: [ainb-tui/tests/test_ui_display.rs, ainb-tui/tests/test_events.rs, ainb-tui/tests/test_app_state.rs, ainb-tui/docs/CLI.md, ainb-tui/README.md] -->

### Overview

Lock in behavior and document the new view.

### Changes Required

#### 1. Tests
**Files**: `ainb-tui/tests/test_ui_display.rs`, `ainb-tui/tests/test_events.rs`, `ainb-tui/tests/test_app_state.rs`
**Changes**:

Add tests for:

- Burndown render smoke.
- Usage tab/state navigation.
- Analytics event routing.
- Parser fixture behavior.
- CLI report/export behavior.

#### 2. Docs
**Files**: `ainb-tui/docs/CLI.md`, `ainb-tui/README.md`
**Changes**:

Document:

- Stats screen shortcut.
- Burndown tab.
- Supported providers and session locations.
- Periods and custom `--from` / `--to` ranges.
- Include/exclude project filters.
- Text, JSON, and CSV export.
- Read-only local parsing.
- Cost estimate caveat.

### Success Criteria

#### Automated Verification
- [ ] `cargo fmt --check` passes.
- [ ] `cargo test --all-targets` passes from `ainb-tui`.
- [ ] `cargo clippy --all-targets -- -D warnings` passes or known existing warnings are documented.

#### Manual Verification
- [ ] Launch `ainb`, open Stats with `i`, tab to Burndown, switch periods/providers, refresh.
- [ ] Verify no session files are modified.

## Testing Strategy

### Unit Tests

- Parser fixture tests for Claude and Codex.
- Classifier category tests.
- Aggregation tests for dashboard panels.
- Usage state tests for tab, period, provider, and scroll behavior.

### Integration Tests

- Event routing from `View::Analytics`.
- Background load coalescing.
- Ratatui render smoke tests at 80x24 and 120x40.

### Manual Testing Steps

1. Run `cargo test --all-targets` inside `ainb-tui`.
2. Launch `ainb`.
3. Press `i` to open Stats.
4. Press `Tab` until Burndown is active.
5. Press `1`, `2`, `3`, `4`, `5` and confirm period labels/data update.
6. Press `p` and confirm provider filter cycles.
7. Enter custom date range and confirm dashboard updates.
8. Add include/exclude project filters and confirm totals change.
9. Press `r` and confirm reload notification/loading state.
10. Run `ainb usage report --from 2026-04-01 --to 2026-04-10 --format json`.
11. Run `ainb usage export --format csv --output /tmp/ainb-usage.csv`.

## Performance Considerations

- Keep all filesystem scans in `spawn_blocking`, matching existing usage load behavior.
- Use file mtime to skip files older than selected date range where safe.
- Cache parsed raw calls by date range/provider where possible; apply include/exclude filters without reparsing.
- Consider a daily cache only after MVP if local histories are too large.

## Migration Notes

No data migration. The feature is read-only and derives summaries from local session files.

## References

- Research: `research/2026-04-28_22-39-40_codeburn-burndown-ainb-tui.md`
- CodeBurn dashboard composition: `/tmp/codeburn-research/src/dashboard.tsx:602`
- CodeBurn parser aggregation: `/tmp/codeburn-research/src/parser.ts:167`
- CodeBurn classifier: `/tmp/codeburn-research/src/classifier.ts:56`
- AINB Analytics view: `ainb-tui/src/app/state.rs:397`
- AINB Usage UI state: `ainb-tui/src/components/usage.rs:121`
- AINB usage parser: `ainb-tui/src/models/usage.rs:85`
