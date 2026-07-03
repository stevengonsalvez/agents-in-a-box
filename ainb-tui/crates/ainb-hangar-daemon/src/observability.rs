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
use tracing_subscriber::{EnvFilter, fmt};

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

/// P8.2 OTLP exporter configuration.
///
/// Carries the collector endpoint the OTLP/HTTP exporter sends spans to. The
/// daemon populates this from `OTEL_EXPORTER_OTLP_ENDPOINT` (see
/// [`OtlpOpts::from_env`]); when that var is unset the daemon leaves
/// [`ObservabilityOpts::otlp`] `None` and [`install`] attaches no OTLP layer —
/// the JSONL sink stays the sole telemetry path (silent fallback, no error).
///
/// Honoured by [`install`] **only** under the `otlp` cargo feature. On a default
/// build the OTEL crates are not linked and any [`OtlpOpts`] is ignored.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OtlpOpts {
    /// The OTLP/HTTP collector base URL spans are sent to (the exporter
    /// appends `/v1/traces`). Sourced from `OTEL_EXPORTER_OTLP_ENDPOINT`.
    pub endpoint: String,
}

impl OtlpOpts {
    /// Construct from an explicit endpoint URL.
    ///
    /// Production code prefers [`OtlpOpts::from_env`]; this is the constructor
    /// integration tests use to point the exporter at a mock receiver (the
    /// `#[non_exhaustive]` marker otherwise blocks struct-literal construction
    /// from outside this crate).
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    /// Discover the OTLP endpoint: the `OTEL_EXPORTER_OTLP_ENDPOINT` env var
    /// first, then the onboarding OTel creds file (P10 / D19).
    ///
    /// Returns `None` when neither is present — the caller then leaves
    /// [`ObservabilityOpts::otlp`] `None` and [`install`] keeps JSONL as the only
    /// sink (the silent, no-error fallback). This is the endpoint-discovery seam:
    /// OTLP is opt-in, never on by default.
    ///
    /// The env var wins (an operator or the shell rc that sourced the creds file
    /// exports it). When it is unset, this reads the onboarding
    /// `~/.agents-in-a-box/otel/grafana-cloud.env` file (the
    /// `crate::otel::write_env_file` output) and parses its exported
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` — so a daemon launched OUTSIDE a shell that
    /// sourced that file (e.g. from launchd) still wires OTLP when the onboarding
    /// creds exist.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            let endpoint = endpoint.trim().to_string();
            if !endpoint.is_empty() {
                return Some(Self { endpoint });
            }
        }
        Self::endpoint_from_creds_file(&creds_env_file_path()?)
    }

    /// Parse `OTEL_EXPORTER_OTLP_ENDPOINT` out of an onboarding creds file at
    /// `path` (the `export KEY='value'` shell-env shape `write_env_file` emits).
    ///
    /// Returns `None` when the file is absent / unreadable, has no endpoint line,
    /// or the endpoint value is empty — every one of which is the silent
    /// JSONL-only fallback. Split out (against an explicit path) so it is unit
    /// testable without touching `$HOME`.
    #[must_use]
    pub fn endpoint_from_creds_file(path: &std::path::Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        for line in contents.lines() {
            let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            let Some(rest) = line.strip_prefix("OTEL_EXPORTER_OTLP_ENDPOINT=") else {
                continue;
            };
            // Strip surrounding single/double quotes the onboarding writer adds.
            let endpoint = rest
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .trim()
                .to_string();
            if !endpoint.is_empty() {
                return Some(Self { endpoint });
            }
        }
        None
    }
}

/// The onboarding OTel creds file path: `~/.agents-in-a-box/otel/grafana-cloud.env`
/// (the `crate::otel::write_env_file` output). Mirrors that resolver — the daemon
/// cannot depend on the `ainb-core` crate (it would form a cycle), so the path is
/// replicated here. `None` when the home directory cannot be resolved.
fn creds_env_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("otel").join("grafana-cloud.env"))
}

/// Tracer name attached to OTLP-exported spans (the instrumentation scope).
#[cfg(feature = "otlp")]
const OTLP_TRACER_NAME: &str = "ainb-hangar-daemon";

/// Build the OTLP `tracing` layer + the tracer provider that backs it.
///
/// Returns `(None, None)` when no endpoint is configured — the silent
/// JSONL-only fallback. When an endpoint is present it builds an OTLP/HTTP
/// (protobuf) [`SpanExporter`](opentelemetry_otlp::SpanExporter) targeting that
/// endpoint, wraps it in an [`SdkTracerProvider`](opentelemetry_sdk::trace::SdkTracerProvider)
/// with a `BatchSpanProcessor` (its own dedicated OS thread — no tokio runtime
/// entanglement), and returns a [`tracing_opentelemetry`] layer plus the provider
/// so the caller can flush + shut it down explicitly at process exit.
///
/// The returned layer is `Option<L>`: `Option<L: Layer<S>>` itself implements
/// `Layer<S>`, so the `None` arm composes as a true no-op without changing the
/// subscriber's type.
#[cfg(feature = "otlp")]
#[allow(clippy::type_complexity)]
fn build_otlp_layer<S>(
    otlp: Option<&OtlpOpts>,
) -> anyhow::Result<(
    Option<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>>,
    Option<opentelemetry_sdk::trace::SdkTracerProvider>,
)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig as _;

    let Some(OtlpOpts { endpoint }) = otlp else {
        // Endpoint discovery came back empty: no OTLP layer, JSONL stays sole sink.
        return Ok((None, None));
    };

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("build OTLP span exporter for {endpoint}: {e}"))?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer(OTLP_TRACER_NAME);
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((Some(layer), Some(provider)))
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

/// Held-for-the-process-lifetime handle returned by [`install`].
///
/// Always owns the non-blocking appender's [`WorkerGuard`] (dropping it flushes
/// and stops the JSONL writer thread — the classic footgun). Under the `otlp`
/// feature it *also* owns the OpenTelemetry tracer provider so the daemon can
/// flush + tear it down **explicitly** at shutdown.
///
/// ## Why explicit shutdown, not Drop
///
/// The OTLP exporter runs on / blocks the tokio runtime. Letting the provider
/// drop implicitly from inside `#[tokio::main]` re-enters the runtime during
/// teardown and can hang or panic (`reference_tokio_runtime_drop_trap`). So the
/// daemon calls [`Guard::shutdown`] from its shutdown handler, which force-flushes
/// pending spans and shuts the provider down on a controlled path, *then* drops
/// the worker guard.
pub struct Guard {
    /// Keeps the non-blocking JSONL writer thread alive. Dropped last.
    _worker: WorkerGuard,
    /// The OTLP tracer provider, present only when the `otlp` feature is on AND
    /// an endpoint was configured. `Option` so the no-OTLP path carries nothing.
    #[cfg(feature = "otlp")]
    otlp_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Guard {
    /// Flush and tear down telemetry explicitly (never via Drop in the runtime).
    ///
    /// Under the `otlp` feature, force-flushes and shuts down the OTLP tracer
    /// provider so buffered spans are exported before the process exits. Without
    /// the feature (or with no endpoint configured) this only drops the JSONL
    /// worker guard. Safe to call exactly once from the daemon shutdown handler.
    pub fn shutdown(self) {
        #[cfg(feature = "otlp")]
        if let Some(provider) = self.otlp_provider {
            // Force-flush exports buffered spans; shutdown stops the exporter on a
            // controlled path rather than during a runtime-entangled Drop.
            let _ = provider.force_flush();
            let _ = provider.shutdown();
        }
        // `_worker` drops here, flushing + stopping the JSONL writer thread.
    }
}

/// Install the global `tracing` subscriber, returning the lifetime [`Guard`].
///
/// Composes, in order: the `RUST_LOG`-driven [`EnvFilter`]; the OTLP layer (only
/// under the `otlp` feature, and only when [`ObservabilityOpts::otlp`] is `Some`);
/// the JSON rolling-file layer; and (when [`ObservabilityOpts::stderr`]) a
/// human-readable stderr mirror. The JSONL sink is **always** installed — OTLP is
/// purely additive, so an unset endpoint silently falls back to JSONL-only.
///
/// Idempotency: this installs the process-global default subscriber and must be
/// called **exactly once**, before any spans are emitted. Calling it twice
/// panics (the global default is already set).
///
/// # Errors
///
/// Returns an error if the log directory cannot be created, the rolling appender
/// cannot open its file, or (under the `otlp` feature) the OTLP exporter pipeline
/// cannot be built for the configured endpoint.
///
/// # Panics
///
/// Panics if the global subscriber has already been installed (second call).
pub fn install(opts: ObservabilityOpts) -> anyhow::Result<Guard> {
    let ObservabilityOpts {
        log_dir,
        stderr,
        otlp,
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
    let (non_blocking, worker) = tracing_appender::non_blocking(file_appender);

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

    // OTLP layer is additive and lives *before* the JSONL layer in the registry
    // (composition order does not change which events each layer sees — both see
    // every event passing the shared `EnvFilter` — but mirrors the spec's
    // `.with(otlp_layer).with(jsonl_layer)` ordering). Off entirely without the
    // feature or without a configured endpoint.
    #[cfg(feature = "otlp")]
    let (otlp_layer, otlp_provider) = build_otlp_layer(otlp.as_ref())?;
    #[cfg(not(feature = "otlp"))]
    let _ = &otlp; // endpoint discovery is a no-op without the feature.

    let registry = tracing_subscriber::registry().with(env_filter);
    #[cfg(feature = "otlp")]
    let registry = registry.with(otlp_layer);
    registry.with(json_layer).with(stderr_layer).init();

    Ok(Guard {
        _worker: worker,
        #[cfg(feature = "otlp")]
        otlp_provider,
    })
}

#[cfg(test)]
mod tests {
    use super::OtlpOpts;

    /// The creds-file parser lifts the exported endpoint out of the onboarding
    /// `grafana-cloud.env` shape (`export KEY='value'`), stripping the quotes.
    #[test]
    fn endpoint_parsed_from_onboarding_creds_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grafana-cloud.env");
        std::fs::write(
            &path,
            "# comment\n\
             export OTEL_RESOURCE_ATTRIBUTES='host.name=box'\n\
             export OTEL_EXPORTER_OTLP_ENDPOINT='http://localhost:4318'\n",
        )
        .unwrap();
        let opts = OtlpOpts::endpoint_from_creds_file(&path).expect("endpoint parsed");
        assert_eq!(opts.endpoint, "http://localhost:4318");
    }

    /// A missing file, or a file with no endpoint line, is the silent
    /// JSONL-only fallback (`None`, never an error).
    #[test]
    fn missing_or_endpointless_creds_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.env");
        assert!(OtlpOpts::endpoint_from_creds_file(&missing).is_none());

        let empty = dir.path().join("empty.env");
        std::fs::write(&empty, "export GRAFANA_API_TOKEN='x'\n").unwrap();
        assert!(OtlpOpts::endpoint_from_creds_file(&empty).is_none());

        // An empty endpoint value is also None (not a wired empty endpoint).
        let blank = dir.path().join("blank.env");
        std::fs::write(&blank, "export OTEL_EXPORTER_OTLP_ENDPOINT=''\n").unwrap();
        assert!(OtlpOpts::endpoint_from_creds_file(&blank).is_none());
    }
}
