//! Daemon observability bootstrap (P8.1).
//!
//! [`install`] is the single place the daemon wires up `tracing`.
//!
//! It composes a [`tracing_subscriber::Registry`] with three layers:
//!
//! - an [`EnvFilter`] driven by `RUST_LOG` (falling back to `info`), so an
//!   operator can flip to `RUST_LOG=debug` live without a recompile;
//! - a JSON [`fmt`](tracing_subscriber::fmt) layer writing to a **non-blocking,
//!   daily-rotated** appender at `<log_dir>/daemon.<date>` — this is the
//!   `daemon.jsonl`-shaped sink the P8.6 CLI/TUI logs-tail surfaces read back;
//! - a human-readable stderr `fmt` layer for the dev TTY.
//!
//! It returns the [`WorkerGuard`] from `tracing_appender::non_blocking`. **The
//! caller MUST keep this guard alive for the whole process lifetime** — dropping
//! it flushes and shuts down the background writer thread, silently losing any
//! buffered log lines (the classic non-blocking-appender footgun). `main` binds
//! it to a `_guard` local that lives until the daemon exits.
//!
//! The subscriber is installed exactly once via [`tracing_subscriber::registry`]
//! + `.init()`. A second call would panic on the global-default already being
//! set, so `install` must be called once from `main` before any service spans
//! are emitted.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

/// The rolling appender's filename prefix. The daily rotation appends a
/// `.<YYYY-MM-DD>` suffix, yielding `daemon.<date>` files under the log dir.
const LOG_FILE_PREFIX: &str = "daemon";

/// Configuration for [`install`].
///
/// Construct via [`ObservabilityOpts::new`] for the production default (JSON
/// sink under the resolved Hangar home, no OTLP) or build it directly in tests
/// to point the sink at an isolated tempdir.
#[derive(Debug, Clone)]
pub struct ObservabilityOpts {
    /// Directory that holds the rolling `daemon.<date>` JSONL files. The daemon
    /// resolves this to `<hangar_home>/hangar/logs`; tests point it at a tempdir.
    pub log_dir: PathBuf,
    /// Also mirror events to stderr with the human-readable `fmt` formatter for
    /// the dev TTY. On by default; the JSON file sink is always installed.
    pub stderr: bool,
    /// P8.2 seam: when an OTLP exporter endpoint is configured, P8.2 attaches an
    /// `opentelemetry` layer here. P8.1 leaves it `None` and installs no OTLP
    /// layer — keeping the default `cargo build` free of the OTEL crates. The
    /// concrete config type lands with the `otlp` cargo feature in P8.2.
    pub otlp: Option<OtlpOpts>,
}

/// Placeholder for the P8.2 OTLP exporter configuration.
///
/// P8.1 only defines the seam so [`ObservabilityOpts`] is forward-compatible;
/// the endpoint/protocol fields and the `#[cfg(feature = "otlp")]` wiring land
/// in P8.2 (`hangar:P8.2`). Constructing one today has no effect on [`install`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OtlpOpts {
    /// The `OTEL_EXPORTER_OTLP_ENDPOINT` the P8.2 exporter will POST spans to.
    pub endpoint: String,
}

impl ObservabilityOpts {
    /// Production default: JSON file sink under `log_dir`, stderr mirror on, no
    /// OTLP.
    #[must_use]
    pub const fn new(log_dir: PathBuf) -> Self {
        Self {
            log_dir,
            stderr: true,
            otlp: None,
        }
    }
}

/// Install the global `tracing` subscriber, returning the appender's guard.
///
/// Composes the `RUST_LOG`-driven [`EnvFilter`], the JSON rolling-file layer,
/// and (when [`ObservabilityOpts::stderr`]) a human-readable stderr mirror.
///
/// Idempotency: this installs the process-global default subscriber and must be
/// called **exactly once**, before any spans are emitted. Calling it twice
/// panics (the global default is already set).
///
/// # Errors
///
/// Returns an error if the log directory cannot be created or the rolling
/// appender cannot open its file (e.g. the directory is not writable).
///
/// # Panics
///
/// Panics if the global subscriber has already been installed (second call).
pub fn install(opts: ObservabilityOpts) -> anyhow::Result<WorkerGuard> {
    let ObservabilityOpts {
        log_dir,
        stderr,
        // P8.2 seam: the OTLP config is intentionally unused in P8.1. When the
        // `otlp` cargo feature lands, P8.2 reads it here and composes a
        // `tracing_opentelemetry` layer before `.init()`. Binding it to `_` keeps
        // the default build free of the OTEL crates.
        otlp: _otlp,
    } = opts;

    std::fs::create_dir_all(&log_dir)
        .map_err(|e| anyhow::anyhow!("create log dir {}: {e}", log_dir.display()))?;

    // Daily-rotated file appender. `Builder::build` validates the directory and
    // returns the appender that the non-blocking writer wraps.
    let file_appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .build(&log_dir)
        .map_err(|e| anyhow::anyhow!("build rolling appender in {}: {e}", log_dir.display()))?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // `RUST_LOG` drives the level; default to `info`. Honoured per invocation
    // since the daemon reads the env at startup.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // JSON sink: one self-describing JSON object per event (the `daemon.jsonl`
    // shape P8.6 tails). Event fields stay nested under `fields`, so `task_id`
    // lives at `/fields/task_id`.
    let json_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(non_blocking);

    // Optional human-readable stderr mirror for the dev TTY. The JSON file sink
    // is always installed regardless.
    let stderr_layer = stderr.then(|| fmt::layer().with_writer(std::io::stderr));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(stderr_layer)
        .init();

    Ok(guard)
}
