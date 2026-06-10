// ABOUTME: The standalone `ainb mcp daemon` — owns every pooled MCP child,
// serves one unix socket per server plus a control socket for status and
// shutdown. Survives TUI exit; sessions keep their MCP access.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

use super::proxy::{ServerStatus, StatusMap, run_server_proxy};
use super::{paths, pooled_servers};
use crate::config::AppConfig;

#[derive(serde::Serialize)]
struct StatusReport {
    pid: u32,
    version: String,
    servers: Vec<ServerStatus>,
}

/// Run the daemon in the foreground until SIGTERM/SIGINT or a control-socket
/// shutdown. `idle_grace_override` (seconds) trumps config — used by tests
/// and the validation script.
pub async fn execute(idle_grace_override: Option<u64>) -> Result<()> {
    let config = AppConfig::load().unwrap_or_default();
    if !config.mcp_pool.enabled {
        anyhow::bail!("mcp_pool.enabled = false in config — refusing to start");
    }

    let servers = pooled_servers(&config);
    if servers.is_empty() {
        eprintln!("mcp daemon: no pooled servers eligible (check [mcp_servers.*] definitions resolve on this host)");
    }

    paths::ensure_sockets_dir()?;
    let control_path = paths::control_socket()?;
    if paths::socket_alive_or_cleanup(&control_path) {
        anyhow::bail!("mcp daemon already running (control socket alive)");
    }
    let control = UnixListener::bind(&control_path)
        .with_context(|| format!("bind {}", control_path.display()))?;

    let idle_grace = Duration::from_secs(idle_grace_override.unwrap_or(config.mcp_pool.idle_grace_secs));
    let status: StatusMap = Arc::new(Mutex::new(HashMap::new()));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    for server in servers {
        let socket_path = paths::server_socket(&server.name)?;
        let status = status.clone();
        let shutdown_rx = shutdown_rx.clone();
        let name = server.name.clone();
        tokio::spawn(async move {
            if let Err(e) = run_server_proxy(server, socket_path, idle_grace, status, shutdown_rx).await {
                tracing::error!("mcp_pool[{name}]: proxy exited: {e}");
            }
        });
    }

    eprintln!("mcp daemon: listening (control: {})", control_path.display());

    // Control loop + signals.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            accepted = control.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let status = status.clone();
                let shutdown_tx = shutdown_tx.clone();
                tokio::spawn(async move {
                    let _ = handle_control(stream, status, shutdown_tx).await;
                });
            }
            _ = wait_for_shutdown(shutdown_rx.clone()) => break,
        }
    }

    // Graceful: signal proxies to kill children + remove their sockets.
    let _ = shutdown_tx.send(true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = std::fs::remove_file(&control_path);
    eprintln!("mcp daemon: stopped");
    Ok(())
}

async fn wait_for_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn handle_control(
    stream: tokio::net::UnixStream,
    status: StatusMap,
    shutdown: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let cmd = line.trim();
        match cmd {
            "status" | "ping" => {
                let servers: Vec<ServerStatus> = {
                    let map = status.lock().await;
                    let mut v: Vec<_> = map.values().cloned().collect();
                    v.sort_by(|a, b| a.name.cmp(&b.name));
                    v
                };
                let report = StatusReport {
                    pid: std::process::id(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    servers,
                };
                let json = serde_json::to_string(&report)?;
                write_half.write_all(json.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
            }
            "shutdown" => {
                write_half.write_all(b"{\"ok\":true}\n").await?;
                let _ = shutdown.send(true);
                return Ok(());
            }
            _ => {
                write_half.write_all(b"{\"error\":\"unknown command\"}\n").await?;
            }
        }
    }
    Ok(())
}
