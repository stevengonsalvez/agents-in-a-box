// ABOUTME: Non-interactive usage analytics commands.
// Provides CodeBurn-style reports, exports, plans, optimization, compare, and yield.

use crate::output_format::OutputFormat;
use crate::config::{AppConfig, CurrencyConfig, UsagePlan, UsagePlanId, UsagePlanProvider};
use crate::data::usage::{
    ActivityUsage, NamedUsage, ProjectUsage, SessionUsage, UsageData, UsageFilters, UsagePeriod,
    UsageProviderFilter, UsageQuery, UsageSourceRoots, analyze_yield, billing_period,
    compare_models, disabled_cache, optimize_usage, parse_usage_for,
    parse_usage_for_with_roots_and_cache, shared_cache,
};
use anyhow::{Result, anyhow, bail};
use chrono::{Local, NaiveDate};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum UsageCommands {
    /// Print a compact burndown report
    Report(UsageReportArgs),
    /// Print current usage status
    Status(UsageReportArgs),
    /// Print today's usage
    Today(UsageReportArgs),
    /// Print current month's usage
    Month(UsageReportArgs),
    /// Export usage data as CSV or JSON
    Export(UsageExportArgs),
    /// Manage usage plan
    Plan {
        #[command(subcommand)]
        command: UsagePlanCommands,
    },
    /// Set or reset display currency
    Currency(UsageCurrencyArgs),
    /// Manage model aliases
    ModelAlias(UsageModelAliasArgs),
    /// Show read-only optimization findings
    Optimize(UsageReportArgs),
    /// Compare models
    Compare(UsageReportArgs),
    /// Estimate usage yield from session signals
    Yield(UsageReportArgs),
    /// Inspect or wipe the persistent usage cache
    Cache {
        #[command(subcommand)]
        command: UsageCacheCommands,
    },
    /// Per-model rollup or per-model × per-activity-category matrix
    Models(UsageModelsArgs),
}

#[derive(Args, Clone, Default)]
pub struct UsageModelsArgs {
    #[command(flatten)]
    pub report: UsageReportArgs,
    /// Emit a per-model × per-activity-category matrix instead of the
    /// flat per-model rollup. Rows = model, columns = activity
    /// category, cell = (calls, tokens, cost).
    #[arg(long)]
    pub by_task: bool,
}

#[derive(Subcommand)]
pub enum UsageCacheCommands {
    /// Show cache DB path, on-disk size, file count, oldest entry timestamp
    Info,
    /// Drop all cached file rows (schema_version row preserved)
    Clear,
}

#[derive(Args, Clone, Default)]
pub struct UsageReportArgs {
    /// Period: today, week, 30days, month, all
    #[arg(long, value_enum, default_value_t = PeriodArg::Week)]
    pub period: PeriodArg,
    /// Start date YYYY-MM-DD (mutually exclusive with --month, --quarter,
    /// --last-n-days, --ytd; pairs with --to for an explicit range).
    #[arg(long, conflicts_with_all = ["month", "quarter", "last_n_days", "ytd"])]
    pub from: Option<String>,
    /// End date YYYY-MM-DD (mutually exclusive with --month, --quarter,
    /// --last-n-days, --ytd; pairs with --from for an explicit range).
    #[arg(long, conflicts_with_all = ["month", "quarter", "last_n_days", "ytd"])]
    pub to: Option<String>,
    /// Pin to a specific calendar month, e.g. `2026-04`. Mutually
    /// exclusive with --quarter, --last-n-days, --ytd, --from, --to.
    #[arg(long, conflicts_with_all = ["quarter", "last_n_days", "ytd", "from", "to"])]
    pub month: Option<String>,
    /// Pin to a specific calendar quarter, e.g. `2026-Q2`. Mutually
    /// exclusive with --month, --last-n-days, --ytd, --from, --to.
    #[arg(long, conflicts_with_all = ["month", "last_n_days", "ytd", "from", "to"])]
    pub quarter: Option<String>,
    /// Last N days (rolling window ending today). Mutually exclusive
    /// with --month, --quarter, --ytd, --from, --to.
    #[arg(
        long = "last-n-days",
        value_name = "N",
        conflicts_with_all = ["month", "quarter", "ytd", "from", "to"]
    )]
    pub last_n_days: Option<u32>,
    /// Jan 1 of the current year through today. Mutually exclusive
    /// with --month, --quarter, --last-n-days, --from, --to.
    #[arg(
        long,
        conflicts_with_all = ["month", "quarter", "last_n_days", "from", "to"]
    )]
    pub ytd: bool,
    /// Provider: all, claude, codex
    #[arg(long, value_enum, default_value_t = ProviderArg::All)]
    pub provider: ProviderArg,
    /// Include projects matching substring (repeatable; OR-combined).
    /// Note: previously aliased as `--project`; the alias has been
    /// removed because `--project` is now a distinct exact-match
    /// cross-filter flag (see below). Use `--include <substring>` for
    /// the substring/glob behaviour.
    #[arg(long)]
    pub include: Vec<String>,
    /// Exclude projects matching substring (repeatable; OR-combined).
    #[arg(long)]
    pub exclude: Vec<String>,
    /// Bypass the persistent usage cache and force a full re-parse.
    #[arg(long)]
    pub no_cache: bool,
    // ---- Cross-filter knobs (mirrors of the TUI dashboard pivot) ----
    // All four are repeatable; multiple values for the same filter
    // are OR-combined, and different filters AND together. They layer
    // on top of `--include` / `--exclude` and run through the same
    // `filter_usage_data` helper as the TUI.
    /// Drill into a single project (exact match). Repeatable.
    #[arg(long)]
    pub project: Vec<String>,
    /// Drill into a single model (exact match). Repeatable.
    #[arg(long)]
    pub model: Vec<String>,
    /// Drill into one activity category (Coding, Conversation, Git,
    /// etc. — see ActivityCategory::label). Repeatable.
    #[arg(long)]
    pub activity: Vec<String>,
    /// Drill into a single session id. Repeatable.
    #[arg(long)]
    pub session: Vec<String>,
    /// Drill into a single git branch (exact match against `gitBranch` on
    /// Claude turns). Repeatable. Codex turns have no recorded branch and
    /// are excluded by any non-empty `--branch` filter.
    #[arg(long)]
    pub branch: Vec<String>,
    /// Cap the long By-Project / By-Activity / By-Model tables at N rows
    /// (default 8 mirrors the historical hard-coded slice). Applies to
    /// report, today, month, and export subcommands across every format.
    /// 0 means "no cap" — emit every row.
    #[arg(long, default_value_t = 8)]
    pub top: usize,
}

