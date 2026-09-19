//! The desktop leg of the skew harness (spec D17, multi-surface `:139` and
//! `:267`): the sidecar supervisor against a daemon of another version.
//!
//! ```text
//!                       daemon N-1 (committed frames)   daemon N+1 (refuses)
//!  Sidecar supervisor         leg 8                          leg 9
//! ```
//!
//! Daemon N-1 is the daemon crate's `tests/fixtures/skew_frames.json`, included
//! by path so the contract has one copy. Daemon N+1 is a listener answering
//! `PROTOCOL_INCOMPATIBLE` with the `HelloResult` the real daemon puts in
//! `error.data` (`rpc/auth.rs`, `incompatible`). Neither is a binary: the
//! `daemon_bin` every test hands the supervisor is a script that leaves a
//! marker file, so "never spawned" is a file that does not exist.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState};
use ainb_hangar_proto::methods;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::watch;

const FIXTURES: &str = include_str!("../../ainb-hangar-daemon/tests/fixtures/skew_frames.json");

fn fixture(name: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(FIXTURES).expect("fixtures are JSON");
    parsed[name]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name} is missing"))
        .to_string()
}

fn framed(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

async fn read_frame(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> serde_json::Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return serde_json::Value::Null;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            length = rest.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; length.expect("Content-Length")];
    reader.read_exact(&mut body).await.expect("frame body");
    serde_json::from_slice(&body).expect("frame is JSON")
}

/// A hangar home with a token file and a raw-frame daemon on its socket.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("scratch home"),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join(".agents-in-a-box")
    }

    /// The "daemon binary": a script that records it was run.
    fn recording_daemon_bin(&self) -> (PathBuf, PathBuf) {
        let marker = self.dir.path().join("daemon-was-spawned");
        let script = self.dir.path().join("fake-daemon.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
        )
        .expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        (script, marker)
    }

    fn config(&self, daemon_bin: PathBuf) -> SidecarConfig {
        let home = self.home();
        std::fs::create_dir_all(home.join("hangar")).expect("hangar dir");
        std::fs::write(
            ainb_hangar_proto::auth::token_file_in(&home),
            "mdt_skew_fixture\n",
        )
        .expect("token");
        let mut config = SidecarConfig::new(home, daemon_bin);
        // Short budgets: the point of these tests is that the supervisor does
        // not wait on a daemon that has already answered.
        config.grace = Duration::from_millis(200);
        config.hello_budget = Duration::from_secs(3);
        config.reconnect_backoff = vec![Duration::from_millis(50); 2];
        config
    }

    /// Serve `reply_for(method, id)` on this home's socket, one frame per
    /// request, forever.
    fn serve(&self, reply_for: fn(&str, &serde_json::Value) -> String) {
        let socket = ainb_hangar_client::socket_path_in(&self.home());
        let listener = UnixListener::bind(&socket).expect("bind fixture daemon");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (read_half, mut writer) = stream.into_split();
                    let mut reader = BufReader::new(read_half);
                    loop {
                        let frame =
                            tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
                                .await;
                        let Ok(frame) = frame else { return };
                        if frame.is_null() {
                            return;
                        }
                        let id = frame["id"].clone();
                        let method = frame["method"].as_str().unwrap_or("").to_string();
                        let body = reply_for(&method, &id);
                        if writer.write_all(&framed(&body)).await.is_err() {
                            return;
                        }
                        let _ = writer.flush().await;
                    }
                });
            }
        });
    }
}

fn id_json(id: &serde_json::Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "0".to_string())
}

/// Daemon N-1: the committed bare `{}` hello ack, `{}` for everything else.
fn n_minus_1(method: &str, id: &serde_json::Value) -> String {
    let result = if method == methods::AUTH_HELLO {
        fixture("n_minus_1_daemon_hello_ack")
    } else {
        "{}".to_string()
    };
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
        id_json(id)
    )
}

/// Daemon N+1: refuses hello the way `rpc/auth.rs::incompatible` does, with
/// what it speaks in `data`.
fn n_plus_1(method: &str, id: &serde_json::Value) -> String {
    if method == methods::AUTH_HELLO {
        format!(
            r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{},"message":"daemon protocol 5-6 cannot serve a client speaking 1-1; restart from the newer binary","data":{{"protocol":{{"min":5,"max":6}},"capabilities":[],"daemon_version":"9.9.9"}}}}}}"#,
            id_json(id),
            ainb_hangar_proto::protocol::PROTOCOL_INCOMPATIBLE
        )
    } else {
        format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{}}}}"#, id_json(id))
    }
}

async fn wait_for(
    state: &mut watch::Receiver<SidecarState>,
    what: &str,
    matches: impl Fn(&SidecarState) -> bool,
) -> SidecarState {
    let deadline = Duration::from_secs(20);
    tokio::time::timeout(deadline, async {
        loop {
            if matches(&state.borrow()) {
                return state.borrow().clone();
            }
            state.changed().await.expect("supervisor alive");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("not {what} within {deadline:?}: {:?}", *state.borrow()))
}

/// Leg 8: a daemon that predates the negotiation is attached to.
#[tokio::test(flavor = "multi_thread")]
async fn the_sidecar_attaches_to_daemon_n_minus_1() {
    let world = World::new();
    let (bin, marker) = world.recording_daemon_bin();
    let config = world.config(bin);
    world.serve(n_minus_1);
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let connected = wait_for(&mut state, "connected", |s| {
        matches!(s, SidecarState::Connected { .. })
    })
    .await;
    let SidecarState::Connected { spawned, .. } = connected else {
        unreachable!("matched connected");
    };
    assert!(
        !spawned,
        "a daemon that answered is attached to, not spawned"
    );
    assert!(
        !marker.exists(),
        "the bundled daemon was started against a live one"
    );
}

/// Leg 9, as the supervisor behaves TODAY, pinned before the fix so the fix
/// is a visible diff: a daemon that refuses this build's range is read as "no
/// daemon", the bundled daemon is spawned against it, and the supervisor ends
/// degraded with the wrong sentence. The next commit turns this around.
#[tokio::test(flavor = "multi_thread")]
async fn today_a_refusing_daemon_is_answered_by_a_spawn_and_a_degraded_state() {
    let world = World::new();
    let (bin, marker) = world.recording_daemon_bin();
    let config = world.config(bin);
    world.serve(n_plus_1);
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let degraded = wait_for(&mut state, "degraded", |s| {
        matches!(s, SidecarState::Degraded { .. })
    })
    .await;
    let SidecarState::Degraded { error, .. } = degraded else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("no daemon answered"), "{error}");
    assert!(marker.exists(), "today the refusal is answered by a spawn");
}
