// ABOUTME: CLI init command for first-time setup and prerequisite checking
//
// Usage:
//   ainb init                     - Run first-time setup (create default config)
//   ainb init --check             - Check prerequisites only (non-destructive)
//   ainb init --status            - Show onboarding completion status
//   ainb init --reset [--force]   - Factory reset ~/.agents-in-a-box

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;

use super::OutputFormat;
use crate::config::{AppConfig, OnboardingConfig};

#[derive(clap::Args)]
pub struct InitArgs {
    /// Only check prerequisites, don't modify any files
    #[arg(long)]
    pub check: bool,

    /// Show current onboarding completion status
    #[arg(long)]
    pub status: bool,

    /// Factory reset: remove ~/.agents-in-a-box entirely
    #[arg(long)]
    pub reset: bool,

    /// Skip interactive confirmation (required for non-interactive --reset)
    #[arg(long, short)]
    pub force: bool,
}

/// Status for a single prerequisite check
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrereqStatus {
    /// Tool is installed and available
    Ok,
    /// Required tool is missing
    Missing,
    /// Optional tool is missing
    Warning,
}

/// Result of checking a single prerequisite
#[derive(Debug, Clone, Serialize)]
pub struct PrereqCheck {
    pub name: String,
    pub required: bool,
    pub status: PrereqStatus,
    pub path: Option<String>,
    pub message: String,
}

/// Full prerequisite report
#[derive(Debug, Clone, Serialize)]
pub struct PrereqReport {
    pub checks: Vec<PrereqCheck>,
    pub all_required_present: bool,
}

/// Output structure for `--status`
#[derive(Debug, Serialize)]
struct StatusReport {
    completed: bool,
    completed_at: Option<String>,
    version: String,
    base_dir: String,
    base_dir_exists: bool,
    config_path: String,
    config_exists: bool,
    skipped_dependencies: Vec<String>,
    /// State of `~/.tmux.conf` relative to ainb's rich bundled conf.
    /// See [`crate::cli::tmux_install::OnboardingTmuxState`].
    tmux_conf_state: String,
}

/// Validate that at most one mode flag is set
fn validate_flags(check: bool, status: bool, reset: bool) -> Result<()> {
    let count = [check, status, reset].iter().filter(|x| **x).count();
    if count > 1 {
        return Err(anyhow!(
            "--check, --status, and --reset are mutually exclusive"
        ));
    }
    Ok(())
}

/// Entry point for `ainb init`
#[allow(clippy::unused_async)]
pub async fn execute(args: InitArgs, format: OutputFormat) -> Result<()> {
    validate_flags(args.check, args.status, args.reset)?;

    if args.reset {
        cmd_reset(args.force, format)
    } else if args.status {
        cmd_status(format)
    } else if args.check {
        cmd_check(format)
    } else {
        cmd_setup(format)
    }
}

// --- Prerequisite checking ---------------------------------------------------

