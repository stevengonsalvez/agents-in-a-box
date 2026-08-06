//! `ainb fleet msg` / `acp` / `transcript` contract tests against a FIXTURE
//! daemon.
//!
//! The fixture is a real Unix socket speaking the daemon's framing and the
//! frozen `fleet/message_*` shapes, so these tests pin the CLI's half of the
//! contract without a live hangar: JSON on stdout, JSON errors on stderr with
//! a `retryable` flag, the semantic exit codes (0 ok, 1 bad input, 2
//! daemon/network, 5 idempotency conflict), stdin `-`, and NDJSON streaming
//! from `follow`.
//!
//! DISCLOSURE: the daemon here is a fixture, not the real binary. The daemon
//! half of the same contract is covered by `rpc_message_bus.rs` in
//! `ainb-hangar-daemon`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const TOKEN: &str = "mdt_fixture";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// How the fixture answers `fleet/message_send`.
#[derive(Clone, Copy)]
enum SendBehaviour {
    Ok,
    IdempotencyConflict,
}

/// Lay down the token file the client reads, and return the socket path.
fn prepare_home(home: &Path) -> PathBuf {
    std::fs::create_dir_all(home.join("hangar")).expect("create hangar dir");
    std::fs::write(home.join("hangar").join("daemon.token"), TOKEN).expect("write token");
    home.join("hangar.sock")
}

/// Serve one connection with the daemon's Content-Length framing.
async fn serve_connection(stream: UnixStream, behaviour: SendBehaviour) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let Some(request) = read_frame(&mut reader).await else {
            return;
        };
        let id = request.get("id").cloned().unwrap_or(json!(0));
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let response = match method.as_str() {
            "auth/hello" => {
                assert_eq!(
                    request["params"]["token"], TOKEN,
                    "the CLI must present the token"
                );
                json!({ "jsonrpc": "2.0", "id": id, "result": {} })
            }
            "fleet/message_send" => match behaviour {
                SendBehaviour::Ok => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "message_id": "msg-01",
                        "deliveries": [
                            { "session_key": request["params"]["targets"][0], "state": "DELIVERED" }
                        ],
                    },
                }),
                SendBehaviour::IdempotencyConflict => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32602,
                        "message": "request_id was reused for a different message",
                    },
                }),
            },
            "fleet/message_list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "messages": [{
                        "id": "msg-01",
                        "scope_key": "session:claude:one",
                        "sender": "operator",
                        "kind": "user",
                        "body": "hello",
                        "created_at": 1,
                    }],
                    "next_after_id": "msg-01",
                },
            }),
            "fleet/acp_session_create" => {
                // The daemon validates `provider` against the adapter registry,
                // so the fixture answers the refusal for an unknown one: the
                // CLI's job is to turn that -32602 into exit 1 with a JSON
                // error, not to second-guess the registry.
                if request["params"]["provider"] == "nope-acp" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": "unknown ACP provider \"nope-acp\"" },
                    })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "session_key": "acp:01J0FIXTURE",
                            "scope_key": "session:acp:01J0FIXTURE",
                        },
                    })
                }
            }
            "fleet/transcript_list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "chunks": [{
                        "ingest_order": 4,
                        "event_id": "evt-4",
                        "session_key": request["params"]["session_key"],
                        "event_type": "acp.message",
                        // Adapter-authored text is NOT trusted: an adapter that
                        // emits ANSI plus a newline must not repaint the
                        // operator's terminal or forge a second transcript line.
                        "payload": { "text": "hello\u{1b}[2J\nforged 99 acp.message {}" },
                        "observed_at": 1,
                    }],
                    "next_after_order": 4,
                },
            }),
            "fleet/transcript_subscribe" => {
                write_frame(
                    &mut writer,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": { "head_order": 4 } }),
                )
                .await;
                for order in 5..7 {
                    write_frame(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "fleet/transcript_event",
                            "params": { "chunk": {
                                "ingest_order": order,
                                "event_id": format!("evt-{order}"),
                                "session_key": "acp:01J0FIXTURE",
                                "event_type": "acp.message",
                                "payload": { "text": format!("live {order}") },
                                "observed_at": order,
                            }},
                        }),
                    )
                    .await;
                }
                return; // closing the stream ends `--follow`
            }
            "fleet/message_subscribe" => {
                write_frame(
                    &mut writer,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": { "head_id": "msg-01" } }),
                )
                .await;
                for index in 2..4 {
                    write_frame(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "fleet/message_event",
                            "params": { "message": {
                                "id": format!("msg-{index:02}"),
                                "scope_key": "session:claude:one",
                                "sender": "operator",
                                "kind": "user",
                                "body": format!("live {index}"),
                                "created_at": index,
                            }},
                        }),
                    )
                    .await;
                }
                return; // closing the stream ends `follow`
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method: {other}") },
            }),
        };
        write_frame(&mut writer, &response).await;
    }
}

