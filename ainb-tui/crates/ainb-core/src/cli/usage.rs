// ABOUTME: Non-interactive usage analytics commands.
// Phase 6c-cli + 6d: this file is now a thin wrapper around clap arg
// types and three host-side admin commands (Plan / Currency / Cache).
// All analytics handlers (report/status/today/month/export/optimize/
// compare/yield/model-alias) route through the burndown plugin via
// `crate::cli::registry::dispatch_usage_via_plugin`. The legacy in-tree
// implementations were deleted in Phase 6d; the surviving `report_json`
// helper is kept solely to power `test_support::cli_usage_report_json`,
// which the tripwire integration tests use as a byte-identity oracle
// for the plugin's CLI output.

use crate::cli::OutputFormat;
use crate::config::{AppConfig, CurrencyConfig, UsagePlan, UsagePlanId, UsagePlanProvider};
use crate::models::usage::{
    ActivityUsage, NamedUsage, ProjectUsage, SessionUsage, UsageData,
};
use anyhow::{Result, anyhow, bail};
use chrono::Local;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;
use std::path::PathBuf;

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
    /// Show configured plan
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

/// Phase 6c-cli + 6d: host-side `ainb usage <subcommand>` execution.
///
/// Plan / Currency / Cache stay in the host (config admin, not
/// analytics — they don't fit the plugin's data-plane model). The
/// remaining 9 subcommands route through the burndown plugin via
/// `ainb-plugin-host::dispatch_cli`; reaching them here means the
/// registry-side runner's plugin host setup failed (e.g. an
/// `AINB_DISABLE_PLUGINS=1` slipped through). The canonical entry
/// is `crate::cli::registry::UsageCommand::run`.
pub async fn execute(command: UsageCommands, format: OutputFormat) -> Result<()> {
    match command {
        // Host-side admin subcommands.
        UsageCommands::Plan { command } => plan_command(command, format),
        UsageCommands::Currency(args) => currency_command(args),
        UsageCommands::Cache { command } => cache_command(command, format),
        // Plugin-routed subcommands. The registry's `dispatch_usage_via_plugin`
        // handles these directly; reaching this branch means the host
        // shim's plugin host setup failed.
        UsageCommands::Report(_)
        | UsageCommands::Status(_)
        | UsageCommands::Today(_)
        | UsageCommands::Month(_)
        | UsageCommands::Export(_)
        | UsageCommands::Optimize(_)
        | UsageCommands::Compare(_)
        | UsageCommands::Yield(_)
        | UsageCommands::ModelAlias(_) => {
            anyhow::bail!(
                "internal: subcommand dispatched through host fallback — \
                 burndown plugin should have handled this"
            )
        }
    }
}

fn cache_command(command: UsageCacheCommands, format: OutputFormat) -> Result<()> {
    use crate::models::usage::shared_cache;
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

/// Phase 6d: the `plan show` projection (`spend / projected / status`)
/// previously called into the host's parser tree (`parse_usage_for` →
/// aggregation pipeline). That whole pipeline is the plugin's job now,
/// so `Show` just prints the configured plan; the plugin's
/// `ainb usage report` covers the projection breakdown.
fn plan_command(command: UsagePlanCommands, format: OutputFormat) -> Result<()> {
    let mut config = AppConfig::load().unwrap_or_default();
    match command {
        UsagePlanCommands::Show(_args) => {
            if matches!(format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "plan": config.usage.plan,
                    }))?
                );
            } else if let Some(plan) = &config.usage.plan {
                println!(
                    "Plan: {:?} ${:.2}/month reset day {}",
                    plan.id, plan.monthly_usd, plan.reset_day
                );
                println!(
                    "(run `ainb usage report --period month` via the burndown plugin for spend/projection)"
                );
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

/// JSON shape used by the burndown plugin's `usage report --format=json`
/// CLI handler. Re-exported through `test_support::cli_usage_report_json`
/// so tripwire / snapshot tests can assert byte-identity between the
/// in-tree golden shape and the plugin's actual stdout.
///
/// Only `report_json` survives Phase 6d: the print/CSV/export helpers
/// it used to live alongside were dead in-tree handlers — `dispatch_usage_via_plugin`
/// now owns those code paths. This stays so the golden survives.
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

// Silence "unused import" warnings on the types still re-exported for
// `report_json` — the CSV/print handlers that referenced them directly
// were deleted in Phase 6d.
#[allow(dead_code)]
type _UnusedActivity = ActivityUsage;
#[allow(dead_code)]
type _UnusedNamed = NamedUsage;
#[allow(dead_code)]
type _UnusedProject = ProjectUsage;
#[allow(dead_code)]
type _UnusedSession = SessionUsage;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

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
    fn month_and_quarter_are_mutually_exclusive_at_clap_parse_time() {
        let app = crate::cli::registry::CommandRegistry::built_ins()
            .build_clap(crate::cli::root_clap_command());
        let result = app.try_get_matches_from([
            "ainb",
            "usage",
            "report",
            "--month",
            "2026-04",
            "--quarter",
            "2026-Q2",
        ]);
        assert!(
            result.is_err(),
            "clap should reject --month and --quarter together"
        );
    }

    #[test]
    fn last_n_days_conflicts_with_ytd_at_clap_parse_time() {
        let app = crate::cli::registry::CommandRegistry::built_ins()
            .build_clap(crate::cli::root_clap_command());
        let result = app.try_get_matches_from([
            "ainb",
            "usage",
            "report",
            "--last-n-days",
            "14",
            "--ytd",
        ]);
        assert!(
            result.is_err(),
            "clap should reject --last-n-days and --ytd together"
        );
    }

    #[test]
    fn from_to_conflicts_with_month_at_clap_parse_time() {
        let app = crate::cli::registry::CommandRegistry::built_ins()
            .build_clap(crate::cli::root_clap_command());
        let result = app.try_get_matches_from([
            "ainb",
            "usage",
            "report",
            "--from",
            "2026-04-01",
            "--month",
            "2026-04",
        ]);
        assert!(result.is_err(), "clap should reject --from with --month");
    }

    // Reference NaiveDate so the chrono import stays alive for the
    // future date-flag tests this module owns.
    #[test]
    fn naive_date_construction_works() {
        let _ = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
    }
}
