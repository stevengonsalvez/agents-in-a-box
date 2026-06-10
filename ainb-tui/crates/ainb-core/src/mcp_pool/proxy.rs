// ABOUTME: Per-server runtime for the MCP pool daemon: unix listener, lazy
// child spawn with process-group hygiene, the mux wiring, refcounted idle
// reaping, and rate-limited restart-on-failure.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

use super::PooledServer;
use super::mux::{ClientId, Mux, Outcome};

const MAX_CLIENTS: usize = 100;
/// Restart hygiene (mirrors agent-deck): ≥5s between spawns, ≤3 per minute,
/// permanently disabled after 10 cumulative failures.
const MIN_RESTART_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RESTARTS_PER_MINUTE: usize = 3;
const MAX_CUMULATIVE_FAILURES: usize = 10;
const KILL_GRACE: Duration = Duration::from_secs(3);

/// Live status snapshot, shared with the daemon's control socket.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ServerStatus {
    pub name: String,
    pub socket: String,
    pub clients: usize,
    pub child_pid: Option<u32>,
    pub state: String,
    pub spawn_count: u64,
}

pub type StatusMap = Arc<Mutex<HashMap<String, ServerStatus>>>;

enum Event {
    ClientConnected(UnixStream),
    ClientLine(ClientId, String),
    ClientGone(ClientId),
    ChildLine(String),
    ChildExited,
}

struct ChildHandle {
    process: Child,
    stdin: tokio::process::ChildStdin,
    pgid: i32,
}

/// Run one pooled server's proxy until the daemon shuts down.
pub async fn run_server_proxy(
    server: PooledServer,
    socket_path: PathBuf,
    idle_grace: Duration,
    status: StatusMap,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if super::paths::socket_alive_or_cleanup(&socket_path) {
        anyhow::bail!("socket {} already served by another daemon", socket_path.display());
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind {}", socket_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();

    // Accept loop → events.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        if tx.send(Event::ClientConnected(stream)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let mut mux = Mux::new();
    let mut clients: HashMap<ClientId, mpsc::UnboundedSender<String>> = HashMap::new();
    let mut next_client: ClientId = 0;
    let mut child: Option<ChildHandle> = None;
    let mut idle_deadline: Option<Instant> = None;
    let mut spawn_times: Vec<Instant> = Vec::new();
    let mut failures: usize = 0;
    let mut disabled = false;
    let mut spawn_count: u64 = 0;

    let update_status = |clients: usize, pid: Option<u32>, state: &str, spawns: u64| {
        let status = status.clone();
        let name = server.name.clone();
        let socket = socket_path.display().to_string();
        let state = state.to_string();
        tokio::spawn(async move {
            status.lock().await.insert(
                name.clone(),
                ServerStatus { name, socket, clients, child_pid: pid, state, spawn_count: spawns },
            );
        });
    };
    update_status(0, None, "idle", 0);

    loop {
        let idle_sleep = async {
            match idle_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    if let Some(handle) = child.take() {
                        kill_child(handle).await;
                    }
                    let _ = std::fs::remove_file(&socket_path);
                    return Ok(());
                }
            }
            _ = idle_sleep => {
                idle_deadline = None;
                if clients.is_empty() {
                    if let Some(handle) = child.take() {
                        tracing::info!("mcp_pool[{}]: idle grace expired, reaping child", server.name);
                        kill_child(handle).await;
                        mux.reset_for_child_restart();
                    }
                    update_status(0, None, "idle", spawn_count);
                }
            }
            event = rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                match event {
                    Event::ClientConnected(stream) => {
                        if disabled || clients.len() >= MAX_CLIENTS {
                            drop(stream);
                            continue;
                        }
                        // Lazy spawn on first client.
                        if child.is_none() {
                            match try_spawn(&server, &tx, &mut spawn_times, &mut failures) {
                                Ok(handle) => {
                                    spawn_count += 1;
                                    child = Some(handle);
                                }
                                Err(e) => {
                                    tracing::warn!("mcp_pool[{}]: spawn refused: {e}", server.name);
                                    if failures >= MAX_CUMULATIVE_FAILURES {
                                        disabled = true;
                                        update_status(clients.len(), None, "disabled", spawn_count);
                                    }
                                    drop(stream);
                                    continue;
                                }
                            }
                        }
                        idle_deadline = None;
                        next_client += 1;
                        let id = next_client;
                        let (write_tx, write_rx) = mpsc::unbounded_channel::<String>();
                        clients.insert(id, write_tx);
                        spawn_client_io(id, stream, tx.clone(), write_rx);
                        update_status(clients.len(), child.as_ref().map(|c| c.process.id().unwrap_or(0)), "running", spawn_count);
                    }
                    Event::ClientLine(id, line) => {
                        for outcome in mux.on_client_line(id, &line) {
                            apply_outcome(outcome, &mut child, &clients).await;
                        }
                    }
                    Event::ClientGone(id) => {
                        clients.remove(&id);
                        mux.on_client_disconnect(id);
                        if clients.is_empty() && child.is_some() {
                            idle_deadline = Some(Instant::now() + idle_grace);
                        }
                        update_status(clients.len(), child.as_ref().map(|c| c.process.id().unwrap_or(0)), if clients.is_empty() { "grace" } else { "running" }, spawn_count);
                    }
                    Event::ChildLine(line) => {
                        for outcome in mux.on_child_line(&line) {
                            apply_outcome(outcome, &mut child, &clients).await;
                        }
                    }
                    Event::ChildExited => {
                        tracing::warn!("mcp_pool[{}]: child exited", server.name);
                        failures += 1;
                        if let Some(handle) = child.take() {
                            kill_child(handle).await; // reap zombie + stragglers
                        }
                        mux.reset_for_child_restart();
                        // Drop all clients so shims reconnect (and trigger a
                        // fresh lazy spawn, rate-limited in try_spawn).
                        clients.clear();
                        idle_deadline = None;
                        if failures >= MAX_CUMULATIVE_FAILURES {
                            disabled = true;
                        }
                        update_status(0, None, if disabled { "disabled" } else { "failed" }, spawn_count);
                    }
                }
            }
        }
    }
}

