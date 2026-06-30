// ABOUTME: `ainb otel` — set up OpenTelemetry export to Grafana Cloud.
//
// Full-auto `setup`: vendors the Alloy assets into ~/.agents-in-a-box/otel/,
// prompts for the three Grafana Cloud values (with how-to-get instructions),
// writes grafana-cloud.env (0600), ensures the generic OTEL keys are in
// ~/.claude/settings.json, wires the shell rc to source the env file,
// optionally `brew install`s Alloy (with consent), and starts Alloy in tmux.
//
// Secrets never touch settings.json (that file syncs to a public repo) — they
// live only in grafana-cloud.env, read by Alloy's launcher.

use std::io::{self, Write};

use anyhow::Result;
use clap::Subcommand;

use crate::cli::OutputFormat;
use crate::otel;

#[derive(Subcommand, Debug)]
pub enum OtelCommands {
    /// Set up OpenTelemetry export to Grafana Cloud (assets, creds, Alloy).
    Setup(SetupArgs),
    /// Show local OTEL pipeline state (env file, Alloy install, tmux session).
    Status,
    /// (Re)start Grafana Alloy in its tmux session.
    Start,
}

#[derive(clap::Args, Debug)]
pub struct SetupArgs {
    /// host.name resource attribute (defaults to the short hostname).
    #[arg(long)]
    pub host_name: Option<String>,

    /// Don't start Alloy after writing config.
    #[arg(long)]
    pub no_start: bool,

    /// Don't offer to `brew install` Alloy if it's missing.
    #[arg(long)]
    pub no_install: bool,

    /// Telemetry provider (only grafana-cloud today).
    #[arg(long, default_value = "grafana-cloud")]
    pub provider: String,
}

/// Dispatch `ainb otel <sub>`.
pub async fn execute(cmd: OtelCommands, format: OutputFormat) -> Result<()> {
    match cmd {
        OtelCommands::Setup(args) => setup(args).await,
        OtelCommands::Status => status(format),
        OtelCommands::Start => start(),
    }
}

// ── setup ───────────────────────────────────────────────────────────────────

