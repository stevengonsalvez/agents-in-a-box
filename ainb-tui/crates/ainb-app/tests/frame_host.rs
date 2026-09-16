#![allow(missing_docs)]

// ABOUTME: #1066 part 2. Every frame this host sends, and every fleet row
// inside one, names the `host_id` the daemon gave in `auth/hello`, and `local`
// only while no daemon has named one.
//
// Its own test binary: the observed host id is process-wide (one home per
// process), so a sibling test completing a different hello would race it.

use std::io::Write as _;

use ainb_app::wire::frame::{HostId, Mirror, Subscription};
use ainb_app::wire::shape;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const HOST: &str = "01K5A0000000000000000AAAAA";

/// A daemon that answers one `auth/hello` naming `HOST`, then hangs up. The
/// listener is bound by the caller, so the socket exists before the client
/// dials it.
async fn fake_daemon(listener: UnixListener) {
    let (stream, _) = listener.accept().await.expect("accept the client");
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Content-Length framing: read the header, then exactly that many bytes.
    let mut header = String::new();
    let mut len = 0usize;
    loop {
        header.clear();
        reader.read_line(&mut header).await.expect("read a header line");
        if header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.trim().strip_prefix("Content-Length:") {
            len = value.trim().parse().expect("a numeric Content-Length");
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.expect("read the hello body");
    let hello: serde_json::Value = serde_json::from_slice(&body).expect("the hello parses");
    assert_eq!(hello["method"], "auth/hello");
    assert!(
        hello["params"].get("host_id").is_none(),
        "the client must never assert a host id"
    );

    let reply = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "host_id": HOST },
    })
    .to_string();
    let mut frame = Vec::new();
    write!(frame, "Content-Length: {}\r\n\r\n", reply.len()).expect("write the header");
    frame.extend_from_slice(reply.as_bytes());
    writer.write_all(&frame).await.expect("write the reply");
    writer.flush().await.expect("flush the reply");
}

#[tokio::test]
async fn frames_and_fleet_rows_name_the_host_the_daemon_gave_in_hello() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("hangar.sock");

    // Before any hello: the host is `local`, which is what a surface that has
    // not reached a daemon must keep saying.
    assert_eq!(HostId::daemon(), HostId::local());
    let state = shape::sample_state(&mut shape::PlainSeed);
    let before = Mirror::for_daemon(Subscription::all()).batch(&state);
    assert!(
        before.frames.iter().all(|frame| frame.host_id == HostId::local()),
        "frames before a hello must name local"
    );

    let listener = UnixListener::bind(&socket).expect("bind the fake hangar socket");
    let daemon = tokio::spawn(fake_daemon(listener));
    ainb_hangar_client::DaemonClient::with_parts(socket, "test-token".to_string())
        .hello()
        .await
        .expect("the hello completes");
    daemon.await.expect("the fake daemon served one hello");

    // After it: every frame, and every fleet row inside one, names the ULID.
    let batch = Mirror::for_daemon(Subscription::all()).batch(&state);
    assert!(
        !batch.frames.is_empty(),
        "the sample state frames something"
    );
    for frame in &batch.frames {
        assert_eq!(
            frame.host_id,
            HostId::new(HOST),
            "section {}",
            frame.section
        );
    }
    let fleet = batch
        .frames
        .iter()
        .find(|frame| frame.section == "fleet")
        .expect("the fleet section is framed");
    let body = serde_json::to_string(fleet.body()).expect("the body serialises");
    assert!(
        body.contains(HOST),
        "a fleet row must name the daemon: {body}"
    );
    assert!(
        !body.contains("\"host_id\":\"local\""),
        "no row may still say local: {body}"
    );
}