async fn apply_outcome(
    outcome: Outcome,
    child: &mut Option<ChildHandle>,
    clients: &HashMap<ClientId, mpsc::UnboundedSender<String>>,
) {
    match outcome {
        Outcome::ToChild(line) => {
            if let Some(handle) = child {
                let _ = handle.stdin.write_all(line.as_bytes()).await;
                let _ = handle.stdin.write_all(b"\n").await;
            }
        }
        Outcome::ToClient(id, line) => {
            if let Some(tx) = clients.get(&id) {
                let _ = tx.send(line);
            }
        }
        Outcome::Broadcast(line) => {
            for tx in clients.values() {
                let _ = tx.send(line.clone());
            }
        }
    }
}

fn spawn_client_io(
    id: ClientId,
    stream: UnixStream,
    events: mpsc::UnboundedSender<Event>,
    mut write_rx: mpsc::UnboundedReceiver<String>,
) {
    let (read_half, mut write_half) = stream.into_split();
    // Socket → events
    {
        let events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if events.send(Event::ClientLine(id, line)).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::ClientGone(id));
        });
    }
    // Outcomes → socket
    tokio::spawn(async move {
        while let Some(line) = write_rx.recv().await {
            if write_half.write_all(line.as_bytes()).await.is_err()
                || write_half.write_all(b"\n").await.is_err()
            {
                return;
            }
        }
    });
}

fn try_spawn(
    server: &PooledServer,
    events: &mpsc::UnboundedSender<Event>,
    spawn_times: &mut Vec<Instant>,
    failures: &mut usize,
) -> Result<ChildHandle> {
    if *failures >= MAX_CUMULATIVE_FAILURES {
        anyhow::bail!("server disabled after {failures} failures");
    }
    let now = Instant::now();
    spawn_times.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
    if let Some(last) = spawn_times.last() {
        if now.duration_since(*last) < MIN_RESTART_INTERVAL {
            anyhow::bail!("respawn throttled (min {MIN_RESTART_INTERVAL:?} between spawns)");
        }
    }
    if spawn_times.len() >= MAX_RESTARTS_PER_MINUTE {
        anyhow::bail!("respawn throttled (max {MAX_RESTARTS_PER_MINUTE}/min)");
    }

    let mut cmd = Command::new(&server.command);
    cmd.args(&server.args)
        .envs(&server.env)
        .env_remove("LD_PRELOAD")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(child_stderr_log(&server.name));
    // Scrub injection vectors (macOS) — children inherit our env otherwise.
    for (key, _) in std::env::vars() {
        if key.starts_with("DYLD_") {
            cmd.env_remove(&key);
        }
    }
    // New process group so npx/uvx grandchildren die with the group.
    cmd.process_group(0);
    cmd.kill_on_drop(true);

    let mut process = cmd.spawn().with_context(|| format!("spawn {}", server.command))?;
    spawn_times.push(now);

    let pid = process.id().context("child has no pid")? as i32;
    let stdin = process.stdin.take().context("child stdin")?;
    let stdout = process.stdout.take().context("child stdout")?;

    // Child stdout → events
    {
        let events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if events.send(Event::ChildLine(line)).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::ChildExited);
        });
    }

    tracing::info!("mcp_pool[{}]: spawned child pid {pid}", server.name);
    Ok(ChildHandle { process, stdin, pgid: pid })
}

fn child_stderr_log(name: &str) -> std::process::Stdio {
    super::paths::pool_dir()
        .ok()
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(format!("{name}.stderr.log")))
                .ok()
        })
        .map_or_else(std::process::Stdio::null, std::process::Stdio::from)
}

/// SIGTERM the process group, wait up to KILL_GRACE, then SIGKILL the group.
/// Signals go via /bin/kill (the workspace forbids unsafe, so no libc::kill);
/// `--` lets it accept the negative (process-group) pid.
async fn kill_child(mut handle: ChildHandle) {
    signal_group(handle.pgid, "TERM").await;
    let waited = tokio::time::timeout(KILL_GRACE, handle.process.wait()).await;
    if waited.is_err() {
        signal_group(handle.pgid, "KILL").await;
        let _ = handle.process.wait().await;
    }
}

async fn signal_group(pgid: i32, sig: &str) {
    let _ = Command::new("/bin/kill")
        .args([format!("-{sig}"), "--".to_string(), format!("-{pgid}")])
        .output()
        .await;
}
