//! The two-direction skew harness (spec D17, critique amendment 21).
//!
//! ```text
//!                    daemon N (this build)      daemon N-1 (committed frames)
//!  hangar-client         leg 1                        leg 2
//!  ainb-web              leg 3                        leg 4
//!  Swift fixture         leg 5                        leg 6
//!  local leg:  hangar.sock and hangar-v<N>.sock both reach daemon N (leg 7)
//! ```
//!
//! Both directions, because a version skew has two failure modes and only one of
//! them is obvious. A new daemon refusing an old client is loud. An old daemon
//! whose bare `{}` ack a new client cannot decode is silent, and it is the one
//! that strands an app-store phone that cannot be force-upgraded.
//!
//! "daemon N-1" is a fixture, not an old binary, and deliberately so: pinning a
//! released tag would test whatever that tag happened to do, whereas the frames
//! in `tests/fixtures/skew_frames.json` are the CONTRACT: the exact bytes a
//! pre-W0-wire daemon answers. A change to either side shows up as a diff in
//! that file rather than as a green test against a moved goalpost.
//!
//! The Swift leg is the same fixture, driven as raw frames. The Swift app's own
//! half runs on the macOS lane (`CanonicalFixtureTests`), and
//! [`the_swift_fixture_matches_the_swift_test`] is what stops the two copies
//! from drifting apart.

use std::path::Path;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::methods;
use ainb_hangar_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// The committed frames. `include_str!` rather than a runtime read so a missing
/// fixture is a compile error, not a skipped leg.
const FIXTURES: &str = include_str!("fixtures/skew_frames.json");

/// The Swift client's own copy of the same frames.
const SWIFT_TESTS: &str = include_str!(
    "../../../../apps/ainb-fleet-macos/Tests/FleetRPCTests/CanonicalFixtureTests.swift"
);

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
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.expect("read header");
        assert!(n > 0, "connection closed while awaiting a frame");
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let mut body = vec![0u8; len.expect("Content-Length header")];
            reader.read_exact(&mut body).await.expect("read body");
            return serde_json::from_slice(&body).expect("frame is JSON");
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                len = value.trim().parse().ok();
            }
        }
    }
}

// ── daemon N: the real listener ─────────────────────────────────────────────

async fn start_daemon_n(dir: &Path) -> (std::path::PathBuf, String) {
    let store = Store::open_in(dir).await.expect("store");
    rpc::auth::ensure_socket_token(store.pool(), dir).await.expect("socket token");
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(dir))
        .expect("token file")
        .trim()
        .to_string();
    let socket = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket).expect("bind");
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    let broker = EventBroker::new();
    let pool = store.pool().clone();
    // The store must outlive the server task.
    std::mem::forget(store);
    tokio::spawn(rpc::serve(listener, pool, health, broker));
    (socket, token)
}

// ── daemon N-1: the committed contract, served ──────────────────────────────

/// A pre-W0-wire daemon: it knows `{ token }`, answers `{}`, and has never
/// heard of a protocol range. Every byte it writes comes from the fixture file.
fn start_daemon_n_minus_1(dir: &Path) -> (std::path::PathBuf, String) {
    let socket = dir.join("hangar.sock");
    let token = "mdt_n_minus_one_fixture".to_string();
    let listener = UnixListener::bind(&socket).expect("bind N-1");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read_half, mut writer) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                loop {
                    let frame =
                        tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader)).await;
                    let Ok(frame) = frame else { return };
                    let id = frame["id"].clone();
                    let result = match frame["method"].as_str() {
                        // The whole point: an old daemon ignores every member it
                        // does not know and acks with a bare object.
                        Some(m) if m == methods::AUTH_HELLO => {
                            fixture("n_minus_1_daemon_hello_ack")
                        }
                        Some(m) if m == methods::ATTENTION_LIST => {
                            fixture("n_minus_1_daemon_attention_list_result")
                        }
                        _ => "{}".to_string(),
                    };
                    let body = format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#,
                        id = serde_json::to_string(&id).unwrap_or_else(|_| "0".to_string()),
                    );
                    if writer.write_all(&framed(&body)).await.is_err() {
                        return;
                    }
                    let _ = writer.flush().await;
                }
            });
        }
    });
    (socket, token)
}

// ── the legs ────────────────────────────────────────────────────────────────

