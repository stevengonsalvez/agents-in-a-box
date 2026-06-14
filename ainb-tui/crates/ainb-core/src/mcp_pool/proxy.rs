// ABOUTME: Per-server runtime for the MCP pool daemon: unix listener, lazy
// child spawn with process-group hygiene, the mux wiring, refcounted idle
// reaping, and rate-limited restart-on-failure.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
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
/// Hard cap on a single JSON-RPC line. MCP payloads can be large (context7
/// returns whole docs; agent-deck used 10 MB), but a line with no newline must
/// not grow the shared daemon's memory without bound — that's a local DoS
/// across every session. Over-cap → the connection is closed.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

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

    let socket_str = socket_path.display().to_string();
    // Status is written inline (awaited) rather than from a detached task, so
    // a later logical state (e.g. "idle" after reap) can never be overwritten
    // by an earlier-issued "running" task that happened to run later.
    macro_rules! set_status {
        ($clients:expr, $pid:expr, $state:expr) => {
            write_status(&status, &server.name, &socket_str, $clients, $pid, $state, spawn_count).await
        };
    }
    set_status!(0, None, "idle");

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
                    set_status!(0, None, "idle");
                }
            }
            event = rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                match event {
                    Event::ClientConnected(stream) => {
                        if disabled {
                            tracing::warn!("mcp_pool[{}]: refusing client — server disabled after repeated failures", server.name);
                            drop(stream); // shim sees EOF → bounded reconnect then gives up
                            continue;
                        }
                        if clients.len() >= MAX_CLIENTS {
                            tracing::warn!("mcp_pool[{}]: refusing client — at MAX_CLIENTS ({MAX_CLIENTS})", server.name);
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
                                    tracing::warn!("mcp_pool[{}]: spawn refused ({} client(s) waiting): {e}", server.name, clients.len());
                                    if failures >= MAX_CUMULATIVE_FAILURES {
                                        disabled = true;
                                        set_status!(clients.len(), None, "disabled");
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
                        set_status!(clients.len(), child_pid(&child), "running");
                    }
                    Event::ClientLine(id, line) => {
                        let mut child_write_failed = false;
                        for outcome in mux.on_client_line(id, &line) {
                            if !apply_outcome(outcome, &mut child, &clients).await {
                                child_write_failed = true;
                            }
                        }
                        // A failed write to the child's stdin means it died
                        // before ChildExited was processed. Trigger the reset
                        // path so clients reconnect instead of hanging.
                        if child_write_failed {
                            let _ = tx.send(Event::ChildExited);
                        }
                    }
                    Event::ClientGone(id) => {
                        clients.remove(&id);
                        mux.on_client_disconnect(id);
                        if clients.is_empty() && child.is_some() {
                            idle_deadline = Some(Instant::now() + idle_grace);
                        }
                        set_status!(clients.len(), child_pid(&child), if clients.is_empty() { "grace" } else { "running" });
                    }
                    Event::ChildLine(line) => {
                        let mut child_write_failed = false;
                        for outcome in mux.on_child_line(&line) {
                            if !apply_outcome(outcome, &mut child, &clients).await {
                                child_write_failed = true;
                            }
                        }
                        if child_write_failed {
                            let _ = tx.send(Event::ChildExited);
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
                        set_status!(0, None, if disabled { "disabled" } else { "failed" });
                    }
                }
            }
        }
    }
}

/// Current child pid for status reporting, `None` if no child (avoids the
/// confusing `0` a fallback would report).
fn child_pid(child: &Option<ChildHandle>) -> Option<u32> {
    child.as_ref().and_then(|c| c.process.id())
}

async fn write_status(
    status: &StatusMap,
    name: &str,
    socket: &str,
    clients: usize,
    pid: Option<u32>,
    state: &str,
    spawns: u64,
) {
    status.lock().await.insert(
        name.to_string(),
        ServerStatus {
            name: name.to_string(),
            socket: socket.to_string(),
            clients,
            child_pid: pid,
            state: state.to_string(),
            spawn_count: spawns,
        },
    );
}

