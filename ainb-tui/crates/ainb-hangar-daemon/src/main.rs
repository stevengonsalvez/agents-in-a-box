//! `ainb-hangar-daemon` entry point.
//!
//! Parses CLI args, initialises tracing, and hands off to
//! [`ainb_hangar_daemon::boot`]. P0 supports two behaviours: `--version`
//! (handled by `clap`) and `--once` (boot, log ready, exit 0). Without `--once`
//! the daemon idles until interrupted.

use ainb_hangar_daemon::beads_sync::reconcile;
use ainb_hangar_daemon::beads_sync::reconcile::cli::{BeadsCli, BeadsCommand};
use ainb_hangar_daemon::boot;
use clap::{Parser, Subcommand};

/// Hangar control-plane daemon.
#[derive(Parser, Debug)]
#[command(name = "ainb-hangar-daemon", version)]
struct Args {
    /// One-shot mode: boot, apply migrations, log `ready`, then exit 0.
    ///
    /// Test-only — used by the boot tripwire
    /// (`tests/tripwire_daemon_boots.rs`) to prove the daemon reaches its idle
    /// state and lays down a fully migrated `hangar.db` without then blocking on
    /// a shutdown signal.
    #[arg(long)]
    once: bool,

    /// Optional subcommand. When omitted the daemon boots and idles (P0/P1);
    /// `beads reconcile` runs the on-demand Hangar ↔ bd drift repair (P2.5).
    #[command(subcommand)]
    command: Option<Command>,
}

/// Top-level daemon subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Sync Hangar issues with the beads (`bd`) tracker.
    Beads(BeadsCli),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    match args.command {
        Some(Command::Beads(cli)) => {
            let BeadsCommand::Reconcile(reconcile_args) = cli.command;
            reconcile::dispatch(&reconcile_args).await
        }
        None => boot(args.once).await,
    }
}