async fn write_frame(writer: &mut (impl AsyncWriteExt + Unpin), value: &Value) {
    let body = serde_json::to_vec(value).expect("serialize frame");
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await.expect("write frame");
    writer.flush().await.expect("flush frame");
}

async fn read_frame(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches("\r\n");
        if line.is_empty() {
            let mut body = vec![0_u8; content_length?];
            reader.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
}

/// Run the fixture daemon for the duration of one CLI invocation.
async fn with_fixture<F>(
    behaviour: SendBehaviour,
    args: &[&str],
    stdin: Option<&str>,
    assertions: F,
) where
    F: FnOnce(std::process::Output),
{
    let home = tempfile::tempdir().expect("temp home");
    let socket = prepare_home(home.path());
    let listener = UnixListener::bind(&socket).expect("bind fixture socket");
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move { serve_connection(stream, behaviour).await });
        }
    });

    let output = run_cli(home.path(), args, stdin).await;
    server.abort();
    assertions(output);
}

async fn run_cli(home: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let mut command = tokio::process::Command::new(ainb_bin());
    command
        .env("AINB_HANGAR_HOME", home)
        .env("AINB_HOME", home)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn ainb");
    if let Some(stdin) = stdin {
        use tokio::io::AsyncWriteExt as _;
        let mut pipe = child.stdin.take().expect("stdin pipe");
        // A CLI that BOUNDS its stdin read stops reading before we stop
        // writing, so a broken pipe here is the bound working as intended.
        if let Err(error) = pipe.write_all(stdin.as_bytes()).await {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe,
                "unexpected stdin write failure: {error}"
            );
        }
        drop(pipe);
    }
    tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("ainb finished within 30s")
        .expect("collect ainb output")
}

fn stderr_error(output: &std::process::Output) -> Value {
    let text = String::from_utf8_lossy(&output.stderr);
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error on stderr: {text}"));
    serde_json::from_str(line).expect("stderr error is JSON")
}

/// Happy path: exit 0 and the daemon's result verbatim on stdout.
#[tokio::test]
async fn send_prints_json_on_stdout_and_exits_zero() {
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "send",
            "--target",
            "claude:one",
            "--text",
            "hello",
            "--request-id",
            "req-cli",
        ],
        None,
        |output| {
            assert_eq!(
                output.status.code(),
                Some(0),
                "stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let parsed: Value =
                serde_json::from_slice(&output.stdout).expect("stdout is one JSON document");
            assert_eq!(parsed["message_id"], "msg-01");
            assert_eq!(parsed["deliveries"][0]["state"], "DELIVERED");
        },
    )
    .await;
}

/// `-` reads the body from stdin, so a long prompt never has to fit in argv.
#[tokio::test]
async fn send_reads_the_body_from_stdin() {
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "send",
            "--target",
            "claude:one",
            "--text",
            "-",
        ],
        Some("piped body"),
        |output| {
            assert_eq!(output.status.code(), Some(0));
            let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout JSON");
            assert_eq!(parsed["message_id"], "msg-01");
        },
    )
    .await;
}