/// Real PATH lookup using the `which` crate
fn real_lookup(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Build a single prerequisite check entry using a caller-supplied lookup
fn check_prerequisite<F>(
    name: &str,
    required: bool,
    ok_message: &str,
    missing_message: &str,
    lookup: &F,
) -> PrereqCheck
where
    F: Fn(&str) -> Option<PathBuf>,
{
    match lookup(name) {
        Some(path) => PrereqCheck {
            name: name.to_string(),
            required,
            status: PrereqStatus::Ok,
            path: Some(path.display().to_string()),
            message: ok_message.to_string(),
        },
        None => PrereqCheck {
            name: name.to_string(),
            required,
            status: if required {
                PrereqStatus::Missing
            } else {
                PrereqStatus::Warning
            },
            path: None,
            message: missing_message.to_string(),
        },
    }
}

/// Run the full prerequisite sweep using the provided lookup function.
///
/// Exposed (pub) for testability - tests pass a mock lookup that consults a
/// canned list of "installed" tools.
pub fn run_checks<F>(lookup: F) -> PrereqReport
where
    F: Fn(&str) -> Option<PathBuf>,
{
    let checks = vec![
        check_prerequisite(
            "tmux",
            true,
            "Terminal multiplexer available",
            "Install tmux (macOS: `brew install tmux`, Debian/Ubuntu: `apt install tmux`)",
            &lookup,
        ),
        check_prerequisite(
            "git",
            true,
            "Git available",
            "Install git (macOS: `brew install git`, Debian/Ubuntu: `apt install git`)",
            &lookup,
        ),
        check_prerequisite(
            "claude",
            false,
            "Claude CLI available",
            "Claude CLI not found - install it or pass `--tool codex|gemini|copilot` to `ainb run`",
            &lookup,
        ),
        check_prerequisite(
            "docker",
            false,
            "Docker available (optional, for containerized sessions)",
            "Docker not found - optional, only needed for containerized sessions",
            &lookup,
        ),
    ];

    let all_required_present = checks.iter().all(|c| !c.required || c.status == PrereqStatus::Ok);

    PrereqReport {
        checks,
        all_required_present,
    }
}

/// Print the prerequisite report in text form
fn print_report_text(report: &PrereqReport) {
    println!("Prerequisite check:");
    println!("{}", "\u{2501}".repeat(60));

    for c in &report.checks {
        let marker = match c.status {
            PrereqStatus::Ok => "\u{2713}",      // ✓
            PrereqStatus::Missing => "\u{2717}", // ✗
            PrereqStatus::Warning => "\u{26A0}", // ⚠
        };
        let tag = if c.required { "required" } else { "optional" };
        println!("  {marker} {:<7} [{tag}] - {}", c.name, c.message);
        if let Some(path) = &c.path {
            println!("      \u{21B3} {path}");
        }
    }

    println!();
    if report.all_required_present {
        println!("All required prerequisites are available.");
    } else {
        println!("Missing required prerequisites (see above).");
    }
}

/// Surface the reflect / statusline toolchain (bash>=4, uv, reflect-kb, qmd +
/// nano-graphrag, …) during setup — classified by what needs each tool —
/// without blocking onboarding. Full breakdown: `ainb doctor`; install:
/// `ainb reflect bootstrap`. Shares the catalog in [`crate::cli::deps`].
fn print_reflect_deps_summary() {
    let reports = crate::cli::deps::detect(&crate::cli::deps::RealEnv);
    println!("\n{}", crate::cli::deps::reflect_summary_line(&reports));
    if reports.iter().any(|r| !r.satisfied) {
        println!(
            "  \u{2192} `ainb doctor` for the full classified breakdown · \
             `ainb reflect bootstrap` to install the reflect toolchain."
        );
    }
}

// --- Subcommand implementations ---------------------------------------------

/// `ainb init --check`
fn cmd_check(format: OutputFormat) -> Result<()> {
    let report = run_checks(real_lookup);

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report)
                .context("Failed to serialize prerequisite report")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            print_report_text(&report)
        }
    }

    if matches!(format, OutputFormat::Text) {
        print_reflect_deps_summary();
    }

    if !report.all_required_present {
        return Err(anyhow!("Required prerequisites missing"));
    }

    Ok(())
}

/// `ainb init` (no flags) - first-time setup
fn cmd_setup(format: OutputFormat) -> Result<()> {
    let report = run_checks(real_lookup);

    if matches!(format, OutputFormat::Text) {
        print_report_text(&report);
        print_reflect_deps_summary();
        println!();
    }

    if !report.all_required_present {
        if matches!(format, OutputFormat::Json) {
            let json = serde_json::to_string_pretty(&report)
                .context("Failed to serialize prerequisite report")?;
            println!("{json}");
        }
        return Err(anyhow!(
            "Cannot complete setup - required prerequisites are missing. Install them and run `ainb init` again."
        ));
    }

    // Ensure base directory exists
    let base_dir = OnboardingConfig::base_dir()?;
    fs::create_dir_all(&base_dir)
        .with_context(|| format!("Failed to create {}", base_dir.display()))?;

    // Create default AppConfig if the user-level config file is missing
    let user_dir = AppConfig::get_user_config_dir()?;
    fs::create_dir_all(&user_dir)
        .with_context(|| format!("Failed to create {}", user_dir.display()))?;
    let config_path = user_dir.join("config.toml");
    let created_config = if !config_path.exists() {
        let default = AppConfig::default();
        default.save().context("Failed to write default config")?;
        true
    } else {
        false
    };

    // Mark onboarding as complete and persist
    let mut onboarding = OnboardingConfig::load().context("Failed to load onboarding config")?;
    onboarding.mark_completed();
    onboarding.save().context("Failed to save onboarding config")?;

    // Statusline wiring step. Only prompts in interactive Text mode AND
    // only when the user hasn't already made a decision — `Unset` is the
    // only state that re-prompts on a follow-up `ainb init` run.
    if matches!(format, OutputFormat::Text) {
        let _ = run_statusline_step(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
    }

    // Tmux conf step — mirrors statusline shape. Only prompts on Missing or
    // a recognisable-old ainb default; never touches custom user confs.
    if matches!(format, OutputFormat::Text) {
        let _ = crate::cli::tmux_install::run_tmux_setup_step(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        );
    }

    match format {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "base_dir": base_dir.display().to_string(),
                "config_path": config_path.display().to_string(),
                "created_default_config": created_config,
                "onboarding_completed": true,
                "prerequisites": report,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output).context("Failed to serialize output")?
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            println!("Setup complete.");
            println!("  Base dir:    {}", base_dir.display());
            println!("  Config file: {}", config_path.display());
            if created_config {
                println!("  Created default config file.");
            } else {
                println!("  Existing config preserved.");
            }
            println!("  Onboarding marked as completed.");
        }
    }

    Ok(())
}

