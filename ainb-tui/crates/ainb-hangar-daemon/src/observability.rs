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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
            let endpoint = rest.trim().trim_matches(|c| c == '\'' || c == '"').trim().to_string();
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

// ---------------------------------------------------------------------------
// Crash breadcrumbs that survive SIGKILL
// ---------------------------------------------------------------------------
//
// `tracing` only records what user code gets to run. Four daemon deaths in one
// day left two with no ERROR line and no panic — the JSON log simply stops
// mid-stream, which is the signature of SIGKILL / abort / an OOM kill, none of
// which run a panic hook or any other user code.
//
// So the breadcrumb has to be inverted: instead of writing something *when* the
// process dies, keep a file that says "still alive" and delete it only on a
// shutdown we actually observed. Two files under `<hangar_home>/hangar/`:
//
//   daemon.heartbeat    rewritten every 10s while the process lives
//   daemon.exit-reason  written once, on an observed exit; heartbeat removed
//
// The diagnosis is then a single `ls`:
//
//   heartbeat present, exit-reason absent   -> running, or killed uncatchably
//                                              (its `updated_at_ms` says which)
//   heartbeat absent, exit-reason present   -> observed, explained shutdown
//
// The ticker runs on a dedicated OS thread, NOT a tokio task, deliberately: a
// wedged runtime (every worker blocked in sync code) would freeze a tokio-based
// heartbeat and be indistinguishable from a kill. A plain thread keeps ticking
// and tells us "process alive, runtime stuck".

/// File name of the liveness breadcrumb under `<hangar_home>/hangar/`.
const HEARTBEAT_FILE: &str = "daemon.heartbeat";

/// File name of the observed-exit breadcrumb under `<hangar_home>/hangar/`.
const EXIT_REASON_FILE: &str = "daemon.exit-reason";

/// How often the heartbeat file is rewritten.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Path of the liveness breadcrumb: `<hangar_home>/hangar/daemon.heartbeat`.
///
/// Sibling of `daemon.pid` (see [`crate::pid_path_in`]), so a reader that
/// resolved the pid file has already resolved this one's directory.
#[must_use]
pub fn heartbeat_path_in(hangar_home: &Path) -> PathBuf {
    hangar_home.join("hangar").join(HEARTBEAT_FILE)
}

/// Path of the observed-exit breadcrumb: `<hangar_home>/hangar/daemon.exit-reason`.
#[must_use]
pub fn exit_reason_path_in(hangar_home: &Path) -> PathBuf {
    hangar_home.join("hangar").join(EXIT_REASON_FILE)
}

/// The coarse phase the process believes it is in, reported in every heartbeat.
///
/// Process-global so the panic hook and the ticker thread can both read it
/// without threading a handle through every call site.
static PHASE: Mutex<&'static str> = Mutex::new("starting");

/// The live breadcrumb writer, installed once by [`start_breadcrumbs`].
static BREADCRUMBS: OnceLock<Arc<Breadcrumbs>> = OnceLock::new();

/// Mutable ticker state, guarded by one mutex so a tick can never resurrect a
/// heartbeat that [`record_exit`] has already removed.
#[derive(Debug, Default)]
struct TickState {
    /// Monotonic count of heartbeats written since process start.
    ticks: u64,
    /// Set by [`record_exit`]; stops the ticker and makes further exits no-ops.
    stopped: bool,
}

/// Owns the two breadcrumb paths and the ticker's shared state.
#[derive(Debug)]
struct Breadcrumbs {
    heartbeat: PathBuf,
    exit_reason: PathBuf,
    pid: u32,
    started_wall: SystemTime,
    started_mono: Instant,
    state: Mutex<TickState>,
    wake: Condvar,
}

impl Breadcrumbs {
    /// Seconds since [`start_breadcrumbs`] ran, on the monotonic clock (immune
    /// to a wall-clock step, which a long-lived daemon will see).
    fn uptime_secs(&self) -> u64 {
        self.started_mono.elapsed().as_secs()
    }