/// A reused `request_id` with different content maps to exit 5 and a
/// non-retryable error naming the command that shows what already landed.
#[tokio::test]
async fn idempotency_conflict_exits_five_with_a_non_retryable_error() {
    with_fixture(
        SendBehaviour::IdempotencyConflict,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "send",
            "--target",
            "claude:one",
            "--text",
            "hello",
            "--request-id",
            "req-cli",
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(5));
            let error = stderr_error(&output);
            assert_eq!(error["error"]["kind"], "idempotency_conflict");
            assert_eq!(error["error"]["retryable"], false);
            assert_eq!(error["error"]["exit_code"], 5);
            assert!(
                error["error"]["next"].as_str().is_some_and(|next| next.contains("msg list")),
                "the error names the follow-up command: {error}"
            );
        },
    )
    .await;
}

/// Bad input is exit 1 and never reaches the daemon.
#[tokio::test]
async fn empty_text_is_bad_input() {
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "send",
            "--target",
            "claude:one",
            "--text",
            "   ",
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(1));
            let error = stderr_error(&output);
            assert_eq!(error["error"]["kind"], "bad_input");
            assert_eq!(error["error"]["retryable"], false);
        },
    )
    .await;
}

/// No daemon on the socket is exit 2, marked retryable, and points at the
/// daemon status command.
#[tokio::test]
async fn a_missing_daemon_is_a_retryable_exit_two() {
    let home = tempfile::tempdir().expect("temp home");
    prepare_home(home.path()); // token but no listener
    let output = run_cli(
        home.path(),
        &["--format", "json", "fleet", "msg", "list", "--limit", "5"],
        None,
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    let error = stderr_error(&output);
    assert_eq!(error["error"]["kind"], "daemon");
    assert_eq!(error["error"]["retryable"], true);
    assert!(error["error"]["next"].as_str().is_some_and(|next| next.contains("daemon")));
}

/// `list` renders the daemon's page as JSON on stdout.
#[tokio::test]
async fn list_prints_the_page_as_json() {
    with_fixture(
        SendBehaviour::Ok,
        &["--format", "json", "fleet", "msg", "list", "--limit", "5"],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(0));
            let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout JSON");
            assert_eq!(parsed["messages"][0]["id"], "msg-01");
            assert_eq!(parsed["next_after_id"], "msg-01");
        },
    )
    .await;
}

/// The CLI enforces the daemon's body ceiling itself, so a piped file that can
/// never be accepted is refused without buffering all of it or making the round
/// trip.
#[tokio::test]
async fn a_stdin_body_past_the_ceiling_is_bad_input() {
    use ainb_hangar_proto::fleet::FLEET_MESSAGE_BODY_MAX;

    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "send",
            "--target",
            "claude:one",
            "--text",
            "-",
        ],
        Some(&"x".repeat(FLEET_MESSAGE_BODY_MAX + 4096)),
        |output| {
            assert_eq!(output.status.code(), Some(1));
            let error = stderr_error(&output);
            assert_eq!(error["error"]["kind"], "bad_input");
            assert!(
                error["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("at most")),
                "the refusal names the ceiling: {error}"
            );
        },
    )
    .await;
}

/// `--origin` and `--scope` are different cuts of the log, so asking for both
/// is a mistake worth naming rather than a silent pick of one.
#[tokio::test]
async fn origin_and_scope_together_is_bad_input() {
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "msg",
            "list",
            "--origin",
            "msg-01",
            "--scope",
            "session:a",
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(1));
            let error = stderr_error(&output);
            assert_eq!(error["error"]["kind"], "bad_input");
            assert!(
                error["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("mutually exclusive")),
                "the refusal names the conflict: {error}"
            );
        },
    )
    .await;
}