/// Legs 1 and 2: the real `hangar-client` against both daemons.
#[tokio::test]
async fn hangar_client_talks_to_both_daemon_versions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;
    let client = ainb_hangar_client::DaemonClient::with_parts(socket, token);
    let rows = client.attention_list_fleet().await.expect("hangar-client must reach daemon N");
    assert!(rows.is_empty(), "a fresh store has no attention rows");

    let old = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n_minus_1(old.path());
    let client = ainb_hangar_client::DaemonClient::with_parts(socket, token);
    let rows = client
        .attention_list_fleet()
        .await
        .expect("hangar-client must survive a daemon that answers a bare {} to hello");
    assert!(rows.is_empty());
}

/// Legs 3 and 4: the real `ainb-web` client against both daemons.
#[tokio::test]
async fn ainb_web_talks_to_both_daemon_versions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;
    let client = ainb_web::daemon::DaemonClient::with_parts(socket, token);
    let rows = client.attention_list_fleet().await.expect("ainb-web must reach daemon N");
    assert!(rows.is_empty());

    let old = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n_minus_1(old.path());
    let client = ainb_web::daemon::DaemonClient::with_parts(socket, token);
    let rows = client
        .attention_list_fleet()
        .await
        .expect("ainb-web must survive a bare {} hello ack");
    assert!(rows.is_empty());
}

/// Leg 5: the Swift client's exact hello frame, against daemon N.
///
/// Driven as raw bytes because the Swift app cannot run on this lane. The frame
/// is the committed one the macOS lane asserts its encoder produces, so this
/// tests the daemon against what Swift really sends, not against a Rust
/// approximation of it.
#[tokio::test]
async fn the_swift_client_frame_is_accepted_by_daemon_n() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let params = fixture("swift_client_hello_params").replace("<TOKEN>", &token);
    let hello = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{params}}}"#,
        methods::AUTH_HELLO
    );
    writer.write_all(&framed(&hello)).await.expect("write hello");
    writer.flush().await.expect("flush");

    let ack = read_frame(&mut reader).await;
    assert!(
        ack["error"].is_null(),
        "Swift's hello frame was refused: {ack}"
    );
    // Everything the Swift decoder reads must be there, and readable.
    assert_eq!(ack["result"]["selected"], 1, "{ack}");
    assert_eq!(ack["result"]["protocol"]["min"], 1, "{ack}");
    assert_eq!(ack["result"]["protocol"]["max"], 1, "{ack}");
    let capabilities = ack["result"]["capabilities"]
        .as_array()
        .unwrap_or_else(|| panic!("no capability catalogue in the ack: {ack}"));
    for declared in [
        ainb_hangar_proto::protocol::CAP_AUTH_HELLO_NEGOTIATED,
        ainb_hangar_proto::protocol::CAP_MUTATION_OP_ID,
        ainb_hangar_proto::protocol::CAP_MUTATION_RECEIPT,
        ainb_hangar_proto::protocol::CAP_SOCKET_VERSIONED,
    ] {
        assert!(
            capabilities.iter().any(|c| c == declared),
            "the daemon must advertise {declared}: {ack}"
        );
    }
}

/// Leg 6: the N-1 client frame (a bare `{ token }`) against daemon N.
///
/// This is the leg an app-store phone lives on. A daemon that required the new
/// members would refuse a client that cannot be upgraded.
#[tokio::test]
async fn the_n_minus_1_client_frame_is_accepted_by_daemon_n() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let params = fixture("n_minus_1_client_hello_params").replace("<TOKEN>", &token);
    let hello = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{params}}}"#,
        methods::AUTH_HELLO
    );
    writer.write_all(&framed(&hello)).await.expect("write hello");
    writer.flush().await.expect("flush");
    let ack = read_frame(&mut reader).await;
    assert!(
        ack["error"].is_null(),
        "a bare {{token}} hello was refused: {ack}"
    );
    assert_eq!(
        ack["result"]["selected"], 1,
        "a client that declared nothing negotiates version 1: {ack}"
    );

    // And it can still call: authentication was not a casualty of negotiation.
    let list = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"{}","params":{{"fleet":true}}}}"#,
        methods::ATTENTION_LIST
    );
    writer.write_all(&framed(&list)).await.expect("write list");
    writer.flush().await.expect("flush");
    let reply = read_frame(&mut reader).await;
    assert!(reply["error"].is_null(), "{reply}");
}

