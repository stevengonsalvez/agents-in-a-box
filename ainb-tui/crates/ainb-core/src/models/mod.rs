// ABOUTME: Core data models for Claude-in-a-Box sessions, workspaces, and state management

pub mod live_window;
pub mod live_window_watcher;
pub mod other_tmux;
pub mod repo_lookup;
pub mod session;
pub mod skills;
pub mod usage;
pub mod workspace;

pub use other_tmux::OtherTmuxSession;
pub use session::{
    ClaudeModel, CodexModel, GitChanges, Session, SessionAgentType, SessionMode, SessionStatus,
    ShellSession, ShellSessionStatus, SshTarget,
};
pub use skills::{AgentDef, Skill, SkillsData};
// Phase 6d: trimmed re-exports. The host purge removed every external
// caller of the analytics aggregation surface (`UsageQuery`,
// `UsageFilters`, `UsageFilterChip`, `UsagePeriod`, `UsageProviderFilter`,
// `OptimizeResult`, `CompareResult`, `YieldResult`, `WasteFinding`,
// `HealthGrade`, `ModelComparison`, `PlanProjection`, `PlanStatus`,
// `filter_usage_data`, `format_tokens_short`, `optimize_usage`) — those
// types live entirely inside `models/usage.rs` now (still referenced by
// the in-module parser-pipeline tests, the `live_window` Tier 2 reader,
// and the `usage_cache` SQLite blob layout). Plugin consumers go through
// `ainb-plugin-types-sessions` for the wire schema instead.
//
// What stays re-exported: types still consumed outside `models/`:
//   * `ProviderCall` — `live_window`, `usage_cache`, `test_support`.
//   * `UsageData` + the row types it owns — `cli/usage.rs::report_json`
//     (the byte-identity oracle) and `test_support::sample_usage_data`.
//   * `ActivityCategory` / `ActivityUsage` / `ModelUsage` / `NamedUsage`
//     / `ProjectUsage` / `SessionUsage` / `TokenBucket` — same pair.
pub use usage::{
    ActivityCategory, ActivityUsage, ModelUsage, NamedUsage, ProjectUsage, ProviderCall,
    SessionUsage, TokenBucket, UsageData,
};
pub use workspace::Workspace;