/// `ainb init --status`
fn cmd_status(format: OutputFormat) -> Result<()> {
    let onboarding = OnboardingConfig::load().context("Failed to load onboarding config")?;
    let base_dir = OnboardingConfig::base_dir()?;
    let user_dir = AppConfig::get_user_config_dir()?;
    let config_path = user_dir.join("config.toml");

    let tmux_state = crate::cli::tmux_install::detect_onboarding_state_default()
        .map(|s| s.label().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let report = StatusReport {
        completed: onboarding.completed,
        completed_at: onboarding.completed_at.clone(),
        version: onboarding.version.clone(),
        base_dir: base_dir.display().to_string(),
        base_dir_exists: base_dir.exists(),
        config_path: config_path.display().to_string(),
        config_exists: config_path.exists(),
        skipped_dependencies: onboarding.skipped_dependencies.clone(),
        tmux_conf_state: tmux_state,
    };

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report)
                .context("Failed to serialize status report")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            println!("Onboarding status:");
            println!("{}", "\u{2501}".repeat(60));
            let marker = if report.completed {
                "\u{2713}"
            } else {
                "\u{2717}"
            };
            println!("  {marker} Completed:    {}", report.completed);
            if let Some(when) = &report.completed_at {
                println!("    Completed at: {when}");
            }
            println!("    Version:      {}", report.version);
            let base_marker = if report.base_dir_exists {
                "\u{2713}"
            } else {
                "\u{2717}"
            };
            println!("  {base_marker} Base dir:     {}", report.base_dir);
            let cfg_marker = if report.config_exists {
                "\u{2713}"
            } else {
                "\u{2717}"
            };
            println!("  {cfg_marker} Config file:  {}", report.config_path);
            if !report.skipped_dependencies.is_empty() {
                println!(
                    "    Skipped deps: {}",
                    report.skipped_dependencies.join(", ")
                );
            }
            // Tmux conf state — informational, never errors out.
            let tmux_marker = match report.tmux_conf_state.as_str() {
                "up to date" => "\u{2713}",
                "custom (not managed by ainb)" => "\u{2139}", // info ⓘ
                _ => "\u{26A0}",                              // ⚠ for missing / old
            };
            println!("  {tmux_marker} Tmux conf:    {}", report.tmux_conf_state);
            if matches!(
                report.tmux_conf_state.as_str(),
                "missing" | "old ainb default (upgradable)"
            ) {
                println!("    \u{21B3} run `ainb tmux install` to install the rich conf");
            }
        }
    }

    Ok(())
}

