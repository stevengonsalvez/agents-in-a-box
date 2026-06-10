// ABOUTME: Env-gated runtime performance instrumentation for the TUI hot loop.
//
// Enabled only when `AINB_PERF_TRACE` is set in the environment; otherwise
// every hook is a single relaxed atomic-bool load and returns immediately, so
// the instrumented binary is representative of normal runtime behaviour. The
// facility records cold-start (process-start → first paint), per-frame draw
// duration, the number of times the favorites store is parsed from disk, and
// key-to-render latency. A summary is printed to stderr on `report()`.
//
// This is a measurement aid retained behind a flag (not a product feature).
// Run with `AINB_PERF_TRACE=1 ainb 2> perf.log` and read the summary at exit.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Whether tracing is active for this process. Resolved once from the
/// `AINB_PERF_TRACE` env var.
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Process start, captured as early as possible in `main`.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

struct Metrics {
    frames: AtomicU64,
    draw_ns_total: AtomicU64,
    draw_ns_max: AtomicU64,
    favorites_loads: AtomicU64,
    keys_seen: AtomicU64,
    first_paint_ns: OnceLock<u64>,
    /// Key-to-render samples in nanoseconds (bounded to avoid unbounded growth
    /// on very long sessions).
    key_to_render_ns: Mutex<Vec<u64>>,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

const MAX_LATENCY_SAMPLES: usize = 200_000;

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        frames: AtomicU64::new(0),
        draw_ns_total: AtomicU64::new(0),
        draw_ns_max: AtomicU64::new(0),
        favorites_loads: AtomicU64::new(0),
        keys_seen: AtomicU64::new(0),
        first_paint_ns: OnceLock::new(),
        key_to_render_ns: Mutex::new(Vec::new()),
    })
}

/// Mark the process start. Call once at the very top of `main`.
///
/// When tracing is enabled, also spawns a low-frequency background thread that
/// writes the running summary to `$AINB_PERF_TRACE_FILE` (default
/// `/tmp/ainb-perf/live.txt`) every two seconds. This makes the measurement
/// independent of how the process exits — a profiling harness can read the
/// file and then kill the process without relying on a clean shutdown path.
pub fn init() {
    let _ = PROCESS_START.set(Instant::now());
    if enabled() {
        // Force metric allocation up front so later hooks never race on init.
        let _ = metrics();
        std::thread::spawn(|| {
            let path = trace_file_path();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let _ = std::fs::write(&path, summary_string());
            }
        });
    }
}

fn trace_file_path() -> std::path::PathBuf {
    std::env::var_os("AINB_PERF_TRACE_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/ainb-perf/live.txt"))
}

/// Is performance tracing enabled for this process?
#[inline]
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("AINB_PERF_TRACE").is_some())
}

/// Record that the favorites store was loaded (parsed) from disk. Cheap hot
/// path; only counts when tracing is enabled.
#[inline]
pub fn record_favorites_load() {
    if !enabled() {
        return;
    }
    metrics().favorites_loads.fetch_add(1, Ordering::Relaxed);
}

/// Record the duration of a single `terminal.draw()`. Also captures the
/// time-to-first-paint relative to process start the first time it is called.
#[inline]
pub fn record_draw(dur: Duration) {
    if !enabled() {
        return;
    }
    let m = metrics();
    let ns = dur.as_nanos() as u64;
    m.frames.fetch_add(1, Ordering::Relaxed);
    m.draw_ns_total.fetch_add(ns, Ordering::Relaxed);
    m.draw_ns_max.fetch_max(ns, Ordering::Relaxed);
    if m.first_paint_ns.get().is_none() {
        if let Some(start) = PROCESS_START.get() {
            let _ = m.first_paint_ns.set(start.elapsed().as_nanos() as u64);
        }
    }
}

/// Record that a key event was received by the event loop.
#[inline]
pub fn record_key() {
    if !enabled() {
        return;
    }
    metrics().keys_seen.fetch_add(1, Ordering::Relaxed);
}

/// Record a key-to-render latency sample (keypress observed → next paint done).
#[inline]
pub fn record_key_to_render(dur: Duration) {
    if !enabled() {
        return;
    }
    if let Ok(mut v) = metrics().key_to_render_ns.lock() {
        if v.len() < MAX_LATENCY_SAMPLES {
            v.push(dur.as_nanos() as u64);
        }
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Build the human-readable summary string from the current metrics.
fn summary_string() -> String {
    use std::fmt::Write as _;
    let m = metrics();
    let frames = m.frames.load(Ordering::Relaxed);
    let draw_total = m.draw_ns_total.load(Ordering::Relaxed);
    let draw_max = m.draw_ns_max.load(Ordering::Relaxed);
    let fav = m.favorites_loads.load(Ordering::Relaxed);
    let keys = m.keys_seen.load(Ordering::Relaxed);
    let first_paint = m.first_paint_ns.get().copied().unwrap_or(0);
    let mut samples = m
        .key_to_render_ns
        .lock()
        .map(|v| v.clone())
        .unwrap_or_default();
    samples.sort_unstable();

    let ms = |ns: u64| ns as f64 / 1_000_000.0;
    let avg_draw = if frames > 0 { draw_total / frames } else { 0 };

    let mut s = String::new();
    let _ = writeln!(s, "=== ainb perf trace ===");
    let _ = writeln!(s, "cold start -> first paint : {:.2} ms", ms(first_paint));
    let _ = writeln!(s, "frames drawn             : {frames}");
    let _ = writeln!(s, "keys received            : {keys}");
    let _ = writeln!(s, "draw avg / max           : {:.3} / {:.3} ms", ms(avg_draw), ms(draw_max));
    let _ = writeln!(s, "total time in draw()     : {:.1} ms", ms(draw_total));
    let _ = writeln!(s, "favorites store loads    : {fav}");
    let _ = writeln!(s, "key-to-render samples    : {}", samples.len());
    if !samples.is_empty() {
        let _ = writeln!(
            s,
            "key-to-render p50/p95/p99/max : {:.3} / {:.3} / {:.3} / {:.3} ms",
            ms(percentile(&samples, 50.0)),
            ms(percentile(&samples, 95.0)),
            ms(percentile(&samples, 99.0)),
            ms(*samples.last().unwrap()),
        );
    }
    let _ = writeln!(s, "=======================");
    s
}

/// Emit a human-readable summary to stderr. No-op when tracing is disabled.
pub fn report() {
    if !enabled() {
        return;
    }
    eprintln!("\n{}", summary_string());
}
