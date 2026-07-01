// ABOUTME: `ainb doctor` — classified dependency health check for the reflect /
// statusline toolchain. Thin wrapper over `crate::cli::deps`; prints what's
// installed, what needs it, and the exact commands to fix anything missing.

use anyhow::Result;

use super::OutputFormat;
use crate::cli::deps::{self, RealEnv};

/// Classified dependency check for the reflect / statusline toolchain.
///
/// Takes no positional/flag args of its own; the global `--format` flag
/// selects text vs json output.
#[derive(clap::Args)]
pub struct DoctorArgs {}

/// Entry point for `ainb doctor`.
#[allow(clippy::unused_async)]
pub async fn execute(_args: DoctorArgs, format: OutputFormat) -> Result<()> {
    let reports = deps::detect(&RealEnv);
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&reports)?);
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            deps::print_text(&reports);
        }
    }
    Ok(())
}
