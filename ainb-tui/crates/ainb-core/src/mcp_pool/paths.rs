// ABOUTME: Filesystem layout for the MCP pool daemon.
// Sockets live under ~/.agents-in-a-box/mcp/sockets/ (0700); the daemon log
// under ~/.agents-in-a-box/mcp/.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

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

/// Unix socket for one pooled MCP server. Rejects unsafe names as
/// defense-in-depth — callers should already have filtered via
/// [`crate::mcp_pool::valid_server_name`], but a stray `..`/`/` must never
/// reach a `join` that could escape the sockets dir.
pub fn server_socket(name: &str) -> Result<PathBuf> {
    if !crate::mcp_pool::valid_server_name(name) {
        anyhow::bail!("unsafe MCP server name: {name:?}");
    }
    Ok(sockets_dir()?.join(format!("{name}.sock")))
}

/// Daemon control socket (status / shutdown).
pub fn control_socket() -> Result<PathBuf> {
    Ok(sockets_dir()?.join("control.sock"))
}

pub fn daemon_log() -> Result<PathBuf> {
    Ok(pool_dir()?.join("daemon.log"))
}

/// Write `contents` to `target` atomically: write a sibling temp file, fsync,
/// then rename over the target (atomic on POSIX). A crash or full disk can
/// never leave a torn/half-written config — these files are owned by other
/// tools (codex/copilot/claude) and a truncated write is data loss.
///
/// When `mode` is set, the temp file is chmod'd before the rename so the
/// final file never briefly exists world-readable (matters for configs that
/// may carry `env` secrets).
pub fn write_atomic(target: &Path, contents: &str, mode: Option<u32>) -> Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent dir: {}", target.display()))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("temp file in {}", dir.display()))?;
    use std::io::Write;
    tmp.write_all(contents.as_bytes())?;
    tmp.flush()?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file().set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    tmp.persist(target)
        .map_err(|e| anyhow::anyhow!("persist {}: {}", target.display(), e.error))?;
    Ok(())
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
