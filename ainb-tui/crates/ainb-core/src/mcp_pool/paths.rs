// ABOUTME: Filesystem layout for the MCP pool daemon.
// Sockets live under ~/.agents-in-a-box/mcp/sockets/ (0700); the daemon log
// under ~/.agents-in-a-box/mcp/.

use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn pool_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".agents-in-a-box").join("mcp"))
}

pub fn sockets_dir() -> Result<PathBuf> {
    Ok(pool_dir()?.join("sockets"))
}

/// Create the sockets dir with owner-only permissions.
pub fn ensure_sockets_dir() -> Result<PathBuf> {
    let dir = sockets_dir()?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Unix socket for one pooled MCP server.
pub fn server_socket(name: &str) -> Result<PathBuf> {
    Ok(sockets_dir()?.join(format!("{name}.sock")))
}

/// Daemon control socket (status / shutdown).
pub fn control_socket() -> Result<PathBuf> {
    Ok(sockets_dir()?.join("control.sock"))
}

pub fn daemon_log() -> Result<PathBuf> {
    Ok(pool_dir()?.join("daemon.log"))
}

/// True when a unix socket file exists AND something is accepting on it.
/// Removes the file if it's stale (exists but nothing listening).
pub fn socket_alive_or_cleanup(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => true,
        Err(_) => {
            let _ = std::fs::remove_file(path);
            false
        }
    }
}
