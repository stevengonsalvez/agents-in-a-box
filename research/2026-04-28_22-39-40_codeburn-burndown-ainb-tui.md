# Research: CodeBurn-Style Burndown In AINB TUI

**Date**: 2026-04-28 22:39:40 Europe/London
**Repository**: agents-in-a-box
**Branch**: feat/codeburn
**Commit**: 8ac34152c726b5ccb025ed32eabc642929eae165
**Research Type**: Comprehensive

## Research Question

Investigate `https://github.com/getagentseal/codeburn` and plan how to bake the same kind of usage observability, similar views, and burndown/dashboard construct into the `ainb-tui` usage area.

## Executive Summary

AINB already has a full-screen `Usage Analytics` view, so the lowest-risk path is to extend that screen with a new `Burndown` sub-tab rather than add a separate top-level view. CodeBurn's highest-value ideas are a provider-normalized usage model, deterministic activity classification, period/provider switching, custom date ranges, project include/exclude filters, export/report output, and compact Ratatui panels for daily, project, model, activity, tool, shell, and MCP breakdowns.

## Key Findings

- CodeBurn is a local-session reader, not a wrapper or proxy. Its README says it reads data from disk and uses LiteLLM pricing, with no API keys or proxy flow required.
- CodeBurn's dashboard is a multi-panel terminal dashboard: overview, daily activity, project, top sessions, activity, model, core tools, shell commands, and MCP servers.
- CodeBurn also provides custom `--from` / `--to` ranges, project include/exclude filters, JSON report output, and CSV/JSON exports; these should be required scope, not follow-up.
- AINB has `View::Analytics`, a `Stats` sidebar entry, `UsageViewState`, async usage parsing, and existing usage keyboard handling.
- AINB's current usage parser only supports Claude Code token totals from `~/.claude/projects/**/*.jsonl`; Codex/Gemini/Copilot providers are listed in UI but have no data implementation.
- Implementation should start by reshaping `UsageData` into a richer normalized summary while preserving the current daily/weekly/project table behavior.

## Prior Learnings

### Relevant Past Solutions

| Learning | Key Insight | Confidence |
|----------|-------------|------------|
| project-cli-integration | Reuse existing AINB integration points and avoid parallel feature plumbing where the CLI/TUI already has a route. | medium |
| tui-bulk-delete-fix | TUI additions need explicit state/event/render tests because visual navigation regressions are easy to miss. | medium |

No dedicated prior learning was found for CodeBurn itself.

## Detailed Findings

### External CodeBurn Behavior

The live GitHub README describes CodeBurn as an AI coding cost observability dashboard supporting Claude Code, Codex, Cursor, Gemini CLI, Kiro, OpenCode, Pi, OMP, and GitHub Copilot with a provider plugin system. It tracks task type, tool, model, MCP server, project, and one-shot success rate, and provides an interactive dashboard with gradient charts and keyboard navigation. Source: https://github.com/getagentseal/codeburn

Important README details:

- Dashboard commands include `codeburn`, `today`, `month`, `report -p 30days`, `report -p all`, JSON report output, status output, export, optimize, and yield.
- Dashboard navigation uses arrows for periods, `1`-`5` for direct periods, `p` for provider, `o` for optimize, and `c` for model comparison.
- JSON output includes overview, daily breakdown, project summaries, model counts, activities with one-shot rates, core tools, MCP servers, and shell commands.
- Provider paths include `~/.claude/projects/` and `~/.codex/sessions/`.
- Custom `--from` and `--to` dates use `YYYY-MM-DD`, local-time inclusive windows.
- Project filters use repeatable include and exclude flags by case-insensitive substring.

### CodeBurn UI Layout

CodeBurn period tabs are `today`, `week`, `30days`, `month`, and `all` in `/tmp/codeburn-research/src/dashboard.tsx:18`. It switches to a wide two-column layout at 90 columns and caps dashboard width at 160 columns in `/tmp/codeburn-research/src/dashboard.tsx:120`.

The overview panel computes total cost, API calls, sessions, token totals, cache hit, and optional plan status in `/tmp/codeburn-research/src/dashboard.tsx:171`. The dashboard content renders overview, daily/project, top sessions, activity/model, tools/shell, and MCP sections in `/tmp/codeburn-research/src/dashboard.tsx:602`.

Panel-level concepts worth porting first:

- Period tabs: Today, 7 Days, 30 Days, This Month, All Time.
- Overview: cost, calls, sessions, cache hit, token totals.
- Daily activity: horizontal bars plus cost/calls.
- By project: cost, average per session, session count.
- By activity: cost, turns, one-shot percentage.
- By model: cost, cache hit, call count.
- Core tools, shell commands, MCP servers: call counts with bars.

### CodeBurn Data Model

CodeBurn normalizes session data into project/session/turn/API-call summaries. Claude parsing extracts usage, model, cache tokens, web search requests, tool names, MCP tools, bash commands, and cost in `/tmp/codeburn-research/src/parser.ts:77`. Session aggregation totals cost, tokens, model/tool/MCP/bash/category breakdowns in `/tmp/codeburn-research/src/parser.ts:167`.

Non-Claude providers emit normalized provider calls and then reuse the same summary/classifier flow in `/tmp/codeburn-research/src/parser.ts:359`.