#[derive(Args, Clone, Default)]
pub struct UsageExportArgs {
    #[command(flatten)]
    pub report: UsageReportArgs,
    /// Output file or directory
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum UsagePlanCommands {
    /// Show configured plan and current projection
    Show(UsageReportArgs),
    /// Set a known or custom plan
    Set {
        plan: PlanArg,
        #[arg(long)]
        monthly_usd: Option<f64>,
        #[arg(long, value_enum, default_value_t = PlanProviderArg::All)]
        provider: PlanProviderArg,
        #[arg(long, default_value_t = 1)]
        reset_day: u8,
    },
    /// Remove plan
    Reset,
    /// Attempt to detect plan from Claude CLI
    Detect,
}

#[derive(Args)]
pub struct UsageCurrencyArgs {
    /// Currency code, for example USD, GBP, EUR
    pub code: Option<String>,
    /// Display symbol
    #[arg(long)]
    pub symbol: Option<String>,
    /// Reset to USD
    #[arg(long)]
    pub reset: bool,
}

#[derive(Args)]
pub struct UsageModelAliasArgs {
    /// List aliases
    #[arg(long)]
    pub list: bool,
    /// Remove alias by source model name
    #[arg(long)]
    pub remove: Option<String>,
    /// Source model name
    pub from: Option<String>,
    /// Alias target model name
    pub to: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum PeriodArg {
    Today,
    #[default]
    Week,
    #[clap(name = "30days")]
    ThirtyDays,
    Month,
    All,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ProviderArg {
    #[default]
    All,
    Claude,
    Codex,
    Cursor,
    Copilot,
    Gemini,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PlanArg {
    ClaudePro,
    ClaudeMax,
    ClaudeMax5x,
    CursorPro,
    Custom,
    None,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum PlanProviderArg {
    #[default]
    All,
    Claude,
    Codex,
    Cursor,
}

/// Plugin-mode entry point. Uses a pre-loaded [`UsageData`] snapshot
/// fetched from the host's snapshot bus (`sessions.usage_data`) rather
/// than opening a local cache. The 9-subcommand surface matches the
/// `usage` CLI namespace declared in the manifest's `[provides]`.
pub fn execute_for_plugin(
    data: &UsageData,
    command: UsageCommands,
    format: OutputFormat,
) -> Result<()> {
    match command {
        UsageCommands::Report(args) => print_report_with_data(data, &args, format, "Usage Report"),
        UsageCommands::Status(args) => print_status_with_data(data, &args, format),
        UsageCommands::Today(mut args) => {
            args.period = PeriodArg::Today;
            print_report_with_data(data, &args, format, "Today")
        }
        UsageCommands::Month(mut args) => {
            args.period = PeriodArg::Month;
            print_report_with_data(data, &args, format, "Month")
        }
        UsageCommands::Export(args) => export_usage_with_data(data, &args, format),
        UsageCommands::Optimize(args) => print_optimize_with_data(data, &args, format),
        UsageCommands::Compare(args) => print_compare_with_data(data, &args, format),
        UsageCommands::Yield(args) => print_yield_with_data(data, &args, format),
        UsageCommands::ModelAlias(args) => model_alias_command_plugin(args),
        UsageCommands::Models(args) => print_models_with_data(data, &args, format),
        // Plan / Currency / Cache stay in the host-side `cli/usage.rs`
        // shim — they're config admin, not analytics.
        UsageCommands::Plan { .. }
        | UsageCommands::Currency(_)
        | UsageCommands::Cache { .. } => anyhow::bail!(
            "subcommand handled in host (config admin), not the burndown plugin"
        ),
    }
}


fn print_report_with_data(
    data: &UsageData,
    args: &UsageReportArgs,
    format: OutputFormat,
    title: &str,
) -> Result<()> {
    // Apply the same filter pipeline the host-side `print_report` uses,
    // but operate on the supplied snapshot directly instead of opening
    // the cache.
    let filters = build_filters_from_args(args);
    let view = if filters.is_empty() {
        data.clone()
    } else {
        crate::data::usage::filter_usage_data(data, &filters)
    };
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report_json(&view))?);
        }
        OutputFormat::Csv => {
            print!("{}", combined_csv(&view));
        }
        OutputFormat::Markdown => {
            print!("{}", render_markdown_report(title, &view, args.top));
        }
        OutputFormat::Text => {
            print_text_report(title, &view, args.top);
        }
    }
    Ok(())
}

fn print_status_with_data(
    data: &UsageData,
    args: &UsageReportArgs,
    format: OutputFormat,
) -> Result<()> {
    let projection = AppConfig::load()
        .unwrap_or_default()
        .usage
        .plan
        .as_ref()
        .map(|plan| {
            crate::data::usage::project_plan_usage(data, plan, Local::now().date_naive())
        });
    match format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "overview": data.overview(),
                "plan": projection,
            }))?
        ),
        OutputFormat::Csv => print!("{}", combined_csv(data)),
        OutputFormat::Markdown => {
            print!("{}", render_markdown_report("Usage Status", data, args.top));
        }
        OutputFormat::Text => {
            print_text_report("Usage Status", data, args.top);
        }
    }
    Ok(())
}

fn export_usage_with_data(
    data: &UsageData,
    args: &UsageExportArgs,
    format: OutputFormat,
) -> Result<()> {
    // Plugin-mode export. When the user passes `--output <path>` we
    // mirror the host's fs-write behaviour (path with extension =
    // single-file dump, path without extension = per-table CSV folder
    // matching the codeburn-style layout). With no `--output` we keep
    // the legacy stdout stream so shell pipelines (`| jq`, `| csvkit`)
    // continue to work.
    match (&args.output, format) {
        (Some(path), OutputFormat::Csv) => write_csv_dump(path, data),
        (Some(path), OutputFormat::Json) => {
            let text = serde_json::to_string_pretty(&json!({
                "schema": "ainb.usage.v1",
                "generated_at": Local::now().to_rfc3339(),
                "currency": "USD",
                "report": report_json(data),
            }))?;
            write_or_print(Some(path.as_path()), &text)
        }
        (Some(path), OutputFormat::Markdown) => {
            let text = render_markdown_report("Usage Export", data, args.report.top);
            write_or_print(Some(path.as_path()), &text)
        }
        (Some(path), OutputFormat::Text) => {
            let text = serde_json::to_string_pretty(&report_json(data))?;
            write_or_print(Some(path.as_path()), &text)
        }
        (None, OutputFormat::Json) => {
            println!("{}", serde_json::to_string_pretty(&report_json(data))?);
            Ok(())
        }
        (None, OutputFormat::Markdown) => {
            print!("{}", render_markdown_report("Usage Export", data, args.report.top));
            Ok(())
        }
        (None, OutputFormat::Csv | OutputFormat::Text) => {
            print!("{}", combined_csv(data));
            Ok(())
        }
    }
}

/// Pick between single-file vs per-table folder dump.
///
/// Decision matrix (matches user expectations regardless of path
/// shape — `mktemp -d` produces paths with what looks like a
/// file extension, so we can't rely on `extension().is_some()`):
///
/// * Path exists & is a directory → per-table folder dump.
/// * Path exists & is a regular file → overwrite as single inline CSV.
/// * Path doesn't exist & ends with `/` (or `\\`) → folder dump.
/// * Path doesn't exist & has a `.csv`/`.tsv`/`.txt` extension → single file.
/// * Path doesn't exist & otherwise → folder dump (codeburn convention:
///   `ainb usage export -o ./report` writes a directory).
fn write_csv_dump(path: &Path, data: &UsageData) -> Result<()> {
    if path.exists() && path.is_dir() {
        return write_csv_folder(path, data);
    }
    if !path.exists() {
        let raw = path.to_string_lossy();
        let ends_with_sep = raw.ends_with('/') || raw.ends_with(std::path::MAIN_SEPARATOR);
        // File-extension allow-list: anything outside this set is
        // assumed to be a directory name (matches the codeburn
        // convention `ainb usage export -o ./report`). Inverted vs an
        // older block-list approach so a stray `mktemp -d` path
        // (`tmp.pApLlhcpA5`) doesn't get treated as a file by accident.
        let looks_like_file = matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("csv") | Some("tsv") | Some("txt") | Some("tab")
        );
        if ends_with_sep || !looks_like_file {
            return write_csv_folder(path, data);
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, combined_csv(data))?;
    Ok(())
}

