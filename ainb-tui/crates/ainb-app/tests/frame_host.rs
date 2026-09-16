#![allow(missing_docs)]

// ABOUTME: #1066 part 2. One mirror across the moment its daemon names a host:
// frames and fleet rows say `local` before the hello and the ULID after the
// surface re-pins, and a renderer that re-peers keeps every section, the
// static ones included.
//
// Its own test binary, so no other test shares the process-wide record of what
// each socket's daemon named.

use std::io::Write as _;

use ainb_app::app::versioned::SectionId;
use ainb_app::wire::frame::{HostId, Mirror, Subscription};
use ainb_app::wire::shape;
use ainb_app::wire::store::MirrorStore;
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
async fn one_mirror_crosses_the_hello_and_the_renderer_keeps_every_section() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("hangar.sock");
    let local = HostId::local();

    // Before any hello the surface pins `local`, and the renderer peers there.
    assert_eq!(HostId::of_daemon(&socket), local);
    let mut state = shape::sample_state(&mut shape::PlainSeed);
    let mut mirror = Mirror::new(HostId::of_daemon(&socket), Subscription::all());
    let mut store = MirrorStore::new(Subscription::all());
    let first = mirror.batch(&state);
    assert!(first.frames.iter().all(|frame| frame.host_id == local));
    store.apply_drain(&local, [first]);
    assert!(store.section(&local, SectionId::Config).is_some());

    // The daemon names its host.
    let listener = UnixListener::bind(&socket).expect("bind the fake hangar socket");
    let daemon = tokio::spawn(fake_daemon(listener));
    ainb_hangar_client::DaemonClient::with_parts(socket.clone(), "test-token".to_string())
        .hello()
        .await
        .expect("the hello completes");
    daemon.await.expect("the fake daemon served one hello");
    let ulid = HostId::of_daemon(&socket);
    assert_eq!(ulid, HostId::new(HOST));

    // A live mirror does not drift: until it is re-pinned, it still says local.
    state.shell.help_visible = !state.shell.help_visible;
    let unpinned = mirror.batch(&state);
    assert_eq!(unpinned.frames.len(), 1, "only the changed section");
    assert_eq!(unpinned.frames[0].host_id, local);
    store.apply_drain(&local, [unpinned]);

    // The surface re-pins, and tells the renderer first. A renderer still
    // peered at `local` would drop the whole batch, which is why it must hear
    // the new id before the batch arrives.
    assert!(mirror.set_host(ulid.clone()));
    state.shell.help_visible = !state.shell.help_visible;
    let pinned = mirror.batch(&state);
    let mut stale = MirrorStore::new(Subscription::all());
    stale.apply_drain(&local, [pinned.clone()]);
    assert_eq!(stale.section(&ulid, SectionId::Config), None);
    assert!(stale.frames_ignored() > 0, "a stale peer drops the batch");

    // Re-peered, the renderer holds every section under the ULID, static ones
    // included, and every fleet row names the ULID too.
    store.apply_drain(&ulid, [pinned]);
    for id in SectionId::ALL {
        let section = store
            .section(&ulid, id)
            .unwrap_or_else(|| panic!("{id:?} missing after the re-pin"));
        if id == SectionId::Fleet {
            let body = serde_json::to_string(&section.body).expect("the body serialises");
            assert!(body.contains(HOST), "fleet rows must name the ULID: {body}");
            assert!(
                !body.contains("\"host_id\":\"local\""),
                "no fleet row may still say local: {body}"
            );
        }
    }
}
