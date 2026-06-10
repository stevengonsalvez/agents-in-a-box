// ABOUTME: Daemon-side helpers used by session creation and `ainb mcp
// status/stop`: liveness probe, auto-spawn (detached), status query.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use super::paths;

/// True when the daemon's control socket answers within ~500ms.
pub fn daemon_alive() -> bool {
    let Ok(path) = paths::control_socket() else { return false };
    if !path.exists() {
        return false;
    }
    query(&path, "ping").is_ok()
}

/// Raw status JSON from the daemon.
pub fn daemon_status() -> Result<String> {
    let path = paths::control_socket()?;
    query(&path, "status")
}

/// Ask the daemon to shut down (kills pooled children).
pub fn daemon_stop() -> Result<String> {
    let path = paths::control_socket()?;
    query(&path, "shutdown")
}

/// Push server definitions to a RUNNING daemon so it can pool them even
/// though its own cwd-scoped config never mentioned them. Unknown names get
/// proxies spawned; already-running names are ignored.
pub fn register_servers(servers: &[super::PooledServer]) -> Result<String> {
    let path = paths::control_socket()?;
    let msg = serde_json::json!({"cmd": "register", "servers": servers});
    query(&path, &msg.to_string())
}

fn query(path: &std::path::Path, cmd: &str) -> Result<String> {
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .with_context(|| format!("connect {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(cmd.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Make sure a daemon is running: probe, else spawn `ainb mcp daemon`
/// detached (own session, stdio → daemon.log) and poll until the control
/// socket answers. Errors here must NOT block session creation — callers
/// treat failure as "fall back to per-session stdio".
pub fn ensure_daemon() -> Result<()> {
    if daemon_alive() {
        return Ok(());
    }

    let exe = std::env::current_exe().context("current_exe")?;
    let log_path = paths::daemon_log()?;
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["mcp", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    // Detach into its own process group so terminal signals aimed at the
    // spawning CLI/TUI (ctrl-c etc.) never reach the daemon.
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("spawn ainb mcp daemon")?;

    // Poll up to ~3s for the control socket.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if daemon_alive() {
            return Ok(());
        }
    }
    anyhow::bail!("mcp daemon did not come up within 3s (see {})", paths::daemon_log()?.display())
}