/// Write a codeburn-style per-table CSV export folder at `dir`.
///
/// Layout:
///
/// ```text
/// dir/
///   .ainb-export       (marker so we can safely overwrite next run)
///   README.txt         (plain-text index of which file holds what)
///   summary.csv
///   daily.csv
///   activity.csv
///   models.csv
///   projects.csv
///   sessions.csv
///   tools.csv          (only when data.tools is non-empty)
///   shell-commands.csv (only when data.shell_commands is non-empty)
///   mcp-servers.csv    (only when data.mcp_servers is non-empty)
/// ```
///
/// Refuses to write into a non-empty directory that lacks the
/// `.ainb-export` marker so a stray `ainb usage export -o ~/Documents`
/// doesn't clobber unrelated files. Empty dirs and previously-exported
/// dirs (marker present) are fine.
pub(crate) fn write_csv_folder(dir: &Path, data: &UsageData) -> Result<()> {
    let marker = dir.join(".ainb-export");
    if dir.exists() {
        let already_export_dir = marker.exists();
        let empty = fs::read_dir(dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        if !empty && !already_export_dir {
            bail!(
                "refusing to overwrite non-empty directory {} (no .ainb-export marker — \
                 pass an empty path or one previously written by `ainb usage export`)",
                dir.display()
            );
        }
        if already_export_dir {
            // Scrub the previous run's outputs so a shrunk dataset
            // (e.g. tools/shell_commands/mcp_servers now empty) doesn't
            // leave stale CSVs behind to mislead the next reader. Only
            // touch our own filenames — anything else stays put.
            scrub_export_dir(dir);
        }
    } else {
        fs::create_dir_all(dir)?;
    }
    fs::write(&marker, "ainb usage export\n")?;
    fs::write(dir.join("summary.csv"), summary_csv(data))?;
    fs::write(dir.join("daily.csv"), daily_csv(data))?;
    fs::write(dir.join("activity.csv"), activities_csv(&data.activities))?;
    fs::write(dir.join("models.csv"), models_csv(data))?;
    fs::write(dir.join("projects.csv"), projects_csv(&data.projects))?;
    fs::write(dir.join("sessions.csv"), sessions_csv(&data.sessions))?;
    let mut readme = String::from("ainb usage export\n=================\n\n");
    readme.push_str("summary.csv       Overview metrics (calls, tokens, cost).\n");
    readme.push_str("daily.csv         Per-day usage breakdown.\n");
    readme.push_str("activity.csv      Per-activity-category turn / retry / token counts.\n");
    readme.push_str("models.csv        Per-model call / token / cost rollups.\n");
    readme.push_str("projects.csv      Per-project bucket.\n");
    readme.push_str("sessions.csv      Per-session bucket.\n");
    if !data.tools.is_empty() {
        fs::write(dir.join("tools.csv"), named_csv("tool", &data.tools))?;
        readme.push_str("tools.csv         Per-tool invocation count.\n");
    }
    if !data.shell_commands.is_empty() {
        fs::write(
            dir.join("shell-commands.csv"),
            named_csv("command", &data.shell_commands),
        )?;
        readme.push_str("shell-commands.csv Per-shell-command invocation count.\n");
    }
    if !data.mcp_servers.is_empty() {
        fs::write(
            dir.join("mcp-servers.csv"),
            named_csv("server", &data.mcp_servers),
        )?;
        readme.push_str("mcp-servers.csv   Per-MCP-server invocation count.\n");
    }
    fs::write(dir.join("README.txt"), readme)?;
    Ok(())
}

/// Files our exporter is allowed to write inside an `.ainb-export`
/// folder. Anything matching here gets removed before a fresh export
/// so a shrunk dataset doesn't leave stale rows behind; anything else
/// the user has dropped into the folder is left untouched.
const EXPORT_OWNED_FILES: &[&str] = &[
    "summary.csv",
    "daily.csv",
    "activity.csv",
    "models.csv",
    "projects.csv",
    "sessions.csv",
    "tools.csv",
    "shell-commands.csv",
    "mcp-servers.csv",
    "README.txt",
];

fn scrub_export_dir(dir: &Path) {
    for name in EXPORT_OWNED_FILES {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// Translate the CLI's filter args into a [`UsageFilters`] struct so the
/// plugin path can reuse [`filter_usage_data`] without touching the
/// load-from-cache path.
fn build_filters_from_args(args: &UsageReportArgs) -> UsageFilters {
    UsageFilters {
        project: args.project.clone(),
        model: args.model.clone(),
        branch: args.branch.clone(),
        ..UsageFilters::default()
    }
}

/// Plugin-mode `optimize` — same shape as the host-side [`print_optimize`]
/// but skips the cache load (data is supplied).
fn print_optimize_with_data(
    data: &UsageData,
    args: &UsageReportArgs,
    format: OutputFormat,
) -> Result<()> {
    let filters = build_filters_from_args(args);
    let view = if filters.is_empty() {
        data.clone()
    } else {
        crate::data::usage::filter_usage_data(data, &filters)
    };
    let result = optimize_usage(&view);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Optimize Findings");
    println!("Health: {:?} ({}/100)", result.grade, result.score);
    println!(
        "Potential savings: {} tokens",
        result.potential_tokens_saved
    );
    for finding in &result.findings {
        println!(
            "- {:?}: {} ({})",
            finding.impact, finding.title, finding.details
        );
        for action in &finding.actions {
            match &action.command {
                Some(command) => println!("  Suggestion: {} -> {}", action.label, command),
                None => println!("  Suggestion: {}", action.label),
            }
        }
    }
    Ok(())
}

/// Plugin-mode `compare`.
fn print_compare_with_data(
    data: &UsageData,
    args: &UsageReportArgs,
    format: OutputFormat,
) -> Result<()> {
    let filters = build_filters_from_args(args);
    let view = if filters.is_empty() {
        data.clone()
    } else {
        crate::data::usage::filter_usage_data(data, &filters)
    };
    let result = compare_models(&view);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Model Compare");
    if let Some(winner) = &result.winner {
        println!("Leader: {winner}");
    }
    if result.low_data {
        println!("Low data: compare results need more sessions.");
    }
    for row in &result.models {
        println!(
            "- {}: {} calls, {} tokens/call, {} one-shot, {} retries",
            row.model, row.calls, row.tokens_per_call, row.one_shot_turns, row.retries
        );
    }
    Ok(())
}

/// Plugin-mode `yield`.
fn print_yield_with_data(
    data: &UsageData,
    args: &UsageReportArgs,
    format: OutputFormat,
) -> Result<()> {
    let filters = build_filters_from_args(args);
    let view = if filters.is_empty() {
        data.clone()
    } else {
        crate::data::usage::filter_usage_data(data, &filters)
    };
    let result = analyze_yield(&view);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Usage Yield");
    println!(
        "Productive: {} sessions (${:.2})",
        result.productive_sessions, result.productive_usd
    );
    println!(
        "Reverted: {} sessions (${:.2})",
        result.reverted_sessions, result.reverted_usd
    );
    println!(
        "Abandoned: {} sessions (${:.2})",
        result.abandoned_sessions, result.abandoned_usd
    );
    Ok(())
}

/// Plugin-mode `model-alias`. The host-side path persists aliases via
/// `AppConfig::save()` — filesystem write through the plugin's config
/// dir. Inside wasmi we have no arbitrary fs path access, so the
/// plugin path is reflection-only for now: lists empty for `--list`,
/// treats `--remove`/`from→to` as no-ops with an informational note.
/// Real persistence (via `cache_get`/`cache_put`) lands in a follow-up;
/// the test surface only asserts the subcommand routes through the
/// plugin and produces non-empty output.
fn model_alias_command_plugin(args: UsageModelAliasArgs) -> Result<()> {
    if args.list {
        println!("Model aliases (plugin scope): none configured.");
        return Ok(());
    }
    if let Some(remove) = args.remove {
        println!(
            "Model alias '{remove}' would be removed (plugin-mode persistence \
             pending — no-op)."
        );
        return Ok(());
    }
    match (args.from, args.to) {
        (Some(from), Some(to)) => {
            println!(
                "Model alias {from} -> {to} would be saved (plugin-mode \
                 persistence pending — no-op)."
            );
            Ok(())
        }
        _ => bail!("Use --list, --remove <model>, or <from> <to>"),
    }
}

pub async fn execute(command: UsageCommands, format: OutputFormat) -> Result<()> {
    match command {
        UsageCommands::Report(args) => print_report(&args, format, "Usage Report"),
        UsageCommands::Status(args) => print_status(&args, format),
        UsageCommands::Today(mut args) => {
            args.period = PeriodArg::Today;
            print_report(&args, format, "Today")
        }
        UsageCommands::Month(mut args) => {
            args.period = PeriodArg::Month;
            print_report(&args, format, "Month")
        }
        UsageCommands::Export(args) => export_usage(&args, format),
        UsageCommands::Plan { command } => plan_command(command, format),
        UsageCommands::Currency(args) => currency_command(args),
        UsageCommands::ModelAlias(args) => model_alias_command(args),
        UsageCommands::Optimize(args) => print_optimize(&args, format),
        UsageCommands::Compare(args) => print_compare(&args, format),
        UsageCommands::Yield(args) => print_yield(&args, format),
        UsageCommands::Cache { command } => cache_command(command, format),
        UsageCommands::Models(args) => {
            let data = load_usage(&args.report)?;
            print_models_with_data(&data, &args, format)
        }
    }
}

/// `usage models` — flat per-model rollup by default, per-model × per-
/// activity-category matrix when `--by-task` is set. Surfaces in
/// text / json / csv / markdown.
fn print_models_with_data(
    data: &UsageData,
    args: &UsageModelsArgs,
    format: OutputFormat,
) -> Result<()> {
    let filters = build_filters_from_args(&args.report);
    let view = if filters.is_empty() {
        data.clone()
    } else {
        crate::data::usage::filter_usage_data(data, &filters)
    };
    if !args.by_task {
        return print_models_flat(&view, format, args.report.top);
    }
    let matrix = build_models_by_task_matrix(&view);
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&matrix_to_json(&matrix))?);
        }
        OutputFormat::Csv => {
            print!("{}", matrix_to_csv(&matrix));
        }
        OutputFormat::Markdown => {
            print!("{}", matrix_to_markdown(&matrix));
        }
        OutputFormat::Text => {
            print!("{}", matrix_to_text(&matrix));
        }
    }
    Ok(())
}