async fn setup(args: SetupArgs) -> Result<()> {
    if args.provider != "grafana-cloud" {
        anyhow::bail!(
            "unknown provider '{}' — only 'grafana-cloud' is supported today",
            args.provider
        );
    }

    println!("OpenTelemetry setup — provider: Grafana Cloud");
    println!("{}", "\u{2501}".repeat(64));
    println!("Docs (setup + example dashboards): {}\n", crate::docs::OTEL);
    println!(
        "Pipeline:  Claude Code --OTLP {ep}--> Grafana Alloy --(authenticated OTLP)--> Grafana Cloud",
        ep = otel::LOCAL_OTLP_ENDPOINT
    );
    println!(
        "           The local {} hop (Claude Code -> Alloy) is automatic. Below you supply",
        otel::LOCAL_OTLP_ENDPOINT
    );
    println!("           the REMOTE Grafana Cloud endpoint + creds that Alloy forwards to.\n");

    // 1. Vendor the generic assets (config.alloy, start-alloy.sh, dashboards).
    otel::write_assets()?;
    println!(
        "[1/6] wrote Alloy config + launcher + dashboards to {}",
        otel::otel_dir()?.display()
    );

    // 2. Collect Grafana Cloud creds — with how-to-get instructions.
    println!("\n[2/6] Grafana Cloud credentials");
    println!(
        "      These are the REMOTE Grafana Cloud values Alloy ships to — NOT the local"
    );
    println!("      {} endpoint above (that one is automatic).", otel::LOCAL_OTLP_ENDPOINT);
    println!(
        "      Where to find these: Grafana Cloud \u{2192} Connections \u{2192} \"OpenTelemetry (OTLP)\"."
    );
    println!("      That page shows the OTLP endpoint URL, an Instance ID (Basic-auth");
    println!("      username), and lets you generate an API token (needs metrics+logs+traces");
    println!("      write). Values can also be preset via the GRAFANA_OTLP_ENDPOINT,");
    println!("      GRAFANA_INSTANCE_ID and GRAFANA_API_TOKEN env vars.");
    println!();
    println!("      Example (your region/numbers differ):");
    println!("        OTLP endpoint  https://otlp-gateway-prod-us-east-0.grafana.net/otlp");
    println!("        Instance ID    1234567");
    println!("        API token      glc_eyJ...  (scope: metrics + logs + traces write)\n");

    let otlp_endpoint = resolve_value(
        "GRAFANA_OTLP_ENDPOINT",
        "      Grafana Cloud OTLP endpoint URL (https://...grafana.net/otlp)",
    )?;
    let instance_id = resolve_value("GRAFANA_INSTANCE_ID", "      Instance ID (Basic-auth user)")?;
    // Token is a secret — read it without echoing to the terminal.
    let api_token = resolve_secret("GRAFANA_API_TOKEN", "      API token (hidden)")?;

    // Light sanity check: the endpoint should be an http(s) URL ending in /otlp.
    // Warn (don't bail) — a common mistake is swapping the endpoint and instance
    // fields, which otherwise only surfaces as a confusing Alloy auth error.
    let ep = otlp_endpoint.trim();
    let looks_local = ep.contains("localhost")
        || ep.contains("127.0.0.1")
        || ep.contains(":4318")
        || ep.contains(":4317");
    if looks_local {
        eprintln!(
            "      warning: that looks like the LOCAL Alloy endpoint ({}) — this field wants the",
            otel::LOCAL_OTLP_ENDPOINT
        );
        eprintln!(
            "      REMOTE Grafana Cloud URL, e.g. https://otlp-gateway-prod-us-east-0.grafana.net/otlp"
        );
    } else if !ep.starts_with("http://") && !ep.starts_with("https://") {
        eprintln!(
            "      warning: OTLP endpoint doesn't start with http(s):// — did you paste the right field?"
        );
    } else if !ep.ends_with("/otlp") {
        eprintln!("      warning: OTLP endpoint usually ends in /otlp — double-check the value.");
    }

    let creds = otel::GrafanaCloudCreds {
        otlp_endpoint,
        instance_id,
        api_token,
    };
    if !creds.is_complete() {
        anyhow::bail!("all three Grafana Cloud values are required — aborting");
    }

    // 3. host.name resource attr.
    let host_name = match args.host_name {
        Some(h) => h,
        None => {
            let detected = otel::detect_host_name();
            let answer = prompt(&format!("      host.name [{detected}]"))?;
            if answer.trim().is_empty() {
                detected
            } else {
                answer.trim().to_string()
            }
        }
    };

    // 4. Write the secret env file (0600).
    let env_path = otel::write_env_file(&creds, &host_name)?;
    println!(
        "\n[3/6] wrote {} (0600, secrets — never committed)",
        env_path.display()
    );

    // 5. Ensure the generic, non-secret OTEL keys are in settings.json.
    let added = otel::ensure_settings_env()?;
    if added.is_empty() {
        println!("[4/6] ~/.claude/settings.json already has the generic OTEL env");
    } else {
        println!(
            "[4/6] added {} generic OTEL key(s) to ~/.claude/settings.json (no secrets)",
            added.len()
        );
    }

    // 6. Wire the shell rc to source the env file.
    match otel::ensure_shell_rc_sources_env() {
        Ok(true) => println!(
            "[5/6] wired {} to source the OTEL env (backup at <rc>.bak) \u{2014} open a new shell to load it",
            otel::shell_rc_path()?.display()
        ),
        Ok(false) => println!("[5/6] shell rc already sources the OTEL env"),
        Err(e) => eprintln!("[5/6] could not wire shell rc: {e} (add the source line manually)"),
    }

    // 7. Alloy: install (consent) + start.
    if !otel::alloy_installed() {
        if args.no_install {
            println!(
                "[6/6] alloy not installed \u{2014} install it then run `ainb otel start`:\n        brew install {}",
                otel::ALLOY_BREW_FORMULA
            );
            print_done(&host_name)?;
            return Ok(());
        }
        let yes = prompt_yes_no(&format!(
            "[6/6] alloy not installed. brew install {}?",
            otel::ALLOY_BREW_FORMULA
        ))?;
        if yes {
            otel::brew_install_alloy()?;
        } else {
            println!(
                "      skipped. install later then run `ainb otel start`:\n        brew install {}",
                otel::ALLOY_BREW_FORMULA
            );
            print_done(&host_name)?;
            return Ok(());
        }
    }

    if args.no_start {
        println!("[6/6] alloy installed \u{2014} start it with: ainb otel start");
    } else {
        otel::start_alloy()?;
    }

    print_done(&host_name)?;
    Ok(())
}

