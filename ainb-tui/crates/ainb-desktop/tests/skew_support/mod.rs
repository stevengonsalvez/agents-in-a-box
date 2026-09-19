//! A raw-frame hangar listener for the desktop's skew legs, driven the way the
//! daemon crate's `skew_harness.rs` drives daemon N-1: every byte it answers a
//! hello with is either a committed fixture frame or a refusal shaped exactly
//! as `rpc/auth.rs` shapes one. No daemon binary runs behind it, which is how
//! a test can assert that the sidecar never started one.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_hangar_proto::auth::HelloResult;
use ainb_hangar_proto::methods;
use ainb_hangar_proto::protocol::{PROTOCOL_INCOMPATIBLE, ProtocolRange};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// The daemon crate's committed N-1 frames, one copy for every leg.
const FIXTURES: &str =
    include_str!("../../../ainb-hangar-daemon/tests/fixtures/skew_frames.json");

/// The token the listener's home carries; every hello names it.
pub const TOKEN: &str = "mdt_desktop_skew_fixture";

/// One committed frame by name.
pub fn fixture(name: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(FIXTURES).expect("fixtures are JSON");
    parsed[name]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name} is missing"))
        .to_string()
}

/// What the listener answers every `auth/hello` with.
#[derive(Debug, Clone)]
pub enum Hello {
    /// Daemon N-1: the committed bare `{}` ack, and `{}` to everything after.
    NMinusOne,
    /// A daemon whose range does not meet this build's: `PROTOCOL_INCOMPATIBLE`
    /// with a `HelloResult` in `data`, as `rpc/auth.rs` sends it.
    Refuse {
        protocol: ProtocolRange,
        daemon_version: Option<String>,
    },
}

/// A listener on `home`'s plain socket with the home's token written, so a
/// `SidecarConfig` for `home` dials it as the daemon. Dropping it stops
/// accepting.
pub struct Listener {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start answering hellos for `home` as `hello` says. Must be called inside a
/// tokio runtime.
pub fn listen(home: &Path, hello: Hello) -> Listener {
    let token_path = ainb_hangar_proto::auth::token_file_in(home);
    std::fs::create_dir_all(token_path.parent().expect("token dir")).expect("hangar dir");
    std::fs::write(&token_path, format!("{TOKEN}\n")).expect("token file");
    // The plain socket: the versioned alias is the daemon's to publish, and
    // `socket_path_in` falls back to this one when it is absent.
    let socket = home.join("hangar.sock");
    let listener = UnixListener::bind(&socket).expect("bind the fixture socket");
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let hello = hello.clone();
            tokio::spawn(async move {
                let (read_half, mut writer) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                loop {
                    let frame =
                        tokio::time::timeout(Duration::from_secs(60), read_frame(&mut reader))
                            .await;
                    let Ok(Some(frame)) = frame else { return };
                    let id = serde_json::to_string(&frame["id"]).unwrap_or_else(|_| "0".into());
                    let body = match (frame["method"].as_str(), &hello) {
                        (Some(m), Hello::NMinusOne) if m == methods::AUTH_HELLO => format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{}}}"#,
                            fixture("n_minus_1_daemon_hello_ack")
                        ),
                        (
                            Some(m),
                            Hello::Refuse {
                                protocol,
                                daemon_version,
                            },
                        ) if m == methods::AUTH_HELLO => {
                            let client: ProtocolRange =
                                serde_json::from_value(frame["params"]["protocol"].clone())
                                    .unwrap_or_default();
                            let data = serde_json::to_string(&HelloResult {
                                protocol: *protocol,
                                selected: None,
                                capabilities: Vec::new(),
                                daemon_version: daemon_version.clone(),
                                host_id: None,
                            })
                            .expect("data");
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{PROTOCOL_INCOMPATIBLE},"message":"daemon protocol {}-{} cannot serve a client speaking {}-{}; restart from the newer binary","data":{data}}}}}"#,
                                protocol.min, protocol.max, client.min, client.max
                            )
                        }
                        _ => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#),
                    };
                    if writer.write_all(&framed(&body)).await.is_err() {
                        return;
                    }
                    let _ = writer.flush().await;
                }
            });
        }
    });
    Listener { task }
}

/// A "daemon binary" that records it ran by creating `marker`, then exits 0,
/// which is what the real one does when it loses the flock to a daemon that
/// already owns the home.
pub fn recording_daemon(dir: &Path, marker: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let script = dir.join("recording-daemon.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch \"{}\"\nexit 0\n", marker.display()),
    )
    .expect("fixture");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

fn framed(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// The next frame, or `None` once the peer has closed.
async fn read_frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<serde_json::Value> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let mut body = vec![0u8; len?];
            reader.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                len = value.trim().parse().ok();
            }
        }
    }
}
