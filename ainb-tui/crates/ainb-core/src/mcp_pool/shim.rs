// ABOUTME: `ainb mcp proxy <socket>` — the per-session stdio shim that
// bridges a Claude Code MCP stdio slot onto a pooled server's unix socket.
//
// Line-framed (never splits a JSON-RPC line across a reconnect) with
// exponential-backoff reconnect: 100ms doubling to 5s cap, gives up after
// ~60s of continuous failure. Exits 0 when stdin closes (session ended).

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

const BACKOFF_START: Duration = Duration::from_millis(100);
const BACKOFF_CAP: Duration = Duration::from_secs(5);
const RETRY_BUDGET: Duration = Duration::from_secs(60);

pub async fn execute(socket_path: PathBuf, session: Option<String>) -> Result<()> {
    // Single persistent stdin reader; lines survive socket reconnects.
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(256);
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if stdin_tx.send(line).await.is_err() {
                return;
            }
        }
        // stdin closed → drop sender; main loop exits.
    });

    let mut stdout = tokio::io::stdout();
    let mut pending: Option<String> = None; // line read but not yet delivered

    'reconnect: loop {
        let stream = match connect_with_backoff(&socket_path).await {
            Some(s) => s,
            None => anyhow::bail!("could not reach pool socket {} within retry budget", socket_path.display()),
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut sock_lines = BufReader::new(read_half).lines();

        // Announce our session as the first line on every (re)connect — the
        // daemon treats each connection as a fresh client, so the label must
        // be re-sent after a reconnect. The proxy strips this before the mux.
        if let Some(ref s) = session {
            if write_line(&mut write_half, &super::proxy::attach_frame(s)).await.is_err() {
                continue 'reconnect;
            }
        }

        // Flush any line that was in hand when the previous socket died.
        if let Some(line) = pending.take() {
            if write_line(&mut write_half, &line).await.is_err() {
                pending = Some(line);
                continue 'reconnect;
            }
        }

        loop {
            tokio::select! {
                line = stdin_rx.recv() => {
                    let Some(line) = line else { return Ok(()) }; // session ended
                    if write_line(&mut write_half, &line).await.is_err() {
                        pending = Some(line);
                        continue 'reconnect;
                    }
                }
                sock = sock_lines.next_line() => {
                    match sock {
                        Ok(Some(line)) => {
                            stdout.write_all(line.as_bytes()).await?;
                            stdout.write_all(b"\n").await?;
                            stdout.flush().await?;
                        }
                        _ => continue 'reconnect, // socket dropped → redial
                    }
                }
            }
        }
    }
}

async fn write_line(half: &mut tokio::net::unix::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    half.write_all(line.as_bytes()).await?;
    half.write_all(b"\n").await
}

async fn connect_with_backoff(path: &PathBuf) -> Option<UnixStream> {
    let start = std::time::Instant::now();
    let mut delay = BACKOFF_START;
    loop {
        match UnixStream::connect(path).await {
            Ok(s) => return Some(s),
            Err(_) if start.elapsed() < RETRY_BUDGET => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(BACKOFF_CAP);
            }
            Err(_) => return None,
        }
    }
}