    /// Rewrite the heartbeat file. Best-effort: an IO failure is logged once per
    /// tick and never propagates — breadcrumb bookkeeping must not be able to
    /// stop the daemon it is watching.
    fn write_heartbeat(&self, ticks: u64) {
        let body = serde_json::json!({
            "pid": self.pid,
            "phase": phase(),
            "ticks": ticks,
            "started_at_ms": epoch_ms(self.started_wall),
            "updated_at_ms": epoch_ms(SystemTime::now()),
            "uptime_secs": self.uptime_secs(),
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Err(e) = write_atomic(&self.heartbeat, &format!("{body}\n")) {
            tracing::warn!(error = %e, path = %self.heartbeat.display(), "heartbeat write failed");
        }
    }
}

/// Convert a wall-clock instant to epoch milliseconds (0 if it predates the
/// epoch, which only a badly wrong clock produces).
fn epoch_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or_default()
}

/// Write `body` to `path` via a sibling `.tmp` + rename, so a kill mid-write
/// leaves the previous complete breadcrumb rather than a truncated one.
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// The current coarse phase, or `"unknown"` if the lock was poisoned.
fn phase() -> &'static str {
    PHASE.lock().map_or("unknown", |p| *p)
}

/// Record the coarse phase the process is now in; it rides along in every
/// subsequent heartbeat and in the exit reason.
///
/// Today the daemon entry points report only `boot` and `shutdown` — enough to
/// tell "died during migrations" from "died in the run loop". Finer, loop-level
/// phases (`claim`, `sweep`, `reconcile`) are a matter of calling this from the
/// loops themselves; this is the seam for that.
pub fn note_phase(phase: &'static str) {
    if let Ok(mut slot) = PHASE.lock() {
        *slot = phase;
    }
}

/// Start writing crash breadcrumbs for this process under `hangar_home`.
///
/// Clears any `daemon.exit-reason` left by a previous run (so a stale one can
/// never be mistaken for this run's), writes the first `daemon.heartbeat`
/// synchronously (so the file exists the moment this returns), then spawns the
/// ticker thread.
///
/// Best-effort and idempotent: a second call is a no-op, and every IO failure is
/// logged and swallowed — mirroring [`crate::PidFile::register`], because pid
/// and breadcrumb bookkeeping must never stop a daemon from booting.
pub fn start_breadcrumbs(hangar_home: &Path) {
    let heartbeat = heartbeat_path_in(hangar_home);
    let exit_reason = exit_reason_path_in(hangar_home);

    if let Some(parent) = heartbeat.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, path = %parent.display(), "breadcrumb dir create failed");
            return;
        }
    }
    // A previous run's exit reason must not be read as this run's.
    if let Err(e) = std::fs::remove_file(&exit_reason) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %exit_reason.display(), "stale exit-reason removal failed");
        }
    }

    let crumbs = Arc::new(Breadcrumbs {
        heartbeat,
        exit_reason,
        pid: std::process::id(),
        started_wall: SystemTime::now(),
        started_mono: Instant::now(),
        state: Mutex::new(TickState::default()),
        wake: Condvar::new(),
    });
    if BREADCRUMBS.set(Arc::clone(&crumbs)).is_err() {
        tracing::warn!("crash breadcrumbs already started; ignoring second call");
        return;
    }
    crumbs.write_heartbeat(0);

    let ticker = Arc::clone(&crumbs);
    let spawned = std::thread::Builder::new()
        .name("ainb-breadcrumb".to_string())
        .spawn(move || run_ticker(&ticker));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "breadcrumb ticker thread spawn failed (heartbeat will not refresh)");
    }
}

/// The ticker body: wait out the interval, then rewrite the heartbeat, until
/// [`record_exit`] stops it. Exits promptly on the condvar notify rather than
/// sleeping out the remaining interval.
fn run_ticker(crumbs: &Breadcrumbs) {
    let mut state = match crumbs.state.lock() {
        Ok(s) => s,
        Err(poisoned) => poisoned.into_inner(),
    };
    loop {
        let (next, _) = match crumbs.wake.wait_timeout(state, HEARTBEAT_INTERVAL) {
            Ok(pair) => pair,
            Err(poisoned) => poisoned.into_inner(),
        };
        state = next;
        if state.stopped {
            return;
        }
        state.ticks += 1;
        crumbs.write_heartbeat(state.ticks);
    }
}

