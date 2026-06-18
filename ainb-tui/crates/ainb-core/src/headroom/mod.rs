// ABOUTME: ainb-managed shared Headroom compression proxy.
// One `headroom proxy --port <N>` process shared by all Headroom-enabled
// sessions. Lazily started on first opt-in session, reaped on quit.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

use crate::interactive::session_manager::HEADROOM_DEFAULT_PORT;

// ── Directory helpers ────────────────────────────────────────────────────────

/// Root directory for headroom runtime files (pid + log).
/// Honors `AINB_HOME` just like `SessionStore::storage_path()`.
fn headroom_dir() -> PathBuf {
    let base = std::env::var_os("AINB_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join(".agents-in-a-box").join("headroom")
}

fn pid_file() -> PathBuf {
    headroom_dir().join("proxy.pid")
}

fn log_file() -> PathBuf {
    headroom_dir().join("proxy.log")
}

// ── Port resolution ──────────────────────────────────────────────────────────

/// Effective port for the Headroom proxy.
/// Reads `AINB_HEADROOM_PORT`; falls back to `HEADROOM_DEFAULT_PORT` (8787).
pub fn proxy_port() -> u16 {
    std::env::var("AINB_HEADROOM_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(HEADROOM_DEFAULT_PORT)
}

// ── Liveness probe ───────────────────────────────────────────────────────────

/// Returns `true` when the Headroom proxy answers `GET /health` within 500ms.
/// Never panics; any error maps to `false`.
pub async fn is_healthy() -> bool {
    let port = proxy_port();
    let url = format!("http://127.0.0.1:{port}/health");
    let client = match reqwest::Client::builder().timeout(Duration::from_millis(500)).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    client.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

// ── Proxy lifecycle ──────────────────────────────────────────────────────────

/// Ensure the Headroom proxy is running.
///
/// If already healthy, returns immediately. Otherwise:
/// 1. Locates the `headroom` binary via `which` (bail with install hint if absent).
/// 2. Spawns `headroom proxy --port <N>` detached into its own process group,
///    stdout+stderr → `~/.agents-in-a-box/headroom/proxy.log`.
/// 3. Writes the child PID to `proxy.pid`.
/// 4. Polls `is_healthy()` for up to 5 s (50 × 100ms); returns `Ok` when live.
pub async fn ensure_proxy_running() -> Result<()> {
    if is_healthy().await {
        return Ok(());
    }

    // Locate binary — descriptive error if not on PATH.
    let headroom_bin = which::which("headroom").map_err(|_| {
        anyhow::anyhow!(
            "headroom binary not found on PATH — install it with:\n  \
             uv tool install 'headroom-ai[proxy]'"
        )
    })?;

    let dir = headroom_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create headroom dir {}", dir.display()))?;

    let log_path = log_file();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open headroom log {}", log_path.display()))?;

    let port = proxy_port();
    let mut cmd = std::process::Command::new(&headroom_bin);
    cmd.args(["proxy", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));

    // Detach into its own process group so terminal signals (ctrl-c aimed at
    // the spawning CLI/TUI) never reach the proxy.
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd.spawn().context("spawn headroom proxy")?;
    let pid = child.id();

    // Persist PID for stop() to use.
    std::fs::write(pid_file(), pid.to_string())
        .with_context(|| format!("write pid file {}", pid_file().display()))?;

    info!(
        "spawned headroom proxy (pid={pid}, port={port}, log={})",
        log_path.display()
    );

    // Poll up to ~5 s for the health endpoint. Async sleep so we yield the
    // tokio worker instead of blocking it during session creation.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if is_healthy().await {
            info!("headroom proxy is healthy on port {port}");
            return Ok(());
        }
    }

    anyhow::bail!(
        "headroom proxy did not come up within 5s (see {})",
        log_path.display()
    )
}

// ── Stats ────────────────────────────────────────────────────────────────────

/// Summary statistics reported by the Headroom proxy `/stats` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct HeadroomStats {
    #[serde(default)]
    pub tokens_saved: u64,
    #[serde(default)]
    pub requests_total: u64,
}

/// Intermediate shape for deserializing `/stats` JSON:
/// `{"summary":{"tokens_saved_total":N,"requests_total":N}}`.
#[derive(Debug, Deserialize)]
struct StatsResponse {
    #[serde(default)]
    summary: StatsSummary,
}

#[derive(Debug, Default, Deserialize)]
struct StatsSummary {
    #[serde(default)]
    tokens_saved_total: u64,
    #[serde(default)]
    requests_total: u64,
}