fn print_models_flat(data: &UsageData, format: OutputFormat, top: usize) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let rows: Vec<_> = top_iter(&data.models, top)
                .map(|m| {
                    json!({
                        "model": m.model,
                        "calls": m.bucket.call_count,
                        "tokens": m.bucket.total(),
                        "cost_usd": m.bucket.cost_usd,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json!({"models": rows}))?);
        }
        OutputFormat::Csv => {
            println!("model,calls,tokens,cost_usd");
            for m in top_iter(&data.models, top) {
                println!(
                    "{},{},{},{:.4}",
                    csv_quote(&m.model),
                    m.bucket.call_count,
                    m.bucket.total(),
                    m.bucket.cost_usd.unwrap_or(0.0)
                );
            }
        }
        OutputFormat::Markdown => {
            println!("# Models\n");
            println!("| Model | Calls | Tokens | Cost |");
            println!("|-------|------:|-------:|-----:|");
            for m in top_iter(&data.models, top) {
                println!(
                    "| {} | {} | {} | {} |",
                    md_escape(&m.model),
                    m.bucket.call_count,
                    m.bucket.total(),
                    format_cost(m.bucket.cost_usd)
                );
            }
        }
        OutputFormat::Text => {
            print_top_models(data, top);
        }
    }
    Ok(())
}

/// One row of the per-model × per-activity matrix.
#[derive(Debug)]
struct ModelTaskRow {
    model: String,
    /// Same column order as [`ActivityCategory`]'s display list.
    cells: Vec<ModelTaskCell>,
}

#[derive(Debug, Default, Clone, Copy)]
struct ModelTaskCell {
    calls: usize,
    tokens: u64,
    cost_usd: f64,
}

/// Build the matrix from raw `data.activities` + per-call provider
/// model attribution. Caveat: `UsageData` aggregates activities ×
/// models in a flat list; this helper buckets the global model list
/// against each ActivityCategory and emits zeros where data is sparse.
fn build_models_by_task_matrix(data: &UsageData) -> (Vec<String>, Vec<ModelTaskRow>) {
    use crate::data::usage::{ActivityCategory, classify_activity};
    use std::collections::BTreeMap;

    // Column order is deterministic across runs.
    let categories: Vec<ActivityCategory> = vec![
        ActivityCategory::Coding,
        ActivityCategory::Debugging,
        ActivityCategory::Feature,
        ActivityCategory::Refactoring,
        ActivityCategory::Testing,
        ActivityCategory::Exploration,
        ActivityCategory::Planning,
        ActivityCategory::Delegation,
        ActivityCategory::Git,
        ActivityCategory::BuildDeploy,
        ActivityCategory::Brainstorming,
        ActivityCategory::Conversation,
        ActivityCategory::General,
    ];
    let column_labels: Vec<String> = categories.iter().map(|c| c.label().to_string()).collect();

    // model → (category-index → cell)
    let mut by_model: BTreeMap<String, Vec<ModelTaskCell>> = BTreeMap::new();
    for call in &data.calls {
        let category = classify_activity(call);
        if let Some(col) = categories.iter().position(|c| *c == category) {
            let row = by_model
                .entry(call.model.clone())
                .or_insert_with(|| vec![ModelTaskCell::default(); categories.len()]);
            row[col].calls += 1;
            row[col].tokens += call.input_tokens
                + call.cache_creation_tokens
                + call.cache_read_tokens
                + call.output_tokens
                + call.reasoning_tokens;
            if let Some(cost) = call.cost_usd {
                row[col].cost_usd += cost;
            }
        }
    }

    let rows: Vec<ModelTaskRow> = by_model
        .into_iter()
        .map(|(model, cells)| ModelTaskRow { model, cells })
        .collect();

    (column_labels, rows)
}

fn matrix_to_json(matrix: &(Vec<String>, Vec<ModelTaskRow>)) -> serde_json::Value {
    let (cols, rows) = matrix;
    let rows_json: Vec<_> = rows
        .iter()
        .map(|row| {
            let cells: Vec<_> = row
                .cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    json!({
                        "activity": cols[i],
                        "calls": c.calls,
                        "tokens": c.tokens,
                        "cost_usd": c.cost_usd,
                    })
                })
                .collect();
            json!({ "model": row.model, "by_task": cells })
        })
        .collect();
    json!({
        "schema": "ainb.usage.models_by_task.v1",
        "columns": cols,
        "rows": rows_json,
    })
}

fn matrix_to_csv(matrix: &(Vec<String>, Vec<ModelTaskRow>)) -> String {
    let (cols, rows) = matrix;
    let mut out = String::from("model");
    for col in cols {
        out.push_str(&format!(",{}_calls,{}_tokens,{}_cost_usd", col, col, col));
    }
    out.push('\n');
    for row in rows {
        out.push_str(&csv_quote(&row.model));
        for c in &row.cells {
            out.push_str(&format!(",{},{},{:.4}", c.calls, c.tokens, c.cost_usd));
        }
        out.push('\n');
    }
    out
}

fn matrix_to_markdown(matrix: &(Vec<String>, Vec<ModelTaskRow>)) -> String {
    let (cols, rows) = matrix;
    let mut out = String::from("# Models by Task\n\n");
    out.push_str("| Model");
    for col in cols {
        out.push_str(&format!(" | {} calls | {} tokens | {} cost", col, col, col));
    }
    out.push_str(" |\n|------");
    for _ in cols {
        out.push_str("|------:|-------:|------:");
    }
    out.push_str("|\n");
    for row in rows {
        out.push_str(&format!("| {}", md_escape(&row.model)));
        for c in &row.cells {
            out.push_str(&format!(" | {} | {} | ${:.2}", c.calls, c.tokens, c.cost_usd));
        }
        out.push_str(" |\n");
    }
    out
}

fn matrix_to_text(matrix: &(Vec<String>, Vec<ModelTaskRow>)) -> String {
    let (cols, rows) = matrix;
    let mut out = String::from("Models by Task\n");
    for row in rows {
        out.push_str(&format!("{}\n", row.model));
        for (i, c) in row.cells.iter().enumerate() {
            if c.calls == 0 && c.tokens == 0 {
                continue;
            }
            out.push_str(&format!(
                "  {}: {} calls, {} tokens, ${:.2}\n",
                cols[i], c.calls, c.tokens, c.cost_usd
            ));
        }
    }
    out
}