/// Record an observed exit: write `daemon.exit-reason` naming `reason`, then
/// remove `daemon.heartbeat` and stop the ticker.
///
/// Order matters — the exit reason lands first, so there is never a window in
/// which neither file exists (which would read as "no daemon ever ran"). The
/// first call wins; later ones are no-ops, and a call before
/// [`start_breadcrumbs`] does nothing.
pub fn record_exit(reason: &str) {
    let Some(crumbs) = BREADCRUMBS.get() else {
        return;
    };
    let mut state = match crumbs.state.lock() {
        Ok(s) => s,
        Err(poisoned) => poisoned.into_inner(),
    };
    if state.stopped {
        return;
    }
    state.stopped = true;

    let body = serde_json::json!({
        "pid": crumbs.pid,
        "reason": reason,
        "phase": phase(),
        "ticks": state.ticks,
        "started_at_ms": epoch_ms(crumbs.started_wall),
        "exited_at_ms": epoch_ms(SystemTime::now()),
        "uptime_secs": crumbs.uptime_secs(),
        "recorded_by": "daemon",
    });
    if let Err(e) = write_atomic(&crumbs.exit_reason, &format!("{body}\n")) {
        tracing::warn!(error = %e, path = %crumbs.exit_reason.display(), "exit-reason write failed");
    }
    if let Err(e) = std::fs::remove_file(&crumbs.heartbeat) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %crumbs.heartbeat.display(), "heartbeat removal failed");
        }
    }
    drop(state);
    crumbs.wake.notify_all();
}

/// Record an exit on a daemon's behalf, from the process that ended it.
///
/// `ainb hangar daemon stop` signals the daemon with `SIGTERM`, whose default
/// disposition runs NO user code in the target — so the daemon itself cannot
/// write this breadcrumb. The stopper knows the reason and writes it here, once
/// it has confirmed the pid is gone. Without this, a deliberate `stop` would be
/// indistinguishable from the SIGKILL/OOM deaths these breadcrumbs exist to
/// catch.
///
/// Best-effort: IO failures are logged and swallowed.
pub fn record_external_exit(hangar_home: &Path, pid: u32, reason: &str) {
    let exit_reason = exit_reason_path_in(hangar_home);
    if let Some(parent) = exit_reason.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, path = %parent.display(), "breadcrumb dir create failed");
            return;
        }
    }
    let body = serde_json::json!({
        "pid": pid,
        "reason": reason,
        "exited_at_ms": epoch_ms(SystemTime::now()),
        "recorded_by": "stopper",
    });
    if let Err(e) = write_atomic(&exit_reason, &format!("{body}\n")) {
        tracing::warn!(error = %e, path = %exit_reason.display(), "exit-reason write failed");
    }
    if let Err(e) = std::fs::remove_file(heartbeat_path_in(hangar_home)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "heartbeat removal failed");
        }
    }
}