/// Returns `false` only when a write to the child's stdin failed — the signal
/// the caller uses to drive the crash-reset path. Client/broadcast send
/// failures are benign (a disconnected client is cleaned up via ClientGone).
async fn apply_outcome(
    outcome: Outcome,
    child: &mut Option<ChildHandle>,
    clients: &HashMap<ClientId, mpsc::UnboundedSender<String>>,
) -> bool {
    match outcome {
        Outcome::ToChild(line) => match child {
            // A write error means the child's stdin pipe is broken — it died
            // before ChildExited was processed. Report failure so the caller
            // fires the reset path; the in-flight request is lost and the
            // client retries (same contract as a per-session server).
            Some(handle) => {
                handle.stdin.write_all(line.as_bytes()).await.is_ok()
                    && handle.stdin.write_all(b"\n").await.is_ok()
            }
            // Already post-reset (child reaped). Benign no-op — the reset
            // already ran; don't re-fire ChildExited and inflate `failures`.
            None => true,
        },
        Outcome::ToClient(id, line) => {
            if let Some(tx) = clients.get(&id) {
                let _ = tx.send(line);
            }
            true
        }
        Outcome::Broadcast(line) => {
            for tx in clients.values() {
                let _ = tx.send(line.clone());
            }
            true
        }
    }
}

/// Read one newline-delimited line with a hard size cap. A `take` adapter
/// bounds each line to `MAX_LINE_BYTES + 1`, so an unterminated giant line
/// can never balloon the shared daemon's memory. Returns `None` on EOF, read
/// error, or over-cap (the caller closes the stream in all three cases).
async fn read_capped_line<R>(reader: &mut R, who: &str) -> Option<String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut buf: Vec<u8> = Vec::new();
    let n = reader
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await
        .ok()?;
    if n == 0 {
        return None; // EOF
    }
    if buf.last() == Some(&b'\n') {
        buf.pop(); // normal newline-terminated line
    } else if buf.len() > MAX_LINE_BYTES {
        // Hit the cap with no newline → over-long; close the stream.
        tracing::warn!(
            "mcp_pool: {who} sent a line over {MAX_LINE_BYTES} bytes with no newline — closing"
        );
        return None;
    }
    // else: a final, unterminated line at EOF — yield it as-is.
    Some(String::from_utf8_lossy(&buf).into_owned())
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
            let mut reader = BufReader::new(read_half);
            while let Some(line) = read_capped_line(&mut reader, "client").await {
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
            let mut reader = BufReader::new(stdout);
            while let Some(line) = read_capped_line(&mut reader, "mcp child").await {
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

/// Reap the child and its process group. Signals go via `/bin/kill` (the
/// workspace forbids `unsafe`, so no `libc::kill`); `--` lets it accept the
/// negative (process-group) pid.
///
/// pgid == the child's pid (set via `process_group(0)`). While any group
/// member is alive the kernel won't recycle that pid, so signaling the group
/// is safe. The recycle race only exists once the group is fully empty — so
/// we `try_wait` first: if the direct child is already dead we still TERM the
/// group **once** to sweep any lingering npx/uvx grandchildren, but skip the
/// wait+KILL escalation (the leader is gone; escalating would be the most
/// likely moment to signal a recycled pid).
async fn kill_child(mut handle: ChildHandle) {
    let already_exited = matches!(handle.process.try_wait(), Ok(Some(_)));

    signal_group(handle.pgid, "TERM").await;

    if already_exited {
        // Leader reaped by try_wait; one best-effort group TERM is enough.
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn capped_reader_splits_lines_and_strips_newline() {
        let mut r = BufReader::new(Cursor::new(b"first\nsecond\n".to_vec()));
        assert_eq!(read_capped_line(&mut r, "t").await.as_deref(), Some("first"));
        assert_eq!(read_capped_line(&mut r, "t").await.as_deref(), Some("second"));
        assert_eq!(read_capped_line(&mut r, "t").await, None); // EOF
    }

    #[tokio::test]
    async fn capped_reader_yields_unterminated_final_line() {
        let mut r = BufReader::new(Cursor::new(b"only-line-no-newline".to_vec()));
        assert_eq!(
            read_capped_line(&mut r, "t").await.as_deref(),
            Some("only-line-no-newline")
        );
        assert_eq!(read_capped_line(&mut r, "t").await, None);
    }

    #[tokio::test]
    async fn capped_reader_closes_on_oversize_line() {
        // One byte over the cap, no newline → treated as over-long → None.
        let huge = vec![b'x'; MAX_LINE_BYTES + 1];
        let mut r = BufReader::new(Cursor::new(huge));
        assert_eq!(read_capped_line(&mut r, "t").await, None);
    }

    #[tokio::test]
    async fn capped_reader_accepts_line_at_the_limit() {
        // Exactly MAX_LINE_BYTES of content + newline must still parse.
        let mut data = vec![b'a'; MAX_LINE_BYTES];
        data.push(b'\n');
        let mut r = BufReader::new(Cursor::new(data));
        let line = read_capped_line(&mut r, "t").await.unwrap();
        assert_eq!(line.len(), MAX_LINE_BYTES);
    }
}
