//! Integration: the web surface's [`DaemonClient`] over a real framed
//! `UnixStream` (spec P8 / D18).
//!
//! Stands up a MINIMAL fake daemon socket server (Content-Length JSON-RPC, the
//! same framing the real daemon speaks) so the client's dial + mandatory
//! `auth/hello` first frame + `attention/list` / `attention/answer` round-trips
//! are proven end-to-end WITHOUT depending on the daemon crate. The fake asserts
//! the handshake contract (auth first, correct token) and returns canned proto
//! results the client must decode.

use std::path::PathBuf;

use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use ainb_web::daemon::{DaemonClient, DaemonError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const TOKEN: &str = "mdt_TESTSECRET";

/// Read one Content-Length-framed JSON object off the connection.
async fn read_frame(reader: &mut BufReader<&mut UnixStream>) -> Value {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0, "connection closed mid-frame");
        let t = line.trim_end_matches("\r\n");
        if t.is_empty() {
            let mut body = vec![0u8; len.expect("Content-Length header")];
            reader.read_exact(&mut body).await.unwrap();
            return serde_json::from_slice(&body).unwrap();
        }
        if let Some((name, v)) = t.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().ok();
            }
        }
    }
}

async fn write_frame(stream: &mut UnixStream, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    stream.write_all(&out).await.unwrap();
    stream.flush().await.unwrap();
}

/// Spawn a one-shot fake daemon that: (1) requires `auth/hello` with the right
/// token as its first frame, and (2) answers the single follow-up call with
/// `reply` (echoing the request id). If the token is wrong it answers the hello
/// with a JSON-RPC error and closes. Returns the socket path.
fn spawn_fake_daemon(reply: Value) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    // Keep the tempdir alive for the server's lifetime by leaking it — the test
    // process is short-lived and this avoids a race on early drop.
    let socket = dir.path().join("hangar.sock");
    let socket_ret = socket.clone();
    Box::leak(Box::new(dir));

    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello = {
            let mut reader = BufReader::new(&mut stream);
            read_frame(&mut reader).await
        };
        assert_eq!(hello["method"], "auth/hello", "first frame must be auth/hello");
        let hello_id = hello["id"].clone();
        if hello["params"]["token"] != json!(TOKEN) {
            write_frame(
                &mut stream,
                &json!({ "jsonrpc": "2.0", "id": hello_id,
                         "error": { "code": -32000, "message": "unauthorized" } }),
            )
            .await;
            return;
        }
        write_frame(
            &mut stream,
            &json!({ "jsonrpc": "2.0", "id": hello_id, "result": {} }),
        )
        .await;

        // The real call.
        let call = {
            let mut reader = BufReader::new(&mut stream);
            read_frame(&mut reader).await
        };
        let call_id = call["id"].clone();
        write_frame(
            &mut stream,
            &json!({ "jsonrpc": "2.0", "id": call_id, "result": reply }),
        )
        .await;
    });

    socket_ret
}

#[tokio::test]
async fn attention_list_round_trips_after_auth() {
    let reply = json!({
        "attention": [
            {
                "id": "att-1",
                "session_id": "s1",
                "cwd": "/work/x",
                "kind": "ask_user_question",
                "payload": "{\"question\":\"go?\"}",
                "created_at": 1000
            }
        ]
    });
    let socket = spawn_fake_daemon(reply);
    let client = DaemonClient::with_parts(socket, TOKEN.to_string());

    let rows = client.attention_list_fleet().await.expect("attention/list ok");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "att-1");
    assert_eq!(rows[0].kind, "ask_user_question");
    assert_eq!(rows[0].cwd, "/work/x");
}

#[tokio::test]
async fn answer_round_trips_and_decodes_outcome() {
    let reply = json!({ "outcome": "delivered", "via": "tmux (s1)" });
    let socket = spawn_fake_daemon(reply);
    let client = DaemonClient::with_parts(socket, TOKEN.to_string());

    let result = client
        .answer(AnswerParams {
            attention_id: "att-1".into(),
            answer: "2".into(),
            answered_by: "web".into(),
            is_answer: true,
        })
        .await
        .expect("attention/answer ok");
    match result {
        AnswerResult::Delivered { via } => assert_eq!(via, "tmux (s1)"),
        other => panic!("expected Delivered, got {other:?}"),
    }
}

#[tokio::test]
async fn wrong_token_is_refused_at_hello() {
    let socket = spawn_fake_daemon(json!({ "attention": [] }));
    // A client presenting the wrong token must surface the daemon's auth error,
    // never a decoded (empty) result.
    let client = DaemonClient::with_parts(socket, "mdt_WRONG".to_string());
    let err = client.attention_list_fleet().await.expect_err("wrong token must fail");
    match err {
        DaemonError::Rpc { code, .. } => assert_eq!(code, -32000),
        other => panic!("expected an Rpc auth error, got {other:?}"),
    }
}

#[tokio::test]
async fn unreachable_socket_degrades_to_error_not_panic() {
    // A socket path that does not exist (daemon down) is a clean error the
    // caller degrades on — never a panic.
    let client = DaemonClient::with_parts(PathBuf::from("/nonexistent/hangar.sock"), TOKEN.into());
    let err = client.attention_list_fleet().await.expect_err("down daemon must error");
    assert!(matches!(err, DaemonError::Connect { .. }));
}
