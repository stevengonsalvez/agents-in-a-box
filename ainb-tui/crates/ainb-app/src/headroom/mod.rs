// ABOUTME: ainb-managed shared Headroom compression proxy.
// One `headroom proxy --port <N>` process shared by all Headroom-enabled
// sessions. Lazily started on first opt-in session, reaped on quit.

use std::path::PathBuf;
use std::time::Duration;
use std::{io::Read, io::Write};

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

/// Serializes `ensure_proxy_running` so concurrent callers can't double-spawn
/// the proxy on the same port (and clobber `proxy.pid`). Real runtime lock —
/// distinct from the test-only `HEADROOM_ENV_LOCK`.
static SPAWN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn log_file() -> PathBuf {
    headroom_dir().join("proxy.log")
}

// ── Availability ───────────────────────────────────────────────────────────

/// Whether the `headroom` binary is on `PATH`. Cheap (one PATH lookup) — call
/// it once when a screen opens, not per render. Used to gate the per-session
/// toggle: no point offering Headroom if it can't be run.
pub fn is_installed() -> bool {
    which::which("headroom").is_ok()
}

// ── Port resolution ──────────────────────────────────────────────────────────

/// Effective port for the Headroom proxy: `AINB_HEADROOM_PORT`, else
/// `usage_client.headroom_port`, else [`HEADROOM_DEFAULT_PORT`] (8787).
pub fn proxy_port() -> u16 {
    crate::config::tunables::resolved(
        "AINB_HEADROOM_PORT",
        crate::config::tunables::snapshot().usage_client.headroom_port,
    )
}

// ── Liveness probe ───────────────────────────────────────────────────────────

/// Synchronous liveness: is anything accepting on the proxy port?
///
/// [`is_healthy`] is the richer probe but it is async, and the Daemons
/// background collector runs on a plain thread with no tokio runtime under it.
/// A bounded TCP connect answers the only question that row needs — is the
/// proxy listening — without dragging a runtime into the collector.
#[must_use]
pub fn is_listening() -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], proxy_port()));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// The pid of the ainb-managed proxy, when one is recorded.
#[must_use]
pub fn pid() -> Option<u32> {
    read_pid()
}

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
/// 4. Polls `/health` for up to 5 s (50 × 100ms); returns `Ok` when live.
pub async fn ensure_proxy_running() -> Result<()> {
    if is_healthy().await {
        return Ok(());
    }

    // Serialize the spawn. Two concurrent callers — a session launch and the
    // 10s watchdog tick, or two overlapping watchdog ticks — could otherwise
    // both observe `is_healthy() == false` and both `cmd.spawn()` a proxy on
    // the same port. The loser exits on bind failure but still overwrites
    // `proxy.pid` (below), orphaning the real proxy from `stop()`/idle-reap.
    let _spawn_guard = SPAWN_LOCK.lock().await;

    // flock can block for the full 5s startup window, so keep all filesystem
    // and health-poll work off the Tokio worker thread.
    tokio::task::spawn_blocking(|| ensure_proxy_running_under_process_lock(false))
        .await
        .context("headroom proxy startup task panicked")?
}

/// The watchdog's respawn: [`ensure_proxy_running`], but only while a live
/// surface holds a proxy user lease (see [`register_user`]).
///
/// Checked under `proxy.pid.lock`, so a respawn cannot land after the last
/// user has released and stopped the proxy, which would orphan a proxy
/// nobody uses.
pub async fn ensure_proxy_running_for_live_users() -> Result<()> {
    if is_healthy().await {
        return Ok(());
    }
    let _spawn_guard = SPAWN_LOCK.lock().await;
    tokio::task::spawn_blocking(|| ensure_proxy_running_under_process_lock(true))
        .await
        .context("headroom proxy startup task panicked")?
}