The Codex provider reads `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, validates `session_meta`, maps tool names like `exec_command` to `Bash`, and normalizes OpenAI cached-input semantics in `/tmp/codeburn-research/src/providers/codex.ts:21`, `/tmp/codeburn-research/src/providers/codex.ts:92`, and `/tmp/codeburn-research/src/providers/codex.ts:241`.

### CodeBurn Classification

CodeBurn classification is deterministic and does not call an LLM. It first classifies by tool patterns, then refines with user-message keywords in `/tmp/codeburn-research/src/classifier.ts:56`. It counts edit/test/fix retry cycles with an edit -> bash -> edit pattern in `/tmp/codeburn-research/src/classifier.ts:120`.

Categories:

- Coding
- Debugging
- Feature Dev
- Refactoring
- Testing
- Exploration
- Planning
- Delegation
- Git Ops
- Build/Deploy
- Brainstorming
- Conversation
- General

### AINB Existing Usage Screen

AINB screens are `View` enum variants; `View::Analytics` is the current usage statistics route in `ainb-tui/src/app/state.rs:397`. Home sidebar `Stats` maps to usage analytics with shortcut `i` in `ainb-tui/src/components/sidebar.rs:27`, `ainb-tui/src/components/sidebar.rs:87`, and `ainb-tui/src/components/sidebar.rs:104`.

`LayoutComponent::render` dispatches `View::Analytics` to `crate::components::usage::render` in `ainb-tui/src/components/layout.rs:193`. `AppState` owns `usage_state` plus an async receiver for usage parsing in `ainb-tui/src/app/state.rs:1923`.

The usage UI already has provider state, sub-tabs, loading state, and scroll offset in `ainb-tui/src/components/usage.rs:121`. Current sub-tabs are `Daily`, `Weekly`, and `Projects` in `ainb-tui/src/components/usage.rs:82`, and render branching happens in `ainb-tui/src/components/usage.rs:226`.

### AINB Usage Parser

AINB's current parser is token-only. `TokenBucket` tracks input, cache creation, cache read, output, session count, and project count in `ainb-tui/src/models/usage.rs:10`. `UsageData` only carries daily, weekly, project, and grand total aggregates in `ainb-tui/src/models/usage.rs:41`.

`parse_usage()` reads Claude Code JSONL files from `~/.claude/projects`, aggregates by day/week/project, and sorts projects by token total in `ainb-tui/src/models/usage.rs:85`.

AINB correctly keeps this scan off the UI thread. `start_background_usage_load()` spawns blocking parse work in `ainb-tui/src/app/state.rs:3245`, and completion is drained through `check_usage_load_complete()`.

### AINB Keyboard/Event Flow

Usage events are already first-class `AppEvent` variants in `ainb-tui/src/app/events.rs:332`. Analytics key routing handles `Esc`, left/right, `Tab`, `BackTab`, `j/k`, page keys, `g/G`, and `r` in `ainb-tui/src/app/events.rs:1540`.

The sidebar `Stats` entry enters Analytics and starts background loading in `ainb-tui/src/app/events.rs:2865`. The direct `i` shortcut maps to `GoToStats`, which also enters Analytics and starts loading in `ainb-tui/src/app/events.rs:3072`.

### Tests

UI rendering tests already use `ratatui::backend::TestBackend` and buffer assertions in `ainb-tui/tests/test_ui_display.rs:7`. App state tests exist in `ainb-tui/tests/test_app_state.rs:39`. No direct tests currently cover `UsageViewState`, `models::usage`, `UsageTab`, or Analytics-specific event behavior.

## Code References

- `ainb-tui/src/app/state.rs:397` - `View` enum includes `Analytics`.
- `ainb-tui/src/components/sidebar.rs:27` - sidebar navigation items include `Stats`.
- `ainb-tui/src/components/layout.rs:193` - Analytics layout dispatch.
- `ainb-tui/src/components/usage.rs:82` - current usage sub-tabs.
- `ainb-tui/src/components/usage.rs:121` - current usage UI state.
- `ainb-tui/src/models/usage.rs:10` - current token bucket model.
- `ainb-tui/src/models/usage.rs:85` - current Claude-only usage parser.
- `ainb-tui/src/app/state.rs:3245` - background usage parsing.
- `/tmp/codeburn-research/src/dashboard.tsx:602` - CodeBurn dashboard panel composition.
- `/tmp/codeburn-research/src/parser.ts:167` - CodeBurn session aggregation.
- `/tmp/codeburn-research/src/classifier.ts:56` - deterministic activity classifier.
- `/tmp/codeburn-research/src/providers/codex.ts:92` - Codex session discovery.

## Recommendations

1. Extend the existing AINB `Stats` / `Usage Analytics` screen with `UsageTab::Burndown` instead of adding a new top-level view.
2. Introduce richer usage-domain types before UI work: normalized provider calls, classified turns, session summaries, project summaries, daily summaries, and dashboard totals.
3. Keep provider adapters isolated. Start with Claude and Codex because AINB already exposes those providers in the UI and both have disk-backed session logs.
4. Preserve the current async parse pathway and add lightweight cache/incremental scan later if parsing grows expensive.
5. Treat custom date ranges, include/exclude filters, JSON reports, and CSV/JSON export as required scope for first delivery.
6. Implement the Burndown view as a compact CodeBurn-style panel dashboard first, then add optimize/plan/model-compare as later tabs or overlays.
7. Add behavioral tests at model, CLI, state, event, and Ratatui render layers.

## Open Questions

- No blocking questions. Assumption: `Burndown` should live inside existing `Usage Analytics` as a sub-tab and reuse shortcut `i`, with `Tab` cycling through Daily, Weekly, Projects, and Burndown.
- Later product decision: whether to copy CodeBurn's subscription plan tracking, optimize findings, model comparison, currency conversion, model aliases, menubar, and yield analysis after first delivery.
