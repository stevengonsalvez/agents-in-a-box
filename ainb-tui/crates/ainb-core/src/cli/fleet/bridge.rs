// ABOUTME: `ainb fleet bridge {run,install,uninstall,status}` dispatcher.
//
// Thin CLI shim over `crate::fleet::bridge`. `run` starts the native daemon
// (Telegram + Slack channels sharing one relay core); install/uninstall manage
// the launchd/systemd service; status reports install state. Tokens are read
// from config.toml via the secret resolver — never passed on argv here.

use anyhow::Result;

use crate::cli::OutputFormat;

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("run", _)) | None => crate::fleet::bridge::run().await,
        Some(("install", _)) => {
            let path = crate::fleet::bridge::install()?;
            emit(
                format,
                &format!("phone bridge installed: {}", path.display()),
            );
            // Writing the unit is not the same as being able to start it. Say
            // so at install time rather than leaving the operator to discover
            // a dead bridge from `status` they had no reason to run.
            if let Some(warning) = crate::fleet::bridge::install_would_be_unrunnable() {
                emit(format, &format!("warning: {warning}"));
            }
            Ok(())
        }
        Some(("uninstall", _)) => {
            match crate::fleet::bridge::uninstall()? {
                Some(path) => emit(format, &format!("phone bridge removed: {}", path.display())),
                None => emit(format, "phone bridge was not installed"),
            }
            Ok(())
        }
        Some(("status", _)) => {
            let status = crate::fleet::bridge::status()?;
            emit(format, &status);
            Ok(())
        }
        Some((other, _)) => {
            anyhow::bail!("unknown `ainb fleet bridge` subcommand: {other}")
        }
    }
}

fn emit(format: OutputFormat, message: &str) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "message": message }));
        }
        _ => println!("{message}"),
    }
}