/// There is no tabular rendering of a chat log worth having. `--format csv`
/// used to silently render the TEXT shape, which a script consuming csv had no
/// way to detect.
#[tokio::test]
async fn csv_and_markdown_are_refused_rather_than_rendered_as_text() {
    for format in ["csv", "markdown"] {
        with_fixture(
            SendBehaviour::Ok,
            &["--format", format, "fleet", "msg", "list", "--limit", "5"],
            None,
            |output| {
                assert_eq!(
                    output.status.code(),
                    Some(1),
                    "--format {format} must be refused, not rendered"
                );
                let error = stderr_error(&output);
                assert_eq!(error["error"]["kind"], "bad_input");
                assert!(output.stdout.is_empty(), "nothing may reach stdout");
            },
        )
        .await;
    }
}

/// `follow` streams NDJSON: the acknowledgement first, then one committed
/// message per line, live, before the stream ends.
#[tokio::test]
async fn follow_streams_ndjson() {
    with_fixture(
        SendBehaviour::Ok,
        &["--format", "json", "fleet", "msg", "follow"],
        None,
        |output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<Value> = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("each line is one JSON document"))
                .collect();
            assert_eq!(lines.len(), 3, "ack plus two live messages: {stdout}");
            assert_eq!(lines[0]["head_id"], "msg-01");
            assert_eq!(lines[1]["id"], "msg-02");
            assert_eq!(lines[2]["id"], "msg-03");
            // The daemon went away mid-stream, which is the retryable case.
            assert_eq!(output.status.code(), Some(2));
        },
    )
    .await;
}

// ------------------------------------------------- the Phase 5 verbs (ACP)

/// `fleet acp create` prints the daemon's result verbatim and exits 0.
///
/// The `--cwd` is resolved to an ABSOLUTE path before it leaves the CLI,
/// because the daemon's working directory is not the operator's.
#[tokio::test]
async fn acp_create_prints_json_and_sends_an_absolute_cwd() {
    let cwd = tempfile::tempdir().expect("cwd");
    let relative = cwd.path().join("nested");
    std::fs::create_dir_all(&relative).expect("nested dir");
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "acp",
            "create",
            "--provider",
            "claude-agent-acp",
            "--cwd",
            relative.to_str().expect("utf8"),
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let result: Value = serde_json::from_slice(&output.stdout).expect("json on stdout");
            assert_eq!(result["session_key"], "acp:01J0FIXTURE");
            assert_eq!(result["scope_key"], "session:acp:01J0FIXTURE");
        },
    )
    .await;
}

/// An unknown adapter is the daemon's -32602, rendered as exit 1 plus the JSON
/// error shape every chat-bus verb shares.
#[tokio::test]
async fn acp_create_with_an_unknown_provider_exits_bad_input() {
    let cwd = tempfile::tempdir().expect("cwd");
    with_fixture(
        SendBehaviour::Ok,
        &[
            "fleet",
            "acp",
            "create",
            "--provider",
            "nope-acp",
            "--cwd",
            cwd.path().to_str().expect("utf8"),
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let error = stderr_error(&output);
            assert_eq!(error["error"]["kind"], "bad_input");
            assert_eq!(error["error"]["retryable"], false);
            assert!(output.stdout.is_empty(), "nothing may reach stdout");
        },
    )
    .await;
}

/// `fleet transcript` pages the execution log; `--follow` streams NDJSON,
/// which is the CLI half of I12's live leg.
#[tokio::test]
async fn transcript_lists_and_follows() {
    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "transcript",
            "acp:01J0FIXTURE",
            "--limit",
            "10",
        ],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let result: Value = serde_json::from_slice(&output.stdout).expect("json on stdout");
            assert_eq!(result["chunks"][0]["event_type"], "acp.message");
            assert_eq!(result["next_after_order"], 4);
        },
    )
    .await;

    with_fixture(
        SendBehaviour::Ok,
        &[
            "--format",
            "json",
            "fleet",
            "transcript",
            "acp:01J0FIXTURE",
            "--follow",
        ],
        None,
        |output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<Value> = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("each line is one JSON document"))
                .collect();
            assert_eq!(lines.len(), 3, "ack plus two live chunks: {stdout}");
            assert_eq!(lines[0]["head_order"], 4);
            assert_eq!(lines[1]["ingest_order"], 5);
            assert_eq!(lines[2]["ingest_order"], 6);
            // The fixture went away mid-stream: the retryable daemon case.
            assert_eq!(output.status.code(), Some(2));
        },
    )
    .await;
}

