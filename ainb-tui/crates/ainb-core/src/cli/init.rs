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

use super::OutputFormat;
use crate::config::{AppConfig, OnboardingConfig};
use crate::setup::{
    catalog, detect_all, detect_dep, provision, DepState, ProvisionMode, ProvisionOutcome, RealEnv,
    SetupStatus, Tier,
};

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

// --- Catalog rendering (shared with the TUI via crate::setup) ----------------

fn state_detail(state: &DepState) -> String {
    match state {
        DepState::Ok(Some(d)) => format!(" ({d})"),
        DepState::Ok(None) => String::new(),
        DepState::Alt(d) => format!(" (via {d})"),
        DepState::TooOld(d) => format!(" — {d}"),
        DepState::Missing => String::new(),
        DepState::Unknown => " (?)".to_string(),
    }
}

/// Render the setup catalog grouped by topic — the exact same model the TUI
/// dependency step renders, so the CLI and TUI onboard identically.
fn render_setup_text(status: &SetupStatus) {
    println!("Setup check (topics × dependencies):");
    println!("{}", "\u{2501}".repeat(64));

    for topic in &status.topics {
        println!("\n  {} — {}", topic.label, topic.description);
        for d in &topic.deps {
            let marker = if d.satisfied {
                "\u{2713}" // ✓
            } else if d.tier == Tier::Required {
                "\u{2717}" // ✗
            } else {
                "\u{26A0}" // ⚠
            };
            let tag = if d.satisfied {
                String::new()
            } else {
                format!(" [{}]", d.tier.label())
            };
            println!(
                "    {marker} {:<24}{tag}{}  — {}",
                d.name,
                state_detail(&d.state),
                d.why
            );
            if !d.satisfied {
                println!("        \u{2192} {}", d.install_hint);
            }
        }
    }

    println!();
    let (sat, total) = (status.satisfied_count(), status.total_count());
    if status.required_met() {
        if status.recommended_met() {
            println!("\u{2713} All dependencies ready ({sat}/{total}).");
        } else {
            println!("\u{2713} Required dependencies satisfied ({sat}/{total}) — some recommended missing.");
        }
    } else {
        println!("\u{2717} Missing required dependencies ({sat}/{total}).");
    }
}

/// Interactively offer to install each unsatisfied dependency via the shared
/// provisioner. `yes` runs auto-installable items without prompting (still
/// prints — never auto-runs — the explicit/system ones). Skips suggested-tier
/// items that can't be detected.
fn offer_installs(status: &SetupStatus, yes: bool) {
    let missing: Vec<&str> = status
        .topics
        .iter()
        .flat_map(|t| &t.deps)
        .filter(|d| !d.satisfied && !matches!(d.state, DepState::Unknown))
        .map(|d| d.id)
        .collect();
    if missing.is_empty() {
        return;
    }

    let cat = catalog();
    let mode = if yes { ProvisionMode::Yes } else { ProvisionMode::Ask };
    println!("\nInstall missing dependencies:");
    for id in missing {
        let Some(dep) = cat.iter().flat_map(|t| &t.deps).find(|d| d.id == id) else {
            continue;
        };
        let mut confirm = |cmd: &str| -> bool {
            print!("  install {} ({cmd})? [y/N] ", dep.name);
            let _ = io::stdout().flush();
            let mut line = String::new();
            io::stdin().read_line(&mut line).ok();
            line.trim().eq_ignore_ascii_case("y")
        };
        match provision(dep, mode, &mut confirm) {
            Ok(ProvisionOutcome::Installed) => println!("  \u{2713} {} installed", dep.name),
            Ok(ProvisionOutcome::Declined) => println!("  · {} skipped", dep.name),
            Ok(ProvisionOutcome::PrintOnly { command, reason }) => {
                println!("  · {}: {command}  ({reason})", dep.name)
            }
            Err(e) => println!("  \u{2717} {} failed: {e}", dep.name),
        }
    }
}

// --- Subcommand implementations ---------------------------------------------

/// `ainb init --check`
fn cmd_check(format: OutputFormat) -> Result<()> {
    let status = detect_all(&RealEnv);

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&status)
                .context("Failed to serialize setup status")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            render_setup_text(&status)
        }
    }

    if !status.required_met() {
        return Err(anyhow!("Required prerequisites missing"));
    }

    Ok(())
}

/// `ainb init` (no flags) - first-time setup
fn cmd_setup(format: OutputFormat) -> Result<()> {
    let status = detect_all(&RealEnv);

    if matches!(format, OutputFormat::Text) {
        render_setup_text(&status);
        println!();
    }

    if !status.required_met() {
        if matches!(format, OutputFormat::Json) {
            let json = serde_json::to_string_pretty(&status)
                .context("Failed to serialize setup status")?;
            println!("{json}");
        }
        return Err(anyhow!(
            "Cannot complete setup - required prerequisites are missing. Install them and run `ainb init` again."
        ));
    }

    // Offer to install missing (recommended/optional) dependencies — same
    // provisioner the TUI uses. Interactive only (Text mode).
    if matches!(format, OutputFormat::Text) {
        offer_installs(&status, false);
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
                "setup": status,
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