fn print_done(host_name: &str) -> Result<()> {
    let dash = otel::otel_dir()?.join("dashboards");
    println!("\n{}", "\u{2501}".repeat(64));
    println!("DONE. host.name={host_name}");
    println!("  Alloy UI/health:  {}", otel::ALLOY_HEALTH_URL);
    println!(
        "  Dashboards to import in Grafana Cloud:  {}",
        dash.display()
    );
    println!("  Verify:  ainb otel status   (or: ainb doctor)");
    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────

fn status(format: OutputFormat) -> Result<()> {
    let s = otel::status();
    if matches!(format, OutputFormat::Json) {
        let v = serde_json::json!({
            "env_file_present": s.env_file_present,
            "config_present": s.config_present,
            "alloy_installed": s.alloy_installed,
            "alloy_running": s.alloy_running,
            "settings_env_present": s.settings_env_present,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    let mark = |b: bool| if b { "\u{2713}" } else { "\u{2717}" };
    println!("OpenTelemetry / Grafana Cloud status");
    println!("{}", "\u{2501}".repeat(48));
    println!("  {} grafana-cloud.env present", mark(s.env_file_present));
    println!("  {} config.alloy present", mark(s.config_present));
    println!(
        "  {} generic OTEL env in settings.json",
        mark(s.settings_env_present)
    );
    println!("  {} alloy installed", mark(s.alloy_installed));
    println!(
        "  {} alloy running (tmux: {})",
        mark(s.alloy_running),
        otel::ALLOY_TMUX_SESSION
    );
    if !s.env_file_present || !s.config_present {
        println!("\nRun `ainb otel setup` to configure.");
    } else if !s.alloy_running {
        println!("\nRun `ainb otel start` to launch Alloy.");
    }
    Ok(())
}

// ── start ─────────────────────────────────────────────────────────────────

fn start() -> Result<()> {
    if !otel::alloy_installed() {
        anyhow::bail!(
            "alloy not installed — brew install {} (or run `ainb otel setup`)",
            otel::ALLOY_BREW_FORMULA
        );
    }
    otel::start_alloy()
}

// ── prompt helpers ──────────────────────────────────────────────────────────

/// Use the env var if set+non-empty, else prompt (echoed).
fn resolve_value(env_key: &str, label: &str) -> Result<String> {
    if let Some(v) = from_env(env_key, label) {
        return Ok(v);
    }
    let v = prompt(label)?;
    Ok(v.trim().to_string())
}

/// Like `resolve_value` but reads the prompt without echoing (secrets). Falls
/// back to an echoed read if there is no TTY (e.g. piped input in CI).
fn resolve_secret(env_key: &str, label: &str) -> Result<String> {
    if let Some(v) = from_env(env_key, label) {
        return Ok(v);
    }
    match rpassword::prompt_password(format!("{label}: ")) {
        Ok(v) => Ok(v.trim().to_string()),
        // No TTY (piped stdin) — rpassword errors; fall back to a plain read.
        Err(_) => {
            let v = prompt(label)?;
            Ok(v.trim().to_string())
        }
    }
}

/// Return the env var value if set+non-empty, announcing the source.
fn from_env(env_key: &str, label: &str) -> Option<String> {
    std::env::var(env_key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .inspect(|_| {
            println!("{label}: (from ${env_key})");
        })
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

fn prompt_yes_no(label: &str) -> Result<bool> {
    print!("{label} [Y/n] ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let a = buf.trim().to_lowercase();
    Ok(a.is_empty() || a == "y" || a == "yes")
}
