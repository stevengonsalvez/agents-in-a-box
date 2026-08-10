//! `ainb-hangar-daemon` entry point.
//!
//! Parses CLI args, installs the observability subscriber (P8.1), and hands off
//! to [`ainb_hangar_daemon::boot`]. P0 supports two behaviours: `--version`
//! (handled by `clap`) and `--once` (boot, log ready, exit 0). Without `--once`
//! the daemon idles until interrupted.

use ainb_hangar_daemon::beads_sync::reconcile;
use ainb_hangar_daemon::beads_sync::reconcile::cli::{BeadsCli, BeadsCommand};
use ainb_hangar_daemon::observability::{self, ObservabilityOpts};
use ainb_hangar_daemon::{boot, log_dir};
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
    // P8.1/P8.2: install the observability subscriber BEFORE any service
    // constructs so every span/event from boot onwards is captured. `install`
    // returns a `Guard` owning the non-blocking appender's worker (held for the
    // whole `main` body — dropping it early flushes and stops the JSONL writer,
    // silently losing buffered lines) and, under the `otlp` feature with
    // `OTEL_EXPORTER_OTLP_ENDPOINT` set, the OTLP tracer provider. The JSON sink
    // writes `daemon.<date>` under `<hangar_home>/hangar/logs`.
    //
    // P8.2 endpoint discovery: `OtlpOpts::from_env` reads the standard
    // `OTEL_EXPORTER_OTLP_ENDPOINT`; unset ⇒ `None` ⇒ no OTLP layer (silent JSONL
    // fallback). Without the `otlp` cargo feature this is a no-op and no OTEL
    // crates are linked.
    let mut opts = ObservabilityOpts::new(log_dir()?);
    opts.otlp = observability::OtlpOpts::from_env();
    let guard = observability::install(opts)?;

    // This binary had NO panic hook, while the `ainb` binary that embeds the
    // same daemon library has had one all along, and the launcher prefers this
    // binary whenever it is fresh. So the daemon most likely to be running was
    // the one whose panics reached nothing but a discarded stderr.
    observability::install_panic_hook();

    let args = Args::parse();
    let result = match args.command {
        Some(Command::Beads(cli)) => {
            let BeadsCommand::Reconcile(reconcile_args) = cli.command;
            reconcile::dispatch(&reconcile_args).await
        }
        // Only the long-lived daemon leaves crash breadcrumbs; `beads reconcile`
        // is a short-lived subcommand whose exit is the caller's exit code.
        None => run_daemon(args.once).await,
    };

    // P8.2 graceful shutdown (tokio-runtime-drop trap): the daemon's run loop
    // returns here on Ctrl-C / one-shot completion. Flush + tear the OTLP tracer
    // provider down EXPLICITLY rather than via Drop inside `#[tokio::main]` —
    // dropping the provider while the runtime is live can hang/panic. This also
    // drops the JSONL worker guard, flushing the file sink. Done on both the Ok
    // and Err paths so a daemon error still exports its final spans.
    guard.shutdown();

    result
}

/// Boot the daemon under crash breadcrumbs.
///
/// The breadcrumbs are the half of the diagnosis a panic hook cannot provide:
/// SIGKILL, `abort`, and an OOM kill run no user code at all, and two of the
/// four observed daemon deaths left no ERROR line and no panic, just a JSON log
/// that stops mid-stream. A heartbeat file that is only ever removed on an
/// exit we observed turns that silence into evidence.
///
/// `record_exit` runs on both the `Ok` and `Err` arms so a boot failure (an
/// unmigratable database, a bind error) is recorded as an explained exit rather
/// than reading as a hard kill.
async fn run_daemon(once: bool) -> anyhow::Result<()> {
    // Breadcrumbs are installed inside `boot`, after it has taken the home's
    // ownership lock. Installing them here would let a duplicate that declines
    // erase the incumbent's exit record and overwrite its heartbeat first.
    let result = boot(once).await;

    observability::note_phase("shutdown");
    match &result {
        Ok(()) => observability::record_exit("clean exit: daemon run loop returned"),
        Err(e) => observability::record_exit(&format!("error: {e:#}")),
    }
    result
}