/// A client whose range cannot meet ours is refused with the PROTOCOL code, not
/// an auth failure: the remedy is a different binary, never a different token.
#[tokio::test]
async fn a_client_from_the_future_is_refused_as_incompatible() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let hello = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"token":"{token}","protocol":{{"min":99,"max":100}}}}}}"#,
        methods::AUTH_HELLO
    );
    writer.write_all(&framed(&hello)).await.expect("write hello");
    writer.flush().await.expect("flush");
    let ack = read_frame(&mut reader).await;
    assert_eq!(
        ack["error"]["code"],
        ainb_hangar_proto::protocol::PROTOCOL_INCOMPATIBLE,
        "{ack}"
    );
    assert_ne!(
        ack["error"]["code"],
        ainb_hangar_proto::auth::UNAUTHORIZED,
        "a version mismatch must never read as a credential problem"
    );
    // The refusal still SAYS what the daemon speaks, so a client can report it.
    assert_eq!(ack["error"]["data"]["protocol"]["max"], 1, "{ack}");
}

/// Leg 7, the local leg (amendment 21): `hangar.sock` and `hangar-v<N>.sock`
/// are two names for one listener, so a sidecar daemon and an installed one
/// never race two sockets.
#[tokio::test]
async fn the_versioned_alias_and_the_plain_socket_are_one_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;

    let alias =
        rpc::versioned_socket_path_in(dir.path(), ainb_hangar_proto::protocol::PROTOCOL_VERSION);
    assert!(
        alias.exists(),
        "the daemon must publish {}",
        alias.display()
    );
    assert_eq!(
        std::fs::canonicalize(&alias).unwrap(),
        std::fs::canonicalize(&socket).unwrap(),
        "the alias must be the SAME inode, not a second socket"
    );

    // A client that knows the version dials the alias and is served identically.
    let client = ainb_hangar_client::DaemonClient::with_parts(alias, token);
    assert!(client.attention_list_fleet().await.unwrap().is_empty());
}

/// The versioned alias is PREFERRED, not trusted.
///
/// The daemon creates it best-effort and never re-checks it, and the directory
/// is the operator's own home, so every same-uid process, including an agent
/// this daemon spawned, can unlink it and listen on the path instead. The first
/// frame a client sends is the daemon bearer token, so preferring that path
/// blind would hand the token to whoever got there first.
#[tokio::test]
async fn a_squatted_versioned_alias_is_not_dialled() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, token) = start_daemon_n(dir.path()).await;

    let alias =
        rpc::versioned_socket_path_in(dir.path(), ainb_hangar_proto::protocol::PROTOCOL_VERSION);
    assert!(
        alias.exists(),
        "the daemon must publish {}",
        alias.display()
    );
    assert_eq!(
        ainb_hangar_client::socket_path_in(dir.path()),
        alias,
        "the daemon's own alias is the preferred path"
    );

    // Somebody else takes the path and listens on it.
    std::fs::remove_file(&alias).unwrap();
    let _squatter = std::os::unix::net::UnixListener::bind(&alias).unwrap();
    assert_eq!(
        ainb_hangar_client::socket_path_in(dir.path()),
        socket,
        "a squatted alias must not be dialled: the first frame is the token"
    );

    // And the client still reaches the real daemon over the plain path.
    let client = ainb_hangar_client::DaemonClient::with_parts(
        ainb_hangar_client::socket_path_in(dir.path()),
        token,
    );
    assert!(client.attention_list_fleet().await.unwrap().is_empty());
}

/// The anti-drift gate for the Swift leg.
///
/// The Rust harness and the Swift test suite each hold a copy of the same
/// frames, because neither lane can run the other's code. Two copies of a
/// contract are one drift away from testing nothing, so the copies are compared
/// here, as text.
#[test]
fn the_swift_fixture_matches_the_swift_test() {
    for name in ["swift_client_hello_params", "swift_negotiated_ack_sample"] {
        // `<TOKEN>` is the only templated part of a frame; the Swift suite
        // spells it `fixture-token` because its own assertion is offline.
        let frame = fixture(name).replace("<TOKEN>", "fixture-token");
        // The Swift source escapes nothing inside a raw string literal, so the
        // frame appears verbatim.
        assert!(
            SWIFT_TESTS.contains(&frame),
            "{name} has drifted from CanonicalFixtureTests.swift.\n\
             The Rust harness expects this frame verbatim:\n{frame}"
        );
    }
    assert!(
        SWIFT_TESTS.contains("testLegacyHelloFramesStillDecode"),
        "the Swift side of the N-1 legs is missing"
    );
}