/// Run the complete probe/spawn/health sequence while holding `proxy.pid.lock`.
/// A second ainb process waits here instead of racing a failed bind and
/// overwriting the first process's PID file.
fn ensure_proxy_running_under_process_lock(require_live_user: bool) -> Result<()> {
    let dir = headroom_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create headroom dir {}", dir.display()))?;

    let pid_path = pid_file();
    let _process_lock = crate::config::lock::lock_for(&pid_path)
        .with_context(|| format!("lock headroom pid file {}", pid_path.display()))?;

    let port = proxy_port();
    if is_healthy_blocking(port) {
        return Ok(());
    }
    if require_live_user && live_users(None).is_empty() {
        info!("headroom proxy down but no live user holds a lease; not respawning");
        return Ok(());
    }

    let headroom_bin = which::which("headroom").map_err(|_| {
        anyhow::anyhow!(
            "headroom binary not found on PATH — install it with:\n  \
             uv tool install 'headroom-ai[proxy]'"
        )
    })?;
    let log_path = log_file();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open headroom log {}", log_path.display()))?;

    let mut cmd = std::process::Command::new(&headroom_bin);
    cmd.args(["proxy", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().context("spawn headroom proxy")?;
    let pid = child.id();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if is_healthy_blocking(port) {
            // Do not write a PID for a child that lost its bind race. A live
            // incumbent owns the port, and stop() must never target it.
            if child.try_wait()?.is_none() {
                std::fs::write(&pid_path, pid.to_string())
                    .with_context(|| format!("write pid file {}", pid_path.display()))?;
                info!(
                    "spawned headroom proxy (pid={pid}, port={port}, log={})",
                    log_path.display()
                );
            }
            return Ok(());
        }
    }

    anyhow::bail!(
        "headroom proxy did not come up within 5s (see {})",
        log_path.display()
    )
}

/// Synchronous `/health` probe for the blocking startup critical section.
fn is_healthy_blocking(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 32];
    match stream.read(&mut response) {
        Ok(n) => {
            response[..n].starts_with(b"HTTP/1.1 2") || response[..n].starts_with(b"HTTP/1.0 2")
        }
        Err(_) => false,
    }
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

/// Intermediate shape for deserializing the Headroom `/stats` JSON. The real
/// payload (headroom 0.26) is large; we only pull the two canonical figures:
/// `savings.total_tokens` (total tokens saved across all layers) and
/// `summary.api_requests` (requests seen by the proxy). Verified against a live
/// proxy on 2026-06-19 — an earlier guess at `summary.tokens_saved_total`
/// silently parsed to 0 because that key does not exist.
#[derive(Debug, Default, Deserialize)]
struct StatsResponse {
    #[serde(default)]
    summary: StatsSummary,
    #[serde(default)]
    savings: StatsSavings,
}

#[derive(Debug, Default, Deserialize)]
struct StatsSummary {
    #[serde(default)]
    api_requests: u64,
}

#[derive(Debug, Default, Deserialize)]
struct StatsSavings {
    #[serde(default)]
    total_tokens: u64,
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
        tokens_saved: body.savings.total_tokens,
        requests_total: body.summary.api_requests,
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
/// Returns `true` when there was an ainb-managed proxy to stop (a pid file
/// existed), `false` when there was nothing to do — e.g. the user is running
/// their own `headroom proxy`, which we must NOT kill. Best-effort, never
/// panics.
pub fn stop() -> bool {
    let Some(pid) = read_pid() else {
        return false;
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
    true
}

// ── Proxy users ──────────────────────────────────────────────────────────────

/// One empty file per live surface (a TUI today, the desktop host next) that
/// may route sessions through the shared proxy, named by its pid.
///
/// A liveness check rather than a reference count: a TUI that crashes never
/// decrements a counter, but its pid stops answering `kill(pid, 0)`, so the
/// stale file is pruned the next time anyone counts.
fn users_dir() -> PathBuf {
    headroom_dir().join("users")
}

/// Record this process as a user of the shared proxy. Call once at startup.
pub fn register_user() -> Result<()> {
    let dir = users_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create headroom users dir {}", dir.display()))?;
    let lease = dir.join(std::process::id().to_string());
    std::fs::write(&lease, b"")
        .with_context(|| format!("write headroom user lease {}", lease.display()))
}

/// Pids with a lease whose process is still alive, excluding `except`.
/// Removes the lease of every pid that is gone.
fn live_users(except: Option<u32>) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(users_dir()) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if Some(pid) == except {
            continue;
        }
        if process_is_alive(pid) {
            live.push(pid);
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    live.sort_unstable();
    live
}

/// `kill(pid, 0)`: `EPERM` still means the process exists.
fn process_is_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    matches!(
        kill(Pid::from_raw(raw), None),
        Ok(()) | Err(nix::errno::Errno::EPERM)
    )
}

/// Drop this process's lease, then stop the ainb-managed proxy only if no
/// other live user remains. Returns `true` when the proxy was stopped.
///
/// The whole release runs under `proxy.pid.lock`, the lock the spawn path
/// holds from its health probe until `proxy.pid` is written. Taking it before
/// touching the lease or reading the pid is what makes release and a watchdog
/// spawn mutually exclusive: a spawn still in its health poll has not written
/// `proxy.pid` yet, so a release that read the pid outside the lock would see
/// nothing to stop and leave that proxy running with no lease.
pub fn release_user_and_stop_if_unused() -> bool {
    let me = std::process::id();
    let pid_path = pid_file();
    let _process_lock = std::fs::create_dir_all(headroom_dir())
        .and_then(|()| crate::config::lock::lock_for(&pid_path))
        .map_err(|e| warn!("lock headroom pid file {}: {e}", pid_path.display()))
        .ok();
    let _ = std::fs::remove_file(users_dir().join(me.to_string()));
    if read_pid().is_none() {
        return false;
    }
    let others = live_users(Some(me));
    if !others.is_empty() {
        info!(
            "leaving shared headroom proxy running: {} other live user(s) {others:?}",
            others.len()
        );
        return false;
    }
    stop()
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
/// tests, in this module AND others, must hold this lock. See
/// [reference: ENV_LOCK for parallel tests].
///
/// It is the crate's one environment lock under a second name. It used to be a
/// mutex of its own, which ordered these tests against each other and against
/// nothing else: a test holding it wrote `AINB_HOME` while a test holding
/// `TEST_ENV_LOCK` wrote `HOME`, both at once, which is the race either lock was
/// there to stop.
pub use crate::env_lock::ENV_LOCK as HEADROOM_ENV_LOCK;

#[cfg(test)]
mod tests {
    use super::*;

    const CROSS_PROCESS_ROLE: &str = "AINB_HEADROOM_CROSS_PROCESS_TEST_ROLE";
    const CROSS_PROCESS_TEST_NAME: &str =
        "headroom::tests::cross_process_guard_prevents_second_proxy_spawn";

    /// Kills every test-owned child by its recorded PID if an assertion fails
    /// before the normal cleanup path runs. The fake proxy has no descendants:
    /// its shell script immediately `exec`s this test binary, so each PID is
    /// exact throughout its lifetime.
    struct TestProcessCleanup {
        callers: Vec<std::process::Child>,
        pid_path: PathBuf,
        spawn_log: PathBuf,
    }

    impl TestProcessCleanup {
        fn new(pid_path: PathBuf, spawn_log: PathBuf) -> Self {
            Self {
                callers: Vec::new(),
                pid_path,
                spawn_log,
            }
        }

        fn spawned_pids(&self) -> Vec<u32> {
            let mut pids: Vec<u32> = std::fs::read_to_string(&self.spawn_log)
                .map(|log| log.lines().filter_map(|line| line.parse().ok()).collect())
                .unwrap_or_default();
            if let Some(pid) = std::fs::read_to_string(&self.pid_path)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
            {
                pids.push(pid);
            }
            pids.sort_unstable();
            pids.dedup();
            pids
        }

        fn terminate_pid(pid: u32) {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }

        fn wait_until_gone(pid: u32) -> bool {
            use nix::sys::signal::kill;
            use nix::unistd::Pid;

            let target = Pid::from_raw(pid as i32);
            for _ in 0..100 {
                match kill(target, None) {
                    Err(nix::errno::Errno::ESRCH) => return true,
                    Ok(()) | Err(nix::errno::Errno::EPERM) => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return false,
                }
            }
            false
        }
    }

    impl Drop for TestProcessCleanup {
        fn drop(&mut self) {
            for caller in &mut self.callers {
                if matches!(caller.try_wait(), Ok(None)) {
                    let _ = caller.kill();
                    let _ = caller.wait();
                }
            }
            for pid in self.spawned_pids() {
                Self::terminate_pid(pid);
            }
        }
    }

    /// Terminates exact PIDs observed by a regression test before it reports a
    /// failure. This guard never searches for or signals unrelated processes.
    struct ExactPidCleanup(Vec<u32>);

    impl Drop for ExactPidCleanup {
        fn drop(&mut self) {
            for &pid in &self.0 {
                TestProcessCleanup::terminate_pid(pid);
            }
            for &pid in &self.0 {
                let _ = TestProcessCleanup::wait_until_gone(pid);
            }
        }
    }

    fn wait_for_path(path: &std::path::Path, description: &str) {
        for _ in 0..500 {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {description}: {}", path.display());
    }

    fn wait_for_child(child: &mut std::process::Child, description: &str) {
        for _ in 0..1_000 {
            match child.try_wait().expect("read test caller status") {
                Some(status) => {
                    assert!(status.success(), "{description} failed: {status}");
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("timed out waiting for {description}");
    }

    /// Returns sorted direct children for an exact parent PID without PATH lookups.
    fn direct_child_pids(pid: u32) -> Vec<u32> {
        use sysinfo::{Pid, ProcessesToUpdate, System};

        let parent = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, false);
        // sysinfo lists a task's threads alongside real processes, and a
        // thread's parent is the process that owns it. Without the
        // `thread_kind` filter every thread of the exec'd fake proxy counted
        // as a child process, so this assertion failed on a tokio runtime
        // rather than on a leaked subprocess.
        let mut children: Vec<u32> = system
            .processes()
            .iter()
            .filter(|(_, process)| process.thread_kind().is_none())
            .filter_map(|(&child_pid, process)| {
                (process.parent() == Some(parent)).then_some(child_pid.as_u32())
            })
            .collect();
        children.sort_unstable();
        children
    }

    fn process_is_alive(pid: u32) -> bool {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        matches!(
            kill(Pid::from_raw(pid as i32), None),
            Ok(()) | Err(nix::errno::Errno::EPERM)
        )
    }

    fn write_fake_headroom(fake_headroom: &std::path::Path) {
        std::fs::write(
            fake_headroom,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> \"$AINB_HEADROOM_CROSS_PROCESS_SPAWN_LOG\"\nexport {CROSS_PROCESS_ROLE}=proxy\nexec \"$AINB_HEADROOM_CROSS_PROCESS_TEST_EXE\" --exact \"{CROSS_PROCESS_TEST_NAME}\" --nocapture\n"
            ),
        )
        .expect("write fake headroom executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(fake_headroom)
                .expect("read fake headroom permissions")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(fake_headroom, permissions)
                .expect("make fake headroom executable");
        }
    }

    /// Serves the fake proxy's health endpoint until the parent test terminates
    /// this exact process ID. This binary is launched only by the test-owned
    /// `headroom` executable below.
    fn run_fake_proxy() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, proxy_port()))
            .expect("bind test-owned headroom loopback port");
        listener.set_nonblocking(true).expect("make test listener nonblocking");

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("reply to headroom health probe");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept test-owned headroom connection: {error}"),
            }
        }
    }

    /// Regression test for assertion cleanup: the fake executable must `exec`
    /// immediately. A shell child before `exec` would survive cleanup of the
    /// shell's exact recorded PID.
    #[test]
    fn failure_cleanup_leaves_no_fake_proxy_children() {
        let temp = tempfile::tempdir().expect("test tempdir");
        let fake_headroom = temp.path().join("headroom");
        let spawn_log = temp.path().join("headroom-spawns");
        let pid_path = temp.path().join("proxy.pid");
        let test_exe = std::env::current_exe().expect("locate current test executable");
        let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("reserve test-owned loopback port")
            .local_addr()
            .expect("read reserved loopback port")
            .port();
        write_fake_headroom(&fake_headroom);

        let mut fake_proxy = spawn_retrying_busy_text(
            std::process::Command::new(&fake_headroom)
                .env("AINB_HEADROOM_CROSS_PROCESS_SPAWN_LOG", &spawn_log)
                .env("AINB_HEADROOM_CROSS_PROCESS_TEST_EXE", &test_exe)
                .env("AINB_HEADROOM_PORT", port.to_string()),
        );
        let fake_proxy_pid = fake_proxy.id();
        let cleanup = TestProcessCleanup::new(pid_path, spawn_log.clone());

        // #1129: wait on the facts the checks below depend on, never on a
        // fixed sleep. The log FILE appears before its line is written (`>>`
        // creates it first), and a cleanup that read an empty log signalled
        // nothing and left `wait` below blocked. Children are listed only once
        // the script has exec'd the proxy, which is the process under test.
        wait_for_spawn_record(&spawn_log, fake_proxy_pid);
        // Not asserted yet: a proxy that never exec'd is exactly the regression
        // this test guards, and its children must still be listed and cleaned
        // up before the test reports it.
        let exec_seen = wait_for_exec(fake_proxy_pid, &test_exe);
        let child_pids = direct_child_pids(fake_proxy_pid);
        let child_cleanup = ExactPidCleanup(child_pids.clone());

        drop(cleanup);
        let surviving_children: Vec<u32> =
            child_pids.iter().copied().filter(|pid| process_is_alive(*pid)).collect();
        drop(child_cleanup);
        // Judged on the child handle, not by probing the PID after the reap: a
        // reaped PID can be handed to an unrelated process.
        let exited = wait_for_exit(&mut fake_proxy);

        assert!(
            exec_seen,
            "fake proxy {fake_proxy_pid} never exec'd {}; children: {child_pids:?}",
            test_exe.display()
        );
        assert!(
            child_pids.is_empty(),
            "fake proxy created children before exec: {child_pids:?}"
        );
        assert!(
            surviving_children.is_empty(),
            "failure cleanup left fake proxy children alive: {surviving_children:?}"
        );
        assert!(
            exited,
            "test-owned fake proxy {fake_proxy_pid} survived exact cleanup"
        );
    }

    /// Spawn `command`, retrying while the kernel reports its executable busy.
    ///
    /// The fake `headroom` script was just written. Under `cargo test` another
    /// test thread can fork while that write descriptor is open, and until the
    /// forked child execs it holds the descriptor, so exec'ing the script
    /// fails with `ETXTBSY` (#1129). The condition clears when that child
    /// execs; the retry is bounded.
    fn spawn_retrying_busy_text(command: &mut std::process::Command) -> std::process::Child {
        for _ in 0..500 {
            match command.spawn() {
                Ok(child) => return child,
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("spawn test-owned fake proxy: {error}"),
            }
        }
        panic!("spawn test-owned fake proxy: executable stayed busy for 5s");
    }

    /// Wait until the spawn log holds `pid`, not merely until the file exists.
    fn wait_for_spawn_record(spawn_log: &std::path::Path, pid: u32) {
        for _ in 0..500 {
            let recorded = std::fs::read_to_string(spawn_log)
                .is_ok_and(|log| log.lines().any(|line| line.trim() == pid.to_string()));
            if recorded {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out waiting for fake proxy {pid} in the spawn log: {}",
            spawn_log.display()
        );
    }

    /// Whether `pid` comes to run `exe` within five seconds, i.e. the fake
    /// script has exec'd the proxy. Returns rather than panics, so the caller's
    /// cleanup still runs when it never does.
    fn wait_for_exec(pid: u32, exe: &std::path::Path) -> bool {
        let canonical = |path: &std::path::Path| {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        let wanted = canonical(exe);
        for _ in 0..500 {
            let running = crate::fleet::daemons::heartbeat::process_binary(pid)
                .is_some_and(|path| canonical(&path) == wanted);
            if running {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Whether `child` exits within five seconds; reaps it when it does.
    fn wait_for_exit(child: &mut std::process::Child) -> bool {
        for _ in 0..500 {
            if child.try_wait().expect("read fake proxy status").is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Starts two independent test binaries at one barrier. Both callers see
    /// an initially-unhealthy port. Only the caller holding `proxy.pid.lock`
    /// may invoke the fake `headroom`; the other must wait, observe its healthy
    /// proxy, and return without a second spawn.
    #[tokio::test(flavor = "current_thread")]
    async fn cross_process_guard_prevents_second_proxy_spawn() {
        match std::env::var(CROSS_PROCESS_ROLE).as_deref() {
            Ok("caller") => {
                let ready = std::env::var_os("AINB_HEADROOM_CROSS_PROCESS_CALLER_READY")
                    .map(PathBuf::from)
                    .expect("caller ready path");
                let start = std::env::var_os("AINB_HEADROOM_CROSS_PROCESS_START")
                    .map(PathBuf::from)
                    .expect("caller start path");
                std::fs::write(&ready, "ready").expect("mark cross-process caller ready");
                wait_for_path(&start, "cross-process caller start");
                ensure_proxy_running().await.expect("ensure shared headroom proxy");
                return;
            }
            Ok("proxy") => {
                run_fake_proxy();
                return;
            }
            Ok(role) => panic!("unknown headroom cross-process test role: {role}"),
            Err(_) => {}
        }

        let _guard = HEADROOM_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = tempfile::tempdir().expect("test tempdir");
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir(&bin_dir).expect("create fake PATH directory");
        let spawn_log = temp.path().join("headroom-spawns");
        let caller_one_ready = temp.path().join("caller-one-ready");
        let caller_two_ready = temp.path().join("caller-two-ready");
        let start = temp.path().join("start-callers");
        let pid_path = temp.path().join(".agents-in-a-box").join("headroom").join("proxy.pid");
        let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("reserve test-owned loopback port")
            .local_addr()
            .expect("read reserved loopback port")
            .port();
        let test_exe = std::env::current_exe().expect("locate current test executable");

        let fake_headroom = bin_dir.join("headroom");
        write_fake_headroom(&fake_headroom);

        let mut cleanup = TestProcessCleanup::new(pid_path.clone(), spawn_log.clone());
        for ready in [&caller_one_ready, &caller_two_ready] {
            let caller = std::process::Command::new(&test_exe)
                .args(["--exact", CROSS_PROCESS_TEST_NAME, "--nocapture"])
                .env(CROSS_PROCESS_ROLE, "caller")
                .env("AINB_HEADROOM_CROSS_PROCESS_CALLER_READY", ready)
                .env("AINB_HEADROOM_CROSS_PROCESS_START", &start)
                .env("AINB_HEADROOM_CROSS_PROCESS_SPAWN_LOG", &spawn_log)
                .env("AINB_HEADROOM_CROSS_PROCESS_TEST_EXE", &test_exe)
                .env("AINB_HOME", temp.path())
                .env("AINB_HEADROOM_PORT", port.to_string())
                .env("PATH", &bin_dir)
                .spawn()
                .expect("spawn independent headroom caller");
            cleanup.callers.push(caller);
        }

        wait_for_path(&caller_one_ready, "first cross-process caller");
        wait_for_path(&caller_two_ready, "second cross-process caller");
        std::fs::write(&start, "go").expect("release cross-process callers");

        for caller in &mut cleanup.callers {
            wait_for_child(caller, "cross-process headroom caller");
        }

        let spawned = cleanup.spawned_pids();
        assert_eq!(
            spawned.len(),
            1,
            "concurrent callers spawned more than one headroom proxy: {spawned:?}"
        );
        let managed_pid = std::fs::read_to_string(&pid_path)
            .expect("managed proxy pid file")
            .trim()
            .parse::<u32>()
            .expect("numeric managed proxy pid");
        assert_eq!(
            spawned,
            vec![managed_pid],
            "proxy.pid must name sole fake proxy"
        );

        TestProcessCleanup::terminate_pid(managed_pid);
        assert!(
            TestProcessCleanup::wait_until_gone(managed_pid),
            "test-owned headroom proxy {managed_pid} survived exact SIGTERM"
        );
        std::fs::remove_file(&pid_path).expect("remove cleaned proxy pid file");
        std::fs::remove_file(&spawn_log).expect("remove cleaned proxy spawn log");
    }

    /// `proxy_port()` must honor `AINB_HEADROOM_PORT` override.
    #[test]
    fn proxy_port_honors_override() {
        let _guard = HEADROOM_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// Parse the REAL Headroom `/stats` shape (headroom 0.26, captured from a
    /// live proxy) — `savings.total_tokens` + `summary.api_requests`, nested
    /// among many sibling keys we ignore.
    #[test]
    fn headroom_stats_parses_real_shape() {
        let json = r#"{
            "summary":{"mode":"token","api_requests":196,
                "compression":{"total_tokens_removed":12345}},
            "agent_usage":{"totals":{"tokens_saved":999}},
            "savings":{"total_tokens":1220217,"per_project":{}}
        }"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        let s = HeadroomStats {
            tokens_saved: raw.savings.total_tokens,
            requests_total: raw.summary.api_requests,
        };
        assert_eq!(s.tokens_saved, 1_220_217);
        assert_eq!(s.requests_total, 196);
    }

    /// Missing nested fields must default to 0 (no panic) — the real payload
    /// reports zeros before any traffic.
    #[test]
    fn headroom_stats_defaults_on_absent_fields() {
        let json = r#"{"summary":{},"savings":{}}"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(raw.savings.total_tokens, 0);
        assert_eq!(raw.summary.api_requests, 0);
    }

    /// Entirely missing `summary`/`savings` keys must also work.
    #[test]
    fn headroom_stats_defaults_on_empty_object() {
        let json = r#"{}"#;
        let raw: StatsResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(raw.savings.total_tokens, 0);
        assert_eq!(raw.summary.api_requests, 0);
    }

    /// Point `AINB_HOME` (and the proxy port) at a scratch home for one test,
    /// restoring both on drop. Callers hold `HEADROOM_ENV_LOCK`.
    struct ScratchHome {
        _dir: tempfile::TempDir,
        old_home: Option<std::ffi::OsString>,
        old_port: Option<std::ffi::OsString>,
    }

    impl ScratchHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("scratch home");
            let old_home = std::env::var_os("AINB_HOME");
            let old_port = std::env::var_os("AINB_HEADROOM_PORT");
            // A port nothing listens on, so the proxy always reads as down.
            let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .expect("reserve port")
                .local_addr()
                .expect("port")
                .port();
            std::env::set_var("AINB_HOME", dir.path());
            std::env::set_var("AINB_HEADROOM_PORT", port.to_string());
            std::fs::create_dir_all(users_dir()).expect("users dir");
            Self {
                _dir: dir,
                old_home,
                old_port,
            }
        }
    }

    impl Drop for ScratchHome {
        fn drop(&mut self) {
            match self.old_home.take() {
                Some(v) => std::env::set_var("AINB_HOME", v),
                None => std::env::remove_var("AINB_HOME"),
            }
            match self.old_port.take() {
                Some(v) => std::env::set_var("AINB_HEADROOM_PORT", v),
                None => std::env::remove_var("AINB_HEADROOM_PORT"),
            }
        }
    }

    fn sleeper() -> std::process::Child {
        std::process::Command::new("sleep").arg("600").spawn().expect("spawn sleep")
    }

    /// A pid that no longer exists: a child that has been reaped.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        pid
    }

    /// Another live TUI's lease keeps the proxy running when this one quits.
    #[test]
    fn a_live_users_lease_keeps_the_proxy_on_release() {
        let _guard = HEADROOM_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _home = ScratchHome::new();
        let mut proxy = sleeper();
        let mut other_tui = sleeper();
        std::fs::write(pid_file(), proxy.id().to_string()).expect("proxy.pid");
        register_user().expect("register this process");
        std::fs::write(users_dir().join(other_tui.id().to_string()), b"").expect("lease");

        let stopped = release_user_and_stop_if_unused();

        let proxy_running = proxy.try_wait().expect("poll proxy").is_none();
        let _ = proxy.kill();
        let _ = proxy.wait();
        let _ = other_tui.kill();
        let _ = other_tui.wait();
        assert!(!stopped, "a live user's lease must keep the proxy");
        assert!(proxy_running, "the proxy must not receive SIGTERM");
        assert!(pid_file().exists());
        assert!(
            !users_dir().join(std::process::id().to_string()).exists(),
            "release drops this process's own lease"
        );
    }

    /// A crashed TUI leaves its lease behind; it must not pin the proxy.
    #[test]
    fn a_crashed_users_stale_lease_does_not_keep_the_proxy() {
        let _guard = HEADROOM_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _home = ScratchHome::new();
        let mut proxy = sleeper();
        std::fs::write(pid_file(), proxy.id().to_string()).expect("proxy.pid");
        let crashed = dead_pid();
        std::fs::write(users_dir().join(crashed.to_string()), b"").expect("stale lease");
        register_user().expect("register this process");

        let stopped = release_user_and_stop_if_unused();

        let exited = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            proxy.try_wait().expect("poll proxy").is_some()
        });
        if !exited {
            let _ = proxy.kill();
            let _ = proxy.wait();
        }
        assert!(stopped, "the last live user's release stops the proxy");
        assert!(exited, "the proxy must receive SIGTERM");
        assert!(!pid_file().exists());
        assert!(
            !users_dir().join(crashed.to_string()).exists(),
            "the dead pid's lease is pruned"
        );
    }

    /// The watchdog must not bring back a proxy nobody holds a lease on. The
    /// spawn path opens `proxy.log` before it spawns, so no log and no pid
    /// file proves the spawn was never attempted.
    #[test]
    fn the_watchdog_does_not_respawn_without_a_live_user() {
        let _guard = HEADROOM_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _home = ScratchHome::new();
        std::fs::write(users_dir().join(dead_pid().to_string()), b"").expect("stale lease");

        let watchdog = ensure_proxy_running_under_process_lock(true);

        assert!(watchdog.is_ok(), "watchdog with no live user: {watchdog:?}");
        assert!(!log_file().exists(), "no spawn was attempted");
        assert!(!pid_file().exists(), "no proxy was spawned");
    }
}
