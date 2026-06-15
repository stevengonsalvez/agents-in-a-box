// ABOUTME: The standalone `ainb mcp daemon` — owns every pooled MCP child,
// serves one unix socket per server plus a control socket for status,
// shutdown, and runtime server registration. Survives TUI exit.
//
// Registration matters because the daemon's own AppConfig::load() only sees
// project config at the daemon's cwd; sessions created in OTHER projects
// push their pooled-server definitions over the control socket instead.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc};

use super::proxy::{ServerStatus, StatusMap, run_server_proxy};
use super::{PooledServer, paths, pooled_servers};
use crate::config::AppConfig;

#[derive(serde::Serialize)]
struct StatusReport {
    pid: u32,
    version: String,
    servers: Vec<ServerStatus>,
}

#[derive(serde::Deserialize)]
struct ControlMsg {
    cmd: String,
    #[serde(default)]
    servers: Vec<PooledServer>,
    /// Target server for `stop_server`.
    #[serde(default)]
    name: Option<String>,
}

/// Commands the control handler routes to the daemon's main loop (which owns
/// the per-server stop signals and the running set).
enum DaemonCmd {
    StopServer(String),
    Shutdown,
}

/// Run the daemon in the foreground until SIGTERM/SIGINT or a control-socket
/// shutdown. `idle_grace_override` (seconds) trumps config — used by tests
/// and the validation script.
pub async fn execute(idle_grace_override: Option<u64>) -> Result<()> {
    let config = AppConfig::load().unwrap_or_default();
    if !config.mcp_pool.enabled {
        anyhow::bail!("mcp_pool.enabled = false in config — refusing to start");
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
    let (register_tx, mut register_rx) = mpsc::unbounded_channel::<Vec<PooledServer>>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<DaemonCmd>();

    // Per-server stop signal. Firing one returns just that proxy (reaps its
    // child + removes its socket); global shutdown fires all of them.
    let mut proxy_stops: HashMap<String, tokio::sync::watch::Sender<bool>> = HashMap::new();
    let mut running: HashSet<String> = HashSet::new();
    let spawn_proxy = |server: PooledServer,
                       status: StatusMap,
                       idle_grace: Duration|
     -> tokio::sync::watch::Sender<bool> {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let name = server.name.clone();
        match paths::server_socket(&server.name) {
            Ok(socket_path) => {
                tokio::spawn(async move {
                    if let Err(e) = run_server_proxy(server, socket_path, idle_grace, status, stop_rx).await {
                        tracing::error!("mcp_pool[{name}]: proxy exited: {e}");
                    }
                });
            }
            Err(e) => tracing::error!("mcp_pool[{name}]: socket path: {e}"),
        }
        stop_tx
    };

    for server in pooled_servers(&config) {
        let name = server.name.clone();
        let stop_tx = spawn_proxy(server, status.clone(), idle_grace);
        running.insert(name.clone());
        proxy_stops.insert(name, stop_tx);
    }

    eprintln!("mcp daemon: listening (control: {})", control_path.display());

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            registered = register_rx.recv() => {
                let Some(servers) = registered else { continue };
                for server in servers {
                    if !running.contains(&server.name) && server.resolvable_on_host() {
                        eprintln!("mcp daemon: registered '{}'", server.name);
                        let name = server.name.clone();
                        let stop_tx = spawn_proxy(server, status.clone(), idle_grace);
                        running.insert(name.clone());
                        proxy_stops.insert(name, stop_tx);
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(DaemonCmd::StopServer(name)) => {
                        if let Some(stop_tx) = proxy_stops.remove(&name) {
                            let _ = stop_tx.send(true); // reaps child + removes socket
                            running.remove(&name);
                            status.lock().await.remove(&name);
                            eprintln!("mcp daemon: stopped server '{name}'");
                        }
                    }
                    Some(DaemonCmd::Shutdown) | None => break,
                }
            }
            accepted = control.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let status = status.clone();
                let cmd_tx = cmd_tx.clone();
                let register_tx = register_tx.clone();
                tokio::spawn(async move {
                    let _ = handle_control(stream, status, cmd_tx, register_tx).await;
                });
            }
        }
    }

    // Graceful: fire every per-server stop → kill children + remove sockets.
    for stop_tx in proxy_stops.values() {
        let _ = stop_tx.send(true);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = std::fs::remove_file(&control_path);
    eprintln!("mcp daemon: stopped");
    Ok(())
}

async fn handle_control(
    stream: tokio::net::UnixStream,
    status: StatusMap,
    cmd_chan: mpsc::UnboundedSender<DaemonCmd>,
    register: mpsc::UnboundedSender<Vec<PooledServer>>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        // JSON commands (register / stop_server); plain words for shell probing.
        let cmd = if line.starts_with('{') {
            match serde_json::from_str::<ControlMsg>(line) {
                Ok(msg) => {
                    if msg.cmd == "register" {
                        let count = msg.servers.len();
                        let _ = register.send(msg.servers);
                        write_half
                            .write_all(format!("{{\"ok\":true,\"registered\":{count}}}\n").as_bytes())
                            .await?;
                        continue;
                    }
                    if msg.cmd == "stop_server" {
                        match msg.name {
                            Some(name) => {
                                let _ = cmd_chan.send(DaemonCmd::StopServer(name));
                                write_half.write_all(b"{\"ok\":true}\n").await?;
                            }
                            None => {
                                write_half.write_all(b"{\"error\":\"stop_server needs name\"}\n").await?;
                            }
                        }
                        continue;
                    }
                    msg.cmd
                }
                Err(_) => {
                    write_half.write_all(b"{\"error\":\"bad json\"}\n").await?;
                    continue;
                }
            }
        } else {
            line.to_string()
        };

        match cmd.as_str() {
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
                let _ = cmd_chan.send(DaemonCmd::Shutdown);
                return Ok(());
            }
            _ => {
                write_half.write_all(b"{\"error\":\"unknown command\"}\n").await?;
            }
        }
    }
    Ok(())
}