fn csv_quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn cache_command(command: UsageCacheCommands, format: OutputFormat) -> Result<()> {
    match command {
        UsageCacheCommands::Info => {
            let cache = shared_cache();
            let info = cache.info().map_err(|e| anyhow!("usage cache info failed: {e}"))?;
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "db_path": info.db_path,
                            "size_bytes": info.size_bytes,
                            "file_count": info.file_count,
                            "oldest_updated_at": info.oldest_updated_at,
                            "enabled": cache.is_enabled(),
                        }))?
                    );
                }
                _ => {
                    println!("Usage cache");
                    println!("  Path:           {}", info.db_path.display());
                    println!("  Size on disk:   {} bytes", info.size_bytes);
                    println!("  File count:     {}", info.file_count);
                    println!(
                        "  Oldest entry:   {}",
                        info.oldest_updated_at
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "—".to_string())
                    );
                    println!(
                        "  Enabled:        {}",
                        if cache.is_enabled() { "yes" } else { "no" }
                    );
                }
            }
            Ok(())
        }
        UsageCacheCommands::Clear => {
            let cache = shared_cache();
            cache.clear().map_err(|e| anyhow!("usage cache clear failed: {e}"))?;
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({"cleared": true}))?
                    );
                }
                _ => println!("Usage cache cleared."),
            }
            Ok(())
        }
    }
}

fn print_report(args: &UsageReportArgs, format: OutputFormat, title: &str) -> Result<()> {
    let data = load_usage(args)?;
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report_json(&data))?);
        }
        OutputFormat::Csv => {
            print!("{}", combined_csv(&data));
        }
        OutputFormat::Markdown => {
            print!("{}", render_markdown_report(title, &data, args.top));
        }
        OutputFormat::Text => {
            print_text_report(title, &data, args.top);
        }
    }
    Ok(())
}

fn print_status(args: &UsageReportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(args)?;
    let config = AppConfig::load().unwrap_or_default();
    let projection = config.usage.plan.as_ref().map(|plan| {
        crate::data::usage::project_plan_usage(&data, plan, Local::now().date_naive())
    });
    match format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "overview": data.overview(),
                "plan": projection,
            }))?
        ),
        OutputFormat::Csv => print!("{}", combined_csv(&data)),
        OutputFormat::Markdown => {
            print!("{}", render_markdown_report("Usage Status", &data, args.top));
            if let Some(projection) = &projection {
                println!(
                    "\n## Plan\n\n- **Spent:** ${:.2} / ${:.2} ({:.0}%)\n- **Status:** {:?}\n",
                    projection.spent_usd,
                    projection.monthly_usd,
                    projection.percent_used * 100.0,
                    projection.status
                );
            }
        }
        OutputFormat::Text => {
            print_text_report("Usage Status", &data, args.top);
            if let Some(projection) = projection {
                println!(
                    "Plan: ${:.2} / ${:.2} ({:.0}%) {:?}",
                    projection.spent_usd,
                    projection.monthly_usd,
                    projection.percent_used * 100.0,
                    projection.status
                );
            }
        }
    }
    Ok(())
}

fn print_optimize(args: &UsageReportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(args)?;
    let result = optimize_usage(&data);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Optimize Findings");
    println!("Health: {:?} ({}/100)", result.grade, result.score);
    println!(
        "Potential savings: {} tokens",
        result.potential_tokens_saved
    );
    for finding in &result.findings {
        println!(
            "- {:?}: {} ({})",
            finding.impact, finding.title, finding.details
        );
        for action in &finding.actions {
            match &action.command {
                Some(command) => println!("  Suggestion: {} -> {}", action.label, command),
                None => println!("  Suggestion: {}", action.label),
            }
        }
    }
    Ok(())
}

fn print_compare(args: &UsageReportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(args)?;
    let result = compare_models(&data);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Model Compare");
    if let Some(winner) = &result.winner {
        println!("Leader: {winner}");
    }
    if result.low_data {
        println!("Low data: compare results need more sessions.");
    }
    for row in &result.models {
        println!(
            "- {}: {} calls, {} tokens/call, {} one-shot, {} retries",
            row.model, row.calls, row.tokens_per_call, row.one_shot_turns, row.retries
        );
    }
    Ok(())
}

fn print_yield(args: &UsageReportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(args)?;
    let result = analyze_yield(&data);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!("Usage Yield");
    println!(
        "Productive: {} sessions (${:.2})",
        result.productive_sessions, result.productive_usd
    );
    println!(
        "Reverted: {} sessions (${:.2})",
        result.reverted_sessions, result.reverted_usd
    );
    println!(
        "Abandoned: {} sessions (${:.2})",
        result.abandoned_sessions, result.abandoned_usd
    );
    Ok(())
}

fn export_usage(args: &UsageExportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(&args.report)?;
    match format {
        OutputFormat::Json => {
            let text = serde_json::to_string_pretty(&json!({
                "schema": "ainb.usage.v1",
                "generated_at": Local::now().to_rfc3339(),
                "currency": "USD",
                "report": report_json(&data),
            }))?;
            write_or_print(args.output.as_deref(), &text)
        }
        OutputFormat::Markdown => {
            let text = render_markdown_report("Usage Export", &data, args.report.top);
            write_or_print(args.output.as_deref(), &text)
        }
        OutputFormat::Text => {
            let text = serde_json::to_string_pretty(&report_json(&data))?;
            write_or_print(args.output.as_deref(), &text)
        }
        OutputFormat::Csv => {
            if let Some(path) = &args.output {
                write_csv_dump(path, &data)
            } else {
                print!("{}", combined_csv(&data));
                Ok(())
            }
        }
    }
}

fn plan_command(command: UsagePlanCommands, format: OutputFormat) -> Result<()> {
    let mut config = AppConfig::load().unwrap_or_default();
    match command {
        UsagePlanCommands::Show(args) => {
            let today = Local::now().date_naive();
            let data = if let Some(plan) = config.usage.plan.as_ref() {
                load_usage(&plan_show_args_for_plan(&args, plan, today))?
            } else {
                load_usage(&args)?
            };
            let projection = config
                .usage
                .plan
                .as_ref()
                .map(|plan| crate::data::usage::project_plan_usage(&data, plan, today));
            if matches!(format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "plan": config.usage.plan,
                        "projection": projection,
                    }))?
                );
            } else if let Some(plan) = &config.usage.plan {
                println!(
                    "Plan: {:?} ${:.2}/month reset day {}",
                    plan.id, plan.monthly_usd, plan.reset_day
                );
                if let Some(projection) = projection {
                    println!(
                        "Spend: ${:.2}, projected ${:.2}, {:?}",
                        projection.spent_usd, projection.projected_usd, projection.status
                    );
                }
            } else {
                println!("No usage plan configured.");
            }
        }
        UsagePlanCommands::Set {
            plan,
            monthly_usd,
            provider,
            reset_day,
        } => {
            let id = plan_id(plan);
            let monthly_usd = monthly_usd
                .or_else(|| id.monthly_usd())
                .ok_or_else(|| anyhow!("custom plan requires --monthly-usd"))?;
            config.usage.plan = Some(UsagePlan {
                id,
                monthly_usd,
                provider: plan_provider(provider),
                reset_day: reset_day.clamp(1, 28),
                set_at: Local::now().to_rfc3339(),
            });
            config.save()?;
            println!("Usage plan saved.");
        }
        UsagePlanCommands::Reset => {
            config.usage.plan = None;
            config.save()?;
            println!("Usage plan reset.");
        }
        UsagePlanCommands::Detect => {
            bail!("Plan detection unavailable. Use `ainb usage plan set <plan>`.");
        }
    }
    Ok(())
}

fn currency_command(args: UsageCurrencyArgs) -> Result<()> {
    let mut config = AppConfig::load().unwrap_or_default();
    if args.reset {
        config.usage.currency = CurrencyConfig::default();
    } else {
        let Some(code) = args.code else {
            println!(
                "{} {}",
                config.usage.currency.symbol, config.usage.currency.code
            );
            return Ok(());
        };
        let code = code.to_uppercase();
        if code.len() != 3 || !code.chars().all(|ch| ch.is_ascii_uppercase()) {
            bail!("Currency code must be a 3-letter ISO-style code");
        }
        config.usage.currency.code = code.clone();
        config.usage.currency.symbol = args.symbol.unwrap_or(code);
    }
    config.save()?;
    println!("Currency saved.");
    Ok(())
}

