//! A fake hangar daemon for the P6e resolver tests: it answers hello with the
//! sessions capability and each session request as the test says, and counts
//! what it was sent.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;

/// Connections the fake daemon has accepted.
pub static ACCEPTED: AtomicUsize = AtomicUsize::new(0);
/// Session requests (past hello) the fake daemon has read.
pub static REQUESTS: AtomicUsize = AtomicUsize::new(0);
/// Upserts the fake daemon has been sent.
pub static UPSERTS: AtomicUsize = AtomicUsize::new(0);
/// How long the fake daemon takes over each upsert it answers, in ms.
pub static UPSERT_DELAY_MS: AtomicUsize = AtomicUsize::new(0);

/// A fake daemon on `home`'s plain socket, with a token file, as
/// `DaemonClient::from_env` finds it. It answers hello with the sessions
/// capability, then each session request with `reply(method, n)`, where `n`
/// counts session requests across connections; `None` never answers.
pub fn fake_daemon(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    reply: fn(&str, usize) -> Option<serde_json::Value>,
) -> PathBuf {
    ACCEPTED.store(0, Ordering::SeqCst);
    REQUESTS.store(0, Ordering::SeqCst);
    UPSERTS.store(0, Ordering::SeqCst);
    fs::write(ainb_hangar_proto::auth::token_file_in(home), "t\n").unwrap();
    let socket = home.join("hangar.sock");
    let listener = rt.block_on(async { UnixListener::bind(&socket) }).unwrap();
    rt.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            ACCEPTED.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (read_half, mut writer) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let Some(hello) = read_frame(&mut reader).await else {
                    return;
                };
                let caps = serde_json::json!({ "capabilities": [CAP_WORKSPACE_SESSIONS] });
                write_frame(
                    &mut writer,
                    &serde_json::json!({"jsonrpc": "2.0", "id": hello["id"], "result": caps}),
                )
                .await;
                let Some(req) = read_frame(&mut reader).await else {
                    return;
                };
                let n = REQUESTS.fetch_add(1, Ordering::SeqCst);
                let method = req["method"].as_str().unwrap_or_default();
                if method == "workspace/session_upsert" {
                    UPSERTS.fetch_add(1, Ordering::SeqCst);
                    let delay = UPSERT_DELAY_MS.load(Ordering::SeqCst) as u64;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let Some(mut resp) = reply(method, n) else {
                    // Never answer: hold the connection open.
                    let _ = read_frame(&mut reader).await;
                    std::future::pending::<()>().await;
                    return;
                };
                resp["jsonrpc"] = "2.0".into();
                resp["id"] = req["id"].clone();
                write_frame(&mut writer, &resp).await;
            });
        }
    });
    socket
}

async fn read_frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<serde_json::Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            len = n.parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len?];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

async fn write_frame(writer: &mut tokio::net::unix::OwnedWriteHalf, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let _ = writer.write_all(&frame).await;
    let _ = writer.flush().await;
}

pub fn ready_list() -> serde_json::Value {
    serde_json::json!({ "result": { "sessions": [], "truncated": false, "import_complete": true } })
}

pub fn upsert_ok() -> serde_json::Value {
    serde_json::json!({ "result": { "ok": true } })
}
