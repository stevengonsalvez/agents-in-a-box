//! Standalone Fleet runtime installation.
//!
//! This is a CLI-only provisioning surface. The macOS app uses negotiated
//! daemon RPC after setup, never this command or its output.

use anyhow::{Result, bail};

use crate::cli::OutputFormat;

/// Install supported provider hooks and start both Fleet runtime daemons.
///
/// Setup reuses the database and token, then restarts the daemon so an upgrade
/// serves this binary's Fleet capabilities before the app connects.
pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("install", _)) => {
            crate::cli::hangar::run_daemon_setup().await?;
            crate::cli::hangar::run_daemon_restart()?;
            ainb_plugin_notifyd::cli::cmd_install(ainb_plugin_notifyd::install::Agent::ALL)?;
            ainb_plugin_notifyd::cli::cmd_restart(false)?;
            println!("fleet runtime: ready");
            Ok(())
        }
        Some((other, _)) => bail!("unknown `ainb fleet runtime` subcommand: {other}"),
        None => bail!("missing `ainb fleet runtime` subcommand"),
    }
}