fn model_alias_command(args: UsageModelAliasArgs) -> Result<()> {
    let mut config = AppConfig::load().unwrap_or_default();
    if args.list {
        for (from, to) in &config.usage.model_aliases {
            println!("{from} -> {to}");
        }
        return Ok(());
    }
    if let Some(remove) = args.remove {
        config.usage.model_aliases.remove(&remove);
        config.save()?;
        println!("Model alias removed.");
        return Ok(());
    }
    match (args.from, args.to) {
        (Some(from), Some(to)) => {
            config.usage.model_aliases.insert(from, to);
            config.save()?;
            println!("Model alias saved.");
            Ok(())
        }
        _ => bail!("Use --list, --remove <model>, or <from> <to>"),
    }
}

fn load_usage(args: &UsageReportArgs) -> Result<UsageData> {
    let query = query_from_args(args)?;
    if args.no_cache {
        Ok(parse_usage_for_with_roots_and_cache(
            query,
            &UsageSourceRoots::default(),
            disabled_cache(),
        ))
    } else {
        Ok(parse_usage_for(query))
    }
}

fn query_from_args(args: &UsageReportArgs) -> Result<UsageQuery> {
    // Period precedence (top wins; clap rejects combinations via the
    // conflicts_with_all groups on UsageReportArgs):
    //   --month YYYY-MM            -> SpecificMonth
    //   --quarter YYYY-Qn          -> SpecificQuarter
    //   --last-n-days N            -> LastNDays(N)
    //   --ytd                      -> YearToDate
    //   --from / --to              -> Custom (existing free-text behaviour)
    //   --period {today|week|30days|month|all}  (default)
    let period = if let Some(month_str) = &args.month {
        parse_month_arg(month_str)?
    } else if let Some(quarter_str) = &args.quarter {
        parse_quarter_arg(quarter_str)?
    } else if let Some(n) = args.last_n_days {
        if n == 0 {
            bail!("--last-n-days must be >= 1");
        }
        UsagePeriod::LastNDays(n)
    } else if args.ytd {
        UsagePeriod::YearToDate
    } else if args.from.is_some() || args.to.is_some() {
        let from = match &args.from {
            Some(value) => parse_date(value)?,
            None => NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid date"),
        };
        let to = match &args.to {
            Some(value) => parse_date(value)?,
            None => Local::now().date_naive(),
        };
        if from > to {
            bail!("from date must be before or equal to to date");
        }
        UsagePeriod::Custom { from, to }
    } else {
        match args.period {
            PeriodArg::Today => UsagePeriod::Today,
            PeriodArg::Week => UsagePeriod::Week,
            PeriodArg::ThirtyDays => UsagePeriod::ThirtyDays,
            PeriodArg::Month => UsagePeriod::Month,
            PeriodArg::All => UsagePeriod::All,
        }
    };

    Ok(UsageQuery {
        period,
        provider_filter: match args.provider {
            ProviderArg::All => UsageProviderFilter::All,
            ProviderArg::Claude => UsageProviderFilter::Claude,
            ProviderArg::Codex => UsageProviderFilter::Codex,
            ProviderArg::Cursor => UsageProviderFilter::Cursor,
            ProviderArg::Copilot => UsageProviderFilter::Copilot,
            ProviderArg::Gemini => UsageProviderFilter::Gemini,
        },
        include_projects: args.include.clone(),
        exclude_projects: args.exclude.clone(),
        filters: UsageFilters {
            project: args.project.clone(),
            model: args.model.clone(),
            activity: args.activity.clone(),
            session: args.session.clone(),
            branch: args.branch.clone(),
            // No CLI surface for exclude filters yet — populated only
            // via the TUI X-on-row picker.
            ..UsageFilters::default()
        },
    })
}

fn parse_date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid date `{value}`, expected YYYY-MM-DD"))
}

/// Parse `--month YYYY-MM` into `UsagePeriod::SpecificMonth(first-of-month)`.
fn parse_month_arg(value: &str) -> Result<UsagePeriod> {
    // chrono's NaiveDate parser wants a day; pin to `-01`.
    let with_day = format!("{value}-01");
    let date = NaiveDate::parse_from_str(&with_day, "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid --month `{value}`, expected YYYY-MM"))?;
    Ok(UsagePeriod::SpecificMonth(date))
}

/// Parse `--quarter YYYY-Qn` into `UsagePeriod::SpecificQuarter(year, q)`.
fn parse_quarter_arg(value: &str) -> Result<UsagePeriod> {
    let err = || anyhow!("invalid --quarter `{value}`, expected YYYY-Qn (n=1..=4)");
    // Accept either case for the Q.
    let normalised = value.to_ascii_uppercase();
    let (year_part, q_part) = normalised.split_once("-Q").ok_or_else(err)?;
    let year: i32 = year_part.parse().map_err(|_| err())?;
    let q: u8 = q_part.parse().map_err(|_| err())?;
    if !(1..=4).contains(&q) {
        return Err(err());
    }
    Ok(UsagePeriod::SpecificQuarter(year, q))
}

fn print_text_report(title: &str, data: &UsageData, top: usize) {
    println!("{title}");
    println!(
        "Overview: {} calls, {} sessions, {} projects, {} tokens, {}",
        data.grand_total.call_count,
        data.grand_total.session_count,
        data.grand_total.project_count,
        data.grand_total.total(),
        format_cost(data.grand_total.cost_usd)
    );
    println!();
    print_top_projects(data, top);
    print_top_activities(data, top);
    print_top_models(data, top);
}

/// `top == 0` is treated as "no cap" — emit every row. Anything > 0
/// is the slice length passed to `.take(...)`.
fn top_iter<T>(slice: &[T], top: usize) -> impl Iterator<Item = &T> {
    let n = if top == 0 { slice.len() } else { top };
    slice.iter().take(n)
}

fn print_top_projects(data: &UsageData, top: usize) {
    println!("By Project");
    for project in top_iter(&data.projects, top) {
        println!(
            "- {}: {} calls, {} tokens, {}",
            project.name,
            project.bucket.call_count,
            project.bucket.total(),
            format_cost(project.bucket.cost_usd)
        );
    }
}

fn print_top_activities(data: &UsageData, top: usize) {
    println!("By Activity");
    for activity in top_iter(&data.activities, top) {
        println!(
            "- {}: {} turns, {} retries, {} tokens",
            activity.category.label(),
            activity.turns,
            activity.retries,
            activity.bucket.total()
        );
    }
}

fn print_top_models(data: &UsageData, top: usize) {
    println!("By Model");
    for model in top_iter(&data.models, top) {
        println!(
            "- {}: {} calls, {} tokens, {}",
            model.model,
            model.bucket.call_count,
            model.bucket.total(),
            format_cost(model.bucket.cost_usd)
        );
    }
}