/// Fetch `/stats` from the Headroom proxy; returns `None` on any error.
pub async fn stats() -> Option<HeadroomStats> {
    let port = proxy_port();
    let url = format!("http://127.0.0.1:{port}/stats");
    let client = reqwest::Client::builder().timeout(Duration::from_millis(500)).build().ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: StatsResponse = resp.json().await.ok()?;
    Some(HeadroomStats {
        tokens_saved: body.summary.tokens_saved_total,
        requests_total: body.summary.requests_total,
    })
}

// ── Status ───────────────────────────────────────────────────────────────────

/// Combined status of the ainb-managed Headroom proxy.
#[derive(Debug, Clone)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: u16,
    pub pid: Option<u32>,
    pub tokens_saved: Option<u64>,
}

/// Query the proxy for a combined status snapshot.
pub async fn status() -> ProxyStatus {
    let port = proxy_port();
    let running = is_healthy().await;
    let pid = read_pid();
    let tokens_saved = if running {
        stats().await.map(|s| s.tokens_saved)
    } else {
        None
    };
    ProxyStatus {
        running,
        port,
        pid,
        tokens_saved,
    }
}

// ── Stop ─────────────────────────────────────────────────────────────────────

/// Stop the ainb-managed Headroom proxy.
///
/// Reads the PID file, sends SIGTERM to the process, removes the PID file.
/// Best-effort — never panics.
pub fn stop() {
    let Some(pid) = read_pid() else {
        return;
    };

    // Use nix::sys::signal::kill if available (nix is in the workspace).
    let killed = try_kill(pid);
    if killed {
        info!("sent SIGTERM to headroom proxy (pid={pid})");
    } else {
        warn!("could not kill headroom proxy (pid={pid}): process may have already exited");
    }

    // Remove pid file regardless of kill outcome.
    let _ = std::fs::remove_file(pid_file());
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Send SIGTERM to `pid`. Returns `true` if the signal was sent (or the process
/// was already gone — both are fine outcomes).
fn try_kill(pid: u32) -> bool {
    use nix::sys::signal::{Signal, kill as nix_kill};
    use nix::unistd::Pid;
    match nix_kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => {
            // Process already gone — not an error.
            true
        }
        Err(e) => {
            warn!("nix::kill({pid}, SIGTERM) failed: {e}");
            false
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Serializes every test that reads or mutates `AINB_HEADROOM_PORT`. Cargo runs
/// tests in-process in parallel, so a setter in one test races a reader in
/// another (e.g. `headroom_base_url()` in the session_manager tests). All such
/// tests — in this module AND others — must hold this lock. See
/// [reference: ENV_LOCK for parallel tests].
#[cfg(test)]
pub(crate) static HEADROOM_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// `proxy_port()` must honor `AINB_HEADROOM_PORT` override.
    #[test]
    fn proxy_port_honors_override() {
        let _guard = HEADROOM_ENV_LOCK.lock().unwrap();
        // Isolate: stash any existing value, set ours, restore after.
        let key = "AINB_HEADROOM_PORT";
        let old = std::env::var_os(key);

        std::env::set_var(key, "9999");
        assert_eq!(proxy_port(), 9999);

        // Restore env.
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        // Default restored: should be HEADROOM_DEFAULT_PORT.
        assert_eq!(proxy_port(), HEADROOM_DEFAULT_PORT);
    }

    /// Parsing the Headroom `/stats` JSON shape into `HeadroomStats`.
    #[test]
    fn headroom_stats_parses_summary() {
        let json = r#"{"summary":{"tokens_saved_total":1220217,"requests_total":196}}"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        let s = HeadroomStats {
            tokens_saved: raw.summary.tokens_saved_total,
            requests_total: raw.summary.requests_total,
        };
        assert_eq!(s.tokens_saved, 1_220_217);
        assert_eq!(s.requests_total, 196);
    }

    /// Missing or empty summary fields must default to 0 (no panic).
    #[test]
    fn headroom_stats_defaults_on_absent_fields() {
        let json = r#"{"summary":{}}"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(raw.summary.tokens_saved_total, 0);
        assert_eq!(raw.summary.requests_total, 0);
    }

    /// Entirely missing `summary` key must also work (default-deriving outer struct).
    #[test]
    fn headroom_stats_defaults_on_no_summary() {
        let json = r#"{}"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(raw.summary.tokens_saved_total, 0);
    }
}