/// `ainb init --reset`
fn cmd_reset(force: bool, format: OutputFormat) -> Result<()> {
    let base_dir = OnboardingConfig::base_dir()?;

    if !base_dir.exists() {
        match format {
            OutputFormat::Json => {
                let out = serde_json::json!({
                    "reset": false,
                    "base_dir": base_dir.display().to_string(),
                    "message": "Base directory does not exist - nothing to reset",
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
                println!("Nothing to reset: {} does not exist", base_dir.display());
            }
        }
        return Ok(());
    }

    if !force {
        print!(
            "This will permanently delete {} and all its contents. Continue? [y/N] ",
            base_dir.display()
        );
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    OnboardingConfig::factory_reset().context("Failed to perform factory reset")?;

    match format {
        OutputFormat::Json => {
            let out = serde_json::json!({
                "reset": true,
                "base_dir": base_dir.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            println!("Factory reset complete.");
            println!("  Removed: {}", base_dir.display());
        }
    }

    Ok(())
}

// --- Statusline prompt -------------------------------------------------------

/// Outcome of the statusline wizard step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatuslineStepOutcome {
    /// Step skipped entirely (already wired, or user previously decided).
    Skipped,
    /// User accepted; we ran the install.
    Installed,
    /// User declined; we record `Declined` so we don't re-prompt.
    Declined,
    /// User chose "keep" their existing different statusline.
    Kept,
}

/// Drive the statusline wizard step against a `BufRead` + `Write` pair.
/// Pure on its IO so tests can drive it with in-memory buffers.
pub fn run_statusline_step<R: std::io::BufRead, W: std::io::Write>(
    input: &mut R,
    out: &mut W,
) -> Result<StatuslineStepOutcome> {
    use crate::cli::statusline_install::{
        InstallOutcome, StatuslineStatus, detect_statusline_status, install_statusline,
        install_statusline_at, install_statusline_replace_at, settings_path,
    };
    use crate::config::StatuslineDecision;

    let mut app_config = AppConfig::load().unwrap_or_default();
    if app_config.ui_preferences.statusline_decision != StatuslineDecision::Unset {
        // User already made a choice — don't re-prompt.
        return Ok(StatuslineStepOutcome::Skipped);
    }

    let status = detect_statusline_status().unwrap_or(StatuslineStatus::NotConfigured);

    match status {
        StatuslineStatus::Configured => {
            // Already wired. If on the legacy `ainb statusline` form,
            // silently migrate to the new namespaced command — the user
            // already opted in; the rename is an internal concern. Both
            // legacy and new forms classify as Configured, so we have to
            // call into install to do the in-place rewrite and read its
            // outcome to differentiate.
            let path = settings_path()?;
            match install_statusline_at(&path)? {
                InstallOutcome::Migrated => {
                    writeln!(
                        out,
                        "  ✓ Migrated existing ainb statusline → ainb claudecode statusline."
                    )
                    .ok();
                }
                _ => {
                    writeln!(out, "  ✓ Claude Code statusline already wired.").ok();
                }
            }
            app_config.ui_preferences.statusline_decision = StatuslineDecision::Installed;
            let _ = app_config.save();
            Ok(StatuslineStepOutcome::Skipped)
        }
        StatuslineStatus::Other(cmd) => {
            writeln!(out).ok();
            writeln!(out, "Existing Claude Code statusline detected: {cmd}").ok();
            writeln!(
                out,
                "  [k]eep your current command  [r]eplace with ainb claudecode statusline  [s]kip for now"
            )
            .ok();
            write!(out, "Choice [k/r/s]: ").ok();
            out.flush().ok();
            let choice = read_choice(input)?;
            let path = settings_path()?;
            match choice.as_str() {
                "r" => {
                    install_statusline_replace_at(&path)?;
                    app_config.ui_preferences.statusline_decision = StatuslineDecision::Installed;
                    let _ = app_config.save();
                    writeln!(out, "  ✓ Replaced existing statusline.").ok();
                    Ok(StatuslineStepOutcome::Installed)
                }
                "k" => {
                    // Treat "keep" as Declined for top-bar suppression
                    // but leave settings.json untouched.
                    app_config.ui_preferences.statusline_decision = StatuslineDecision::Declined;
                    let _ = app_config.save();
                    writeln!(out, "  Keeping your existing statusline. (You can wire ainb later via the Budget panel.)").ok();
                    Ok(StatuslineStepOutcome::Kept)
                }
                _ => {
                    writeln!(out, "  Skipped — leave decision unset.").ok();
                    Ok(StatuslineStepOutcome::Skipped)
                }
            }
        }
        StatuslineStatus::NotConfigured => {
            print_install_offer(out);
            write!(out, "Install? [Y/n/s] (s = show example): ").ok();
            out.flush().ok();
            let mut choice = read_choice(input)?;
            // Loop the "show me" path a single time; defensible UX.
            if choice == "s" {
                writeln!(out).ok();
                writeln!(out, "Example powerline output:").ok();
                writeln!(
                    out,
                    "  {}",
                    crate::cli::statusline::render_powerline(&example_cache())
                )
                .ok();
                writeln!(out).ok();
                write!(out, "Install? [Y/n]: ").ok();
                out.flush().ok();
                choice = read_choice(input)?;
            }
            match choice.as_str() {
                "n" => {
                    app_config.ui_preferences.statusline_decision = StatuslineDecision::Declined;
                    let _ = app_config.save();
                    writeln!(out, "  Skipped statusline wiring.").ok();
                    Ok(StatuslineStepOutcome::Declined)
                }
                _ => {
                    // Default to install on empty input or "y".
                    match install_statusline()? {
                        InstallOutcome::Installed | InstallOutcome::AlreadyInstalled => {
                            app_config.ui_preferences.statusline_decision =
                                StatuslineDecision::Installed;
                            let _ = app_config.save();
                            writeln!(out, "  ✓ Wired Claude Code statusline.").ok();
                            Ok(StatuslineStepOutcome::Installed)
                        }
                        InstallOutcome::Migrated => {
                            // Race: between detect (which classified as
                            // NotConfigured) and install, the legacy
                            // form appeared and was migrated in place.
                            // Surface it the same as a fresh install so
                            // the user knows the statusline is wired.
                            app_config.ui_preferences.statusline_decision =
                                StatuslineDecision::Installed;
                            let _ = app_config.save();
                            writeln!(
                                out,
                                "  ✓ Migrated existing ainb statusline → ainb claudecode statusline."
                            )
                            .ok();
                            Ok(StatuslineStepOutcome::Installed)
                        }
                        InstallOutcome::ExistingDifferent { current_command } => {
                            // Race: someone wrote a different statusLine
                            // between our detect and install. Don't
                            // overwrite without consent.
                            writeln!(
                                out,
                                "  Detected a foreign statusline ({current_command}); skipping."
                            )
                            .ok();
                            Ok(StatuslineStepOutcome::Skipped)
                        }
                    }
                }
            }
        }
    }
}

fn print_install_offer<W: std::io::Write>(out: &mut W) {
    writeln!(out).ok();
    writeln!(out, "Claude Code statusline").ok();
    writeln!(out, "  ainb can install a statusline that shows live:").ok();
    writeln!(out, "    - Current model + context %").ok();
    writeln!(out, "    - 5-hour rate window % + 7-day window %").ok();
    writeln!(out, "    - Today's spend (USD)").ok();
    writeln!(out, "    - Reset countdown").ok();
    writeln!(out).ok();
    writeln!(
        out,
        "  Same data as Claude Code's /usage, rendered live in your terminal prompt."
    )
    .ok();
    writeln!(
        out,
        "  ainb-tui's Budget panel + session window read from the same cache."
    )
    .ok();
    writeln!(out).ok();
}

fn example_cache() -> crate::cli::statusline::LiveCache {
    use crate::cli::statusline::{CACHE_SCHEMA_VERSION, LiveCache, RateWindow};
    LiveCache {
        version: CACHE_SCHEMA_VERSION,
        updated_at: chrono::Utc::now().to_rfc3339(),
        five_hour: Some(RateWindow {
            pct: 12,
            resets_at: None,
        }),
        seven_day: Some(RateWindow {
            pct: 3,
            resets_at: None,
        }),
        today_cost_usd: Some(4.21),
        context_pct: Some(32),
        model: Some("Opus 4.7".to_string()),
    }
}

fn read_choice<R: std::io::BufRead>(input: &mut R) -> Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_lowercase())
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a mock PATH-lookup closure that treats the provided list as
    /// "installed" and everything else as missing.
    fn make_lookup(installed: Vec<&'static str>) -> impl Fn(&str) -> Option<PathBuf> {
        move |name: &str| {
            if installed.contains(&name) {
                Some(PathBuf::from(format!("/fake/bin/{name}")))
            } else {
                None
            }
        }
    }

    // --- validate_flags ---

    #[test]
    fn test_validate_flags_none_set() {
        assert!(validate_flags(false, false, false).is_ok());
    }

    #[test]
    fn test_validate_flags_single_set() {
        assert!(validate_flags(true, false, false).is_ok());
        assert!(validate_flags(false, true, false).is_ok());
        assert!(validate_flags(false, false, true).is_ok());
    }

    #[test]
    fn test_validate_flags_two_set_rejected() {
        assert!(validate_flags(true, true, false).is_err());
        assert!(validate_flags(true, false, true).is_err());
        assert!(validate_flags(false, true, true).is_err());
    }

    #[test]
    fn test_validate_flags_all_three_rejected() {
        let err = validate_flags(true, true, true).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    // --- check_prerequisite ---

    #[test]
    fn test_check_prerequisite_found_sets_ok_and_path() {
        let lookup = make_lookup(vec!["tmux"]);
        let c = check_prerequisite("tmux", true, "ok-msg", "missing-msg", &lookup);
        assert_eq!(c.status, PrereqStatus::Ok);
        assert_eq!(c.path.as_deref(), Some("/fake/bin/tmux"));
        assert_eq!(c.message, "ok-msg");
        assert!(c.required);
    }

    #[test]
    fn test_check_prerequisite_missing_required_is_missing() {
        let lookup = make_lookup(vec![]);
        let c = check_prerequisite("tmux", true, "ok-msg", "missing-msg", &lookup);
        assert_eq!(c.status, PrereqStatus::Missing);
        assert!(c.path.is_none());
        assert_eq!(c.message, "missing-msg");
    }

    #[test]
    fn test_check_prerequisite_missing_optional_is_warning() {
        let lookup = make_lookup(vec![]);
        let c = check_prerequisite("docker", false, "ok-msg", "missing-msg", &lookup);
        assert_eq!(c.status, PrereqStatus::Warning);
        assert!(c.path.is_none());
        assert!(!c.required);
    }

    // --- run_checks ---

    #[test]
    fn test_run_checks_all_four_tools_present() {
        let report = run_checks(make_lookup(vec!["tmux", "git", "claude", "docker"]));
        assert!(report.all_required_present);
        assert_eq!(report.checks.len(), 4);
        assert!(report.checks.iter().all(|c| c.status == PrereqStatus::Ok));
    }

    #[test]
    fn test_run_checks_missing_tmux_fails_required() {
        let report = run_checks(make_lookup(vec!["git", "claude", "docker"]));
        assert!(!report.all_required_present);
        let tmux = report.checks.iter().find(|c| c.name == "tmux").unwrap();
        assert_eq!(tmux.status, PrereqStatus::Missing);
    }

    #[test]
    fn test_run_checks_missing_git_fails_required() {
        let report = run_checks(make_lookup(vec!["tmux", "claude"]));
        assert!(!report.all_required_present);
        let git = report.checks.iter().find(|c| c.name == "git").unwrap();
        assert_eq!(git.status, PrereqStatus::Missing);
    }

    #[test]
    fn test_run_checks_missing_claude_does_not_fail_required() {
        let report = run_checks(make_lookup(vec!["tmux", "git"]));
        assert!(report.all_required_present);
        let claude = report.checks.iter().find(|c| c.name == "claude").unwrap();
        assert_eq!(claude.status, PrereqStatus::Warning);
        assert!(!claude.required);
    }

    #[test]
    fn test_run_checks_missing_docker_does_not_fail_required() {
        let report = run_checks(make_lookup(vec!["tmux", "git", "claude"]));
        assert!(report.all_required_present);
        let docker = report.checks.iter().find(|c| c.name == "docker").unwrap();
        assert_eq!(docker.status, PrereqStatus::Warning);
        assert!(!docker.required);
    }

    #[test]
    fn test_run_checks_nothing_installed() {
        let report = run_checks(make_lookup(vec![]));
        assert!(!report.all_required_present);
        let by_name: std::collections::HashMap<&str, &PrereqCheck> =
            report.checks.iter().map(|c| (c.name.as_str(), c)).collect();
        assert_eq!(by_name["tmux"].status, PrereqStatus::Missing);
        assert_eq!(by_name["git"].status, PrereqStatus::Missing);
        assert_eq!(by_name["claude"].status, PrereqStatus::Warning);
        assert_eq!(by_name["docker"].status, PrereqStatus::Warning);
    }

    #[test]
    fn test_run_checks_required_vs_optional_classification() {
        let report = run_checks(make_lookup(vec![]));
        for c in &report.checks {
            match c.name.as_str() {
                "tmux" | "git" => assert!(c.required, "{} should be required", c.name),
                "claude" | "docker" => assert!(!c.required, "{} should be optional", c.name),
                other => panic!("unexpected check: {other}"),
            }
        }
    }

    #[test]
    fn test_run_checks_names_contain_all_four() {
        let report = run_checks(make_lookup(vec![]));
        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"tmux"));
        assert!(names.contains(&"git"));
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"docker"));
    }

    // --- serialization ---

    #[test]
    fn test_prereq_status_json_rename_all_snake_case() {
        assert_eq!(serde_json::to_string(&PrereqStatus::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&PrereqStatus::Missing).unwrap(),
            "\"missing\""
        );
        assert_eq!(
            serde_json::to_string(&PrereqStatus::Warning).unwrap(),
            "\"warning\""
        );
    }

    #[test]
    fn test_prereq_report_serializes_with_all_required_present_field() {
        let report = run_checks(make_lookup(vec!["tmux", "git"]));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["all_required_present"], true);
        assert_eq!(json["checks"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn test_prereq_check_serializes_path_when_ok() {
        let lookup = make_lookup(vec!["tmux"]);
        let c = check_prerequisite("tmux", true, "ok", "missing", &lookup);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["path"], "/fake/bin/tmux");
        assert_eq!(json["required"], true);
    }

    // --- statusline wizard step ---

    /// Helper: run `run_statusline_step` with a HOME pointing at a tmpdir
    /// so settings.json mutations don't touch the real ~/.claude.
    fn run_step_with_home(input: &str) -> (StatuslineStepOutcome, String, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let prev_home = std::env::var("HOME").ok();
        // Single-process tests: HOME is process-global so this races
        // with parallel tests. Tests in this module that mutate HOME
        // are gated behind a shared mutex below.
        let _guard = HOME_MUTEX.lock().unwrap();
        std::env::set_var("HOME", dir.path());
        // Also redirect XDG_CACHE_HOME if it would otherwise leak.
        let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
        std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache"));
        // Keep tmpdir alive for duration of test
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        let mut input_buf = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut out = Vec::new();
        let outcome = run_statusline_step(&mut input_buf, &mut out).unwrap();
        // Restore env to avoid leaking into other tests.
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        (outcome, String::from_utf8(out).unwrap(), path)
    }

    static HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn print_install_offer_mentions_all_promised_fields() {
        let mut buf = Vec::new();
        print_install_offer(&mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("model"));
        assert!(s.contains("context"));
        assert!(s.contains("5-hour"));
        assert!(s.contains("7-day"));
        assert!(s.contains("spend"));
        assert!(s.contains("Reset"));
    }

    #[test]
    fn read_choice_lowercases_and_trims() {
        let mut input = std::io::Cursor::new(b"  Y \n".to_vec());
        let v = read_choice(&mut input).unwrap();
        assert_eq!(v, "y");
    }

    #[test]
    fn statusline_step_decline_path_persists_decision() {
        let (outcome, output, home) = run_step_with_home("n\n");
        assert_eq!(outcome, StatuslineStepOutcome::Declined);
        assert!(output.contains("Skipped"));

        // settings.json should NOT have been written
        let settings = home.join(".claude").join("settings.json");
        assert!(
            !settings.exists(),
            "decline path must not write settings.json"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn statusline_step_install_path_writes_settings() {
        let (outcome, output, home) = run_step_with_home("y\n");
        assert_eq!(outcome, StatuslineStepOutcome::Installed);
        assert!(output.contains("Wired"));
        let settings = home.join(".claude").join("settings.json");
        assert!(settings.exists());
        let contents = std::fs::read_to_string(&settings).unwrap();
        assert!(
            contents.contains("ainb claudecode statusline"),
            "expected new namespaced command, got: {contents}"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn statusline_step_show_example_then_install() {
        // First "s" shows example, second "y" installs.
        let (outcome, output, home) = run_step_with_home("s\ny\n");
        assert_eq!(outcome, StatuslineStepOutcome::Installed);
        assert!(output.contains("Example powerline"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