/// Render a `UsageData` snapshot as GitHub-flavored markdown.
///
/// Mirrors the text renderer's structure (overview line + three "By X"
/// sections) but uses `#`/`##` headings and pipe tables so the output
/// pastes cleanly into READMEs, PR descriptions, and grafana note
/// panels. Pairs with `--format markdown` (also accepts `md` as an
/// argv alias for shell-friendliness).
pub(crate) fn render_markdown_report(title: &str, data: &UsageData, top: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str("## Overview\n\n");
    out.push_str(&format!(
        "- **Calls:** {}\n- **Sessions:** {}\n- **Projects:** {}\n- **Tokens:** {}\n- **Cost:** {}\n\n",
        data.grand_total.call_count,
        data.grand_total.session_count,
        data.grand_total.project_count,
        data.grand_total.total(),
        format_cost(data.grand_total.cost_usd)
    ));
    out.push_str("## By Project\n\n");
    out.push_str("| Project | Calls | Tokens | Cost |\n");
    out.push_str("|---------|------:|-------:|-----:|\n");
    for project in top_iter(&data.projects, top) {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            md_escape(&truncate_label(&project.name, 60)),
            project.bucket.call_count,
            project.bucket.total(),
            format_cost(project.bucket.cost_usd)
        ));
    }
    out.push('\n');
    out.push_str("## By Activity\n\n");
    out.push_str("| Activity | Turns | Retries | Tokens |\n");
    out.push_str("|----------|------:|--------:|-------:|\n");
    for activity in top_iter(&data.activities, top) {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            md_escape(activity.category.label()),
            activity.turns,
            activity.retries,
            activity.bucket.total()
        ));
    }
    out.push('\n');
    out.push_str("## By Model\n\n");
    out.push_str("| Model | Calls | Tokens | Cost |\n");
    out.push_str("|-------|------:|-------:|-----:|\n");
    for model in top_iter(&data.models, top) {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            md_escape(&model.model),
            model.bucket.call_count,
            model.bucket.total(),
            format_cost(model.bucket.cost_usd)
        ));
    }
    out
}

/// Escape characters that have semantic meaning inside a markdown
/// table cell. Only `|` and backticks; other markdown specials are
/// rare in model/project/activity labels and render fine inline.
fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('`', "\\`")
}

/// Char-aware truncation that keeps multi-byte chars intact and
/// appends an ellipsis when truncated. Used to stop long
/// directory-flattened project paths (e.g.
/// `-Users-stevengonsalvez--agents-in-a-box-worktrees-...`) from
/// blowing out the width of markdown tables when pasted into PR
/// descriptions and grafana note panels.
fn truncate_label(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let head: String = s.chars().take(keep).collect();
    format!("{head}…")
}

pub(crate) fn report_json(data: &UsageData) -> serde_json::Value {
    json!({
        "overview": data.overview(),
        "daily": data.daily,
        "projects": data.projects,
        "models": data.models,
        "activities": data.activities,
        "sessions": data.sessions,
        "tools": data.tools,
        "shell_commands": data.shell_commands,
        "mcp_servers": data.mcp_servers,
    })
}

fn write_or_print(path: Option<&Path>, text: &str) -> Result<()> {
    if let Some(path) = path {
        fs::write(path, text)?;
    } else {
        println!("{text}");
    }
    Ok(())
}

fn combined_csv(data: &UsageData) -> String {
    [
        summary_csv(data),
        daily_csv(data),
        projects_csv(&data.projects),
        sessions_csv(&data.sessions),
        activities_csv(&data.activities),
        models_csv(data),
        named_csv("tool", &data.tools),
        named_csv("command", &data.shell_commands),
        named_csv("server", &data.mcp_servers),
    ]
    .join("\n")
}

fn summary_csv(data: &UsageData) -> String {
    format!(
        "section,metric,value\nsummary,calls,{}\nsummary,sessions,{}\nsummary,projects,{}\nsummary,tokens,{}\nsummary,cost_usd,{}\n",
        data.grand_total.call_count,
        data.grand_total.session_count,
        data.grand_total.project_count,
        data.grand_total.total(),
        data.grand_total.cost_usd.unwrap_or(0.0)
    )
}

fn daily_csv(data: &UsageData) -> String {
    let mut out = "date,calls,sessions,projects,tokens,cost_usd\n".to_string();
    for (date, bucket) in &data.daily {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            date,
            bucket.call_count,
            bucket.session_count,
            bucket.project_count,
            bucket.total(),
            bucket.cost_usd.unwrap_or(0.0)
        ));
    }
    out
}

fn projects_csv(projects: &[ProjectUsage]) -> String {
    let mut out = "project,path,calls,sessions,tokens,cost_usd\n".to_string();
    for project in projects {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            csv_cell(&project.name),
            csv_cell(&project.path),
            project.bucket.call_count,
            project.bucket.session_count,
            project.bucket.total(),
            project.bucket.cost_usd.unwrap_or(0.0)
        ));
    }
    out
}

fn sessions_csv(sessions: &[SessionUsage]) -> String {
    let mut out = "provider,project,session_id,first,last,calls,tokens,cost_usd\n".to_string();
    for session in sessions {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_cell(&session.provider),
            csv_cell(&session.project),
            csv_cell(&session.session_id),
            // Emit RFC3339 in the user's local offset. The instant is
            // the same as the Utc-stored value; rendering with the
            // local offset matches what the TUI shows so a CSV row
            // round-trips visibly to the user's clock.
            session.first_timestamp.with_timezone(&chrono::Local).to_rfc3339(),
            session.last_timestamp.with_timezone(&chrono::Local).to_rfc3339(),
            session.bucket.call_count,
            session.bucket.total(),
            session.bucket.cost_usd.unwrap_or(0.0)
        ));
    }
    out
}

fn activities_csv(activities: &[ActivityUsage]) -> String {
    let mut out = "activity,turns,retries,edit_turns,one_shot_turns,tokens,cost_usd\n".to_string();
    for activity in activities {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            activity.category.label(),
            activity.turns,
            activity.retries,
            activity.edit_turns,
            activity.one_shot_turns,
            activity.bucket.total(),
            activity.bucket.cost_usd.unwrap_or(0.0)
        ));
    }
    out
}

fn models_csv(data: &UsageData) -> String {
    let mut out = "model,calls,tokens,cost_usd\n".to_string();
    for model in &data.models {
        out.push_str(&format!(
            "{},{},{},{}\n",
            csv_cell(&model.model),
            model.bucket.call_count,
            model.bucket.total(),
            model.bucket.cost_usd.unwrap_or(0.0)
        ));
    }
    out
}

fn named_csv(name_header: &str, rows: &[NamedUsage]) -> String {
    let mut out = format!("{name_header},calls\n");
    for row in rows {
        out.push_str(&format!("{},{}\n", csv_cell(&row.name), row.calls));
    }
    out
}

fn csv_cell(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    let protected = if escaped
        .chars()
        .next()
        .is_some_and(|ch| matches!(ch, '\t' | '\r' | '=' | '+' | '-' | '@'))
    {
        format!("'{escaped}")
    } else {
        escaped
    };
    format!("\"{protected}\"")
}

fn format_cost(cost: Option<f64>) -> String {
    cost.map(|value| format!("${value:.2}"))
        .unwrap_or_else(|| "cost unavailable".to_string())
}

fn plan_id(plan: PlanArg) -> UsagePlanId {
    match plan {
        PlanArg::ClaudePro => UsagePlanId::ClaudePro,
        PlanArg::ClaudeMax => UsagePlanId::ClaudeMax,
        PlanArg::ClaudeMax5x => UsagePlanId::ClaudeMax5x,
        PlanArg::CursorPro => UsagePlanId::CursorPro,
        PlanArg::Custom => UsagePlanId::Custom,
        PlanArg::None => UsagePlanId::None,
    }
}

fn plan_provider(provider: PlanProviderArg) -> UsagePlanProvider {
    match provider {
        PlanProviderArg::All => UsagePlanProvider::All,
        PlanProviderArg::Claude => UsagePlanProvider::Claude,
        PlanProviderArg::Codex => UsagePlanProvider::Codex,
        PlanProviderArg::Cursor => UsagePlanProvider::Cursor,
    }
}

fn provider_arg_for_plan(provider: UsagePlanProvider) -> ProviderArg {
    match provider {
        UsagePlanProvider::All | UsagePlanProvider::Cursor => ProviderArg::All,
        UsagePlanProvider::Claude => ProviderArg::Claude,
        UsagePlanProvider::Codex => ProviderArg::Codex,
    }
}

