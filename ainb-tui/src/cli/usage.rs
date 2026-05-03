// ABOUTME: Non-interactive usage analytics commands.
// Provides CodeBurn-style reports, exports, plans, optimization, compare, and yield.

use crate::cli::OutputFormat;
use crate::config::{AppConfig, CurrencyConfig, UsagePlan, UsagePlanId, UsagePlanProvider};
use crate::models::usage::{
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
    /// Start date YYYY-MM-DD
    #[arg(long)]
    pub from: Option<String>,
    /// End date YYYY-MM-DD
    #[arg(long)]
    pub to: Option<String>,
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
    }
}

fn cache_command(command: UsageCacheCommands, format: OutputFormat) -> Result<()> {
    match command {
        UsageCacheCommands::Info => {
            let cache = shared_cache();
            let info = cache
                .info()
                .map_err(|e| anyhow!("usage cache info failed: {e}"))?;
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
            cache
                .clear()
                .map_err(|e| anyhow!("usage cache clear failed: {e}"))?;
            match format {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&json!({"cleared": true}))?);
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
        OutputFormat::Text => {
            print_text_report(title, &data);
        }
    }
    Ok(())
}

fn print_status(args: &UsageReportArgs, format: OutputFormat) -> Result<()> {
    let data = load_usage(args)?;
    let config = AppConfig::load().unwrap_or_default();
    let projection = config.usage.plan.as_ref().map(|plan| {
        crate::models::usage::project_plan_usage(&data, plan, Local::now().date_naive())
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
        OutputFormat::Text => {
            print_text_report("Usage Status", &data);
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
        OutputFormat::Text => {
            let text = serde_json::to_string_pretty(&report_json(&data))?;
            write_or_print(args.output.as_deref(), &text)
        }
        OutputFormat::Csv => {
            if let Some(path) = &args.output {
                if path.extension().is_some() {
                    fs::write(path, combined_csv(&data))?;
                } else {
                    fs::create_dir_all(path)?;
                    fs::write(path.join("README.md"), "AINB usage export\n")?;
                    fs::write(path.join("summary.csv"), summary_csv(&data))?;
                    fs::write(path.join("daily.csv"), daily_csv(&data))?;
                    fs::write(path.join("projects.csv"), projects_csv(&data.projects))?;
                    fs::write(path.join("sessions.csv"), sessions_csv(&data.sessions))?;
                    fs::write(
                        path.join("activities.csv"),
                        activities_csv(&data.activities),
                    )?;
                    fs::write(path.join("models.csv"), models_csv(&data))?;
                    fs::write(path.join("tools.csv"), named_csv("tool", &data.tools))?;
                    fs::write(
                        path.join("shell_commands.csv"),
                        named_csv("command", &data.shell_commands),
                    )?;
                    fs::write(
                        path.join("mcp_servers.csv"),
                        named_csv("server", &data.mcp_servers),
                    )?;
                }
                Ok(())
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
                .map(|plan| crate::models::usage::project_plan_usage(&data, plan, today));
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
    let period = if args.from.is_some() || args.to.is_some() {
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
        },
        include_projects: args.include.clone(),
        exclude_projects: args.exclude.clone(),
        filters: UsageFilters {
            project: args.project.clone(),
            model: args.model.clone(),
            activity: args.activity.clone(),
            session: args.session.clone(),
        },
    })
}

fn parse_date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid date `{value}`, expected YYYY-MM-DD"))
}

fn print_text_report(title: &str, data: &UsageData) {
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
    print_top_projects(data);
    print_top_activities(data);
    print_top_models(data);
}

fn print_top_projects(data: &UsageData) {
    println!("By Project");
    for project in data.projects.iter().take(8) {
        println!(
            "- {}: {} calls, {} tokens, {}",
            project.name,
            project.bucket.call_count,
            project.bucket.total(),
            format_cost(project.bucket.cost_usd)
        );
    }
}

fn print_top_activities(data: &UsageData) {
    println!("By Activity");
    for activity in data.activities.iter().take(8) {
        println!(
            "- {}: {} turns, {} retries, {} tokens",
            activity.category.label(),
            activity.turns,
            activity.retries,
            activity.bucket.total()
        );
    }
}

fn print_top_models(data: &UsageData) {
    println!("By Model");
    for model in data.models.iter().take(8) {
        println!(
            "- {}: {} calls, {} tokens, {}",
            model.model,
            model.bucket.call_count,
            model.bucket.total(),
            format_cost(model.bucket.cost_usd)
        );
    }
}

fn report_json(data: &UsageData) -> serde_json::Value {
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
            session.first_timestamp.to_rfc3339(),
            session.last_timestamp.to_rfc3339(),
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
}