/// Install the daemon's panic hook: log the panic through `tracing` (so it
/// reaches the JSON sink) *and* to stderr, then delegate to the previous hook.
///
/// The `ainb` binary installs an equivalent hook in its own `main`, but the
/// standalone `ainb-hangar-daemon` binary had none — and `resolve_daemon_launch_for`
/// prefers that binary whenever it is fresh, so the daemon most likely to be
/// running was the one with no hook at all.
///
/// Chaining to the previous hook keeps the default panic output (location plus
/// `RUST_BACKTRACE` backtrace), which now reaches disk because the launcher
/// captures the child's stderr instead of discarding it.
///
/// Deliberately does NOT touch the breadcrumbs: a panic inside a spawned run
/// task is caught and logged by the run loop and the daemon keeps serving, so
/// writing an exit reason here would declare a live daemon dead.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, phase = phase(), "ainb-hangar-daemon panicked");
        eprintln!("ainb-hangar-daemon panicked (phase {}): {info}", phase());
        previous(info);
    }));
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

    /// Both breadcrumbs sit beside `daemon.pid` under `<hangar_home>/hangar/`,
    /// and — critically — neither the heartbeat nor the exit reason is named
    /// `daemon.*` inside the LOG dir, so they cannot be swept up by the
    /// `starts_with("daemon")` glob the log-tail surfaces use.
    #[test]
    fn breadcrumb_paths_sit_beside_the_pid_file() {
        let home = std::path::Path::new("/tmp/home");
        assert_eq!(
            super::heartbeat_path_in(home),
            crate::pid_path_in(home).with_file_name("daemon.heartbeat")
        );
        assert_eq!(
            super::exit_reason_path_in(home),
            crate::pid_path_in(home).with_file_name("daemon.exit-reason")
        );
    }

    /// The atomic writer replaces the file's contents and leaves no `.tmp`
    /// residue, and the two breadcrumbs use DISTINCT temp names (a shared
    /// `daemon.tmp` would let a heartbeat tick clobber an in-flight exit
    /// reason).
    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = dir.path().join("daemon.heartbeat");
        let exit = dir.path().join("daemon.exit-reason");

        super::write_atomic(&heartbeat, "first\n").unwrap();
        super::write_atomic(&heartbeat, "second\n").unwrap();
        super::write_atomic(&exit, "bye\n").unwrap();

        assert_eq!(std::fs::read_to_string(&heartbeat).unwrap(), "second\n");
        assert_eq!(std::fs::read_to_string(&exit).unwrap(), "bye\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// The stopper-written breadcrumb: exit reason appears, heartbeat is
    /// removed, and the payload names the pid it ended and who recorded it.
    #[test]
    fn external_exit_replaces_heartbeat_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("hangar")).unwrap();
        let heartbeat = super::heartbeat_path_in(dir.path());
        std::fs::write(&heartbeat, "{}\n").unwrap();

        super::record_external_exit(dir.path(), 4242, "stopped by `ainb hangar daemon stop`");

        assert!(
            !heartbeat.exists(),
            "heartbeat must be gone after an observed exit"
        );
        let raw = std::fs::read_to_string(super::exit_reason_path_in(dir.path())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["pid"], 4242);
        assert_eq!(v["recorded_by"], "stopper");
        assert_eq!(v["reason"], "stopped by `ainb hangar daemon stop`");
    }

    /// Full in-process lifecycle. `start_breadcrumbs` installs a process-global
    /// (a `OnceLock`), so this is deliberately the ONE test that calls it —
    /// splitting it would make the second call a no-op and the assertions
    /// meaningless.
    ///
    /// The uncatchable-kill half of the invariant (heartbeat survives, exit
    /// reason absent) cannot be asserted in-process — a test that `kill -9`s
    /// itself has no one left to assert. It is covered by driving the real
    /// binary; what is proven here is that only an OBSERVED exit ever removes
    /// the heartbeat.
    #[test]
    fn breadcrumb_lifecycle_heartbeat_then_exit_reason() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = super::heartbeat_path_in(dir.path());
        let exit_reason = super::exit_reason_path_in(dir.path());
        // A previous run's exit reason must be cleared on start, not inherited.
        std::fs::create_dir_all(exit_reason.parent().unwrap()).unwrap();
        std::fs::write(&exit_reason, "{\"reason\":\"previous run\"}\n").unwrap();

        super::note_phase("boot");
        super::start_breadcrumbs(dir.path());

        assert!(
            heartbeat.exists(),
            "heartbeat must exist as soon as start returns"
        );
        assert!(
            !exit_reason.exists(),
            "a previous run's exit reason must be cleared"
        );
        let beat: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&heartbeat).unwrap()).unwrap();
        assert_eq!(beat["pid"], std::process::id());
        assert_eq!(beat["phase"], "boot");
        assert_eq!(beat["ticks"], 0);

        super::note_phase("shutdown");
        super::record_exit("clean exit: run loop returned");

        assert!(
            !heartbeat.exists(),
            "an observed exit removes the heartbeat"
        );
        let reason: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&exit_reason).unwrap()).unwrap();
        assert_eq!(reason["reason"], "clean exit: run loop returned");
        assert_eq!(reason["phase"], "shutdown");
        assert_eq!(reason["recorded_by"], "daemon");

        // Second exit is a no-op: the first reason stands.
        super::record_exit("should not overwrite");
        let reason: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&exit_reason).unwrap()).unwrap();
        assert_eq!(reason["reason"], "clean exit: run loop returned");
    }
}