/// The text renderer is the LOSSY, terminal-safe surface: one chunk stays one
/// line and no control character survives, so an adapter cannot repaint the
/// screen or forge a transcript entry the operator would read as real.
#[tokio::test]
async fn transcript_text_output_escapes_control_characters() {
    with_fixture(
        SendBehaviour::Ok,
        &["fleet", "transcript", "acp:01J0FIXTURE"],
        None,
        |output| {
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
            assert_eq!(lines.len(), 1, "one chunk stays one line: {stdout:?}");
            assert!(
                !lines[0].chars().any(char::is_control),
                "no control character may reach the terminal: {:?}",
                lines[0]
            );
            // The ESC and the newline survive only as VISIBLE text. The payload
            // is JSON-serialized before escaping, so they arrive already
            // neutralised as escaped u001b / n and `escape_control` then escapes
            // the backslash; either way nothing actionable reaches the terminal.
            assert!(
                lines[0].contains("u001b[2J"),
                "ESC is inert text: {stdout:?}"
            );
            assert!(lines[0].contains("nforged"), "newline is inert: {stdout:?}");
            assert!(
                lines[0].starts_with("4 acp.message "),
                "ingest_order and event_type lead the line: {stdout:?}"
            );
        },
    )
    .await;
}

/// The Phase 5 verbs share `msg`'s refusal of tabular formats: there is no
/// honest csv/markdown rendering of a transcript or a session-create receipt,
/// so they are refused rather than silently emitted as single-column prose.
#[tokio::test]
async fn acp_and_transcript_refuse_csv_and_markdown() {
    for format in ["csv", "markdown"] {
        for verb in [
            vec!["fleet", "transcript", "acp:01J0FIXTURE"],
            vec![
                "fleet",
                "acp",
                "create",
                "--provider",
                "claude-agent-acp",
                "--cwd",
                ".",
            ],
        ] {
            let mut args = vec!["--format", format];
            args.extend(verb.iter().copied());
            with_fixture(SendBehaviour::Ok, &args, None, |output| {
                assert_eq!(
                    output.status.code(),
                    Some(1),
                    "--format {format} on {verb:?} must be refused, not rendered"
                );
                let error = stderr_error(&output);
                assert_eq!(error["error"]["kind"], "bad_input");
                assert_eq!(error["error"]["retryable"], false);
                assert!(output.stdout.is_empty(), "nothing may reach stdout");
            })
            .await;
        }
    }
}

/// A down daemon is the RETRYABLE case for the Phase 5 verbs too: exit 2 with
/// `retryable: true`, not the exit-1 bad-input code a caller would give up on.
#[tokio::test]
async fn the_phase_five_verbs_exit_two_when_the_daemon_is_missing() {
    let home = tempfile::tempdir().expect("temp home");
    prepare_home(home.path()); // token but no listener
    let cwd = tempfile::tempdir().expect("cwd");
    let create = vec![
        "--format",
        "json",
        "fleet",
        "acp",
        "create",
        "--provider",
        "claude-agent-acp",
        "--cwd",
        cwd.path().to_str().expect("utf8"),
    ];
    let transcript = vec!["--format", "json", "fleet", "transcript", "acp:01J0FIXTURE"];
    for args in [create, transcript] {
        let output = run_cli(home.path(), &args, None).await;
        assert_eq!(output.status.code(), Some(2), "{args:?} -> {output:?}");
        let error = stderr_error(&output);
        assert_eq!(error["error"]["kind"], "daemon");
        assert_eq!(error["error"]["retryable"], true);
        assert!(output.stdout.is_empty(), "nothing may reach stdout");
    }
}