fn plan_show_args_for_plan(
    args: &UsageReportArgs,
    plan: &UsagePlan,
    today: NaiveDate,
) -> UsageReportArgs {
    let (from, to) = billing_period(today, plan.reset_day);
    let mut scoped_args = args.clone();
    scoped_args.period = PeriodArg::All;
    scoped_args.from = Some(from.to_string());
    scoped_args.to = Some(to.to_string());
    scoped_args.provider = provider_arg_for_plan(plan.provider);
    scoped_args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cell_escapes_formula_starts() {
        assert_eq!(csv_cell("=cmd"), "\"'=cmd\"");
        assert_eq!(csv_cell("@user"), "\"'@user\"");
        assert_eq!(csv_cell("safe"), "\"safe\"");
    }

    #[test]
    fn report_json_keeps_expected_top_level_sections() {
        let data = UsageData::default();
        let json = report_json(&data);

        for key in [
            "overview",
            "daily",
            "projects",
            "models",
            "activities",
            "sessions",
            "tools",
            "shell_commands",
            "mcp_servers",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn combined_csv_includes_export_section_headers() {
        let csv = combined_csv(&UsageData::default());

        for header in [
            "section,metric,value",
            "date,calls,sessions,projects,tokens,cost_usd",
            "project,path,calls,sessions,tokens,cost_usd",
            "provider,project,session_id,first,last,calls,tokens,cost_usd",
            "activity,turns,retries,edit_turns,one_shot_turns,tokens,cost_usd",
            "model,calls,tokens,cost_usd",
            "tool,calls",
            "command,calls",
            "server,calls",
        ] {
            assert!(csv.contains(header), "missing {header}");
        }
    }

    #[test]
    fn month_flag_yields_specific_month_period() {
        let args = UsageReportArgs {
            month: Some("2026-04".into()),
            ..UsageReportArgs::default()
        };
        let q = query_from_args(&args).unwrap();
        match q.period {
            UsagePeriod::SpecificMonth(d) => {
                assert_eq!(d, NaiveDate::from_ymd_opt(2026, 4, 1).unwrap())
            }
            other => panic!("expected SpecificMonth, got {other:?}"),
        }
    }

    #[test]
    fn quarter_flag_yields_specific_quarter_period() {
        let args = UsageReportArgs {
            quarter: Some("2026-Q2".into()),
            ..UsageReportArgs::default()
        };
        let q = query_from_args(&args).unwrap();
        assert!(matches!(q.period, UsagePeriod::SpecificQuarter(2026, 2)));
    }

    #[test]
    fn quarter_flag_lowercase_q_is_accepted() {
        let args = UsageReportArgs {
            quarter: Some("2026-q3".into()),
            ..UsageReportArgs::default()
        };
        let q = query_from_args(&args).unwrap();
        assert!(matches!(q.period, UsagePeriod::SpecificQuarter(2026, 3)));
    }

    #[test]
    fn quarter_flag_rejects_q5() {
        let args = UsageReportArgs {
            quarter: Some("2026-Q5".into()),
            ..UsageReportArgs::default()
        };
        let err = query_from_args(&args).unwrap_err().to_string();
        assert!(err.contains("YYYY-Qn"));
    }

    #[test]
    fn last_n_days_flag_yields_last_n_days_period() {
        let args = UsageReportArgs {
            last_n_days: Some(14),
            ..UsageReportArgs::default()
        };
        let q = query_from_args(&args).unwrap();
        assert!(matches!(q.period, UsagePeriod::LastNDays(14)));
    }

    #[test]
    fn last_n_days_zero_is_rejected() {
        let args = UsageReportArgs {
            last_n_days: Some(0),
            ..UsageReportArgs::default()
        };
        assert!(query_from_args(&args).is_err());
    }

    // Three clap-parse-time tests (month-vs-quarter, last-n-days-vs-ytd,
    // from-vs-month) lived here in core, exercising the *root* `ainb`
    // clap definition (`crate::cli::registry::CommandRegistry` +
    // `crate::cli::root_clap_command`) which is host-side. From the
    // plugin perspective those flags are still mutually exclusive (the
    // `#[command(group = ...)]` attributes on UsageReportArgs enforce
    // that locally), but the registry-level test belongs on the host
    // side. Re-staged as host integration tests in
    // `crates/ainb-core/tests/plugin_burndown_cli.rs` once the host
    // CLI registry dispatches into the plugin (Phase 3 host wiring).

    #[test]
    fn ytd_flag_yields_year_to_date_period() {
        let args = UsageReportArgs {
            ytd: true,
            ..UsageReportArgs::default()
        };
        let q = query_from_args(&args).unwrap();
        assert!(matches!(q.period, UsagePeriod::YearToDate));
    }

    #[test]
    fn invalid_date_returns_clear_error() {
        let args = UsageReportArgs {
            from: Some("2026/04/01".to_string()),
            ..UsageReportArgs::default()
        };
        let err = query_from_args(&args).unwrap_err().to_string();
        assert!(err.contains("expected YYYY-MM-DD"));
    }

    #[test]
    fn inverted_date_range_errors() {
        let args = UsageReportArgs {
            from: Some("2026-04-02".to_string()),
            to: Some("2026-04-01".to_string()),
            ..UsageReportArgs::default()
        };
        let err = query_from_args(&args).unwrap_err().to_string();
        assert!(err.contains("from date"));
    }

    #[test]
    fn plan_show_uses_billing_window_and_plan_provider() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 29).unwrap();
        let plan = UsagePlan {
            id: UsagePlanId::ClaudePro,
            monthly_usd: 20.0,
            provider: UsagePlanProvider::Claude,
            reset_day: 12,
            set_at: "2026-04-29T00:00:00Z".to_string(),
        };

        let scoped = plan_show_args_for_plan(&UsageReportArgs::default(), &plan, today);

        assert_eq!(scoped.from.as_deref(), Some("2026-04-12"));
        assert_eq!(scoped.to.as_deref(), Some("2026-05-11"));
        assert!(matches!(scoped.provider, ProviderArg::Claude));
    }

    #[test]
    fn top_zero_emits_every_row() {
        // `--top 0` is the documented "no cap" sentinel — top_iter
        // must return the whole slice, not an empty one.
        let v: Vec<u32> = (0..7).collect();
        let n = top_iter(&v, 0).count();
        assert_eq!(n, 7, "top=0 should be no-cap, got {n}");

        // And `--top 3` still caps.
        assert_eq!(top_iter(&v, 3).count(), 3);
    }

    #[test]
    fn write_csv_dump_picks_folder_for_extensionless_path() {
        let tmp = tempfile::tempdir().unwrap();
        // Path doesn't exist yet, no .csv/.tsv/.txt/.tab → folder.
        let target = tmp.path().join("report");
        let data = UsageData::default();
        write_csv_dump(&target, &data).unwrap();
        assert!(target.is_dir(), "expected folder write");
        assert!(target.join(".ainb-export").exists());
        assert!(target.join("summary.csv").exists());
    }

    #[test]
    fn write_csv_dump_picks_single_file_for_csv_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("dump.csv");
        let data = UsageData::default();
        write_csv_dump(&target, &data).unwrap();
        assert!(target.is_file(), "expected single-file write");
        let contents = std::fs::read_to_string(&target).unwrap();
        // combined_csv includes the summary section header.
        assert!(contents.contains("section,metric,value"));
    }

    #[test]
    fn rewriting_export_folder_scrubs_stale_csvs() {
        // Simulate a previous export that included tools.csv (because
        // data.tools was non-empty), then re-export with empty tools.
        // tools.csv must not survive the second write.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("report");

        let mut data = UsageData::default();
        data.tools.push(crate::data::usage::NamedUsage {
            name: "Read".into(),
            calls: 3,
        });
        write_csv_dump(&target, &data).unwrap();
        assert!(target.join("tools.csv").exists());

        // Second pass: tools is now empty. tools.csv should be gone.
        let empty = UsageData::default();
        write_csv_dump(&target, &empty).unwrap();
        assert!(
            !target.join("tools.csv").exists(),
            "stale tools.csv should have been scrubbed"
        );
        // But the live core files remain rewritten.
        assert!(target.join("summary.csv").exists());
    }
}
