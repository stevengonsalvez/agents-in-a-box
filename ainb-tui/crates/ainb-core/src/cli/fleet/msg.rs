// ABOUTME: `ainb fleet msg send|list|follow`, the chat bus as a first-class
// CLI client of the same frozen fleet/* contract the TUI and macOS app use.
//
// Transport is the existing `fleet::bridge::daemon::DaemonClient` (hangar.sock,
// auth token, Content-Length JSON-RPC); this module adds no second client.
//
// Output contract (CLI surface section of the chat-bus plan):
//   * `--format json` prints JSON on stdout; text is the default rendering.
//   * every error is JSON on STDERR carrying a `retryable` boolean, and the
//     process exits with a semantic code: 0 ok, 1 bad input, 2 daemon/network,
//     3 auth, 4 other, 5 idempotency conflict.
//   * a free-text argument accepts `-` to read stdin.

use std::io::Read as _;

use ainb_hangar_proto::fleet::{
    FLEET_MESSAGE_LIST_MAX, FleetMessage, FleetMessageListParams, FleetMessageSendParams,
};
use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::bridge::daemon::{DaemonClient, DaemonError};

/// Semantic exit codes. The caller of a chat-bus verb branches on these, so
/// they are part of the contract, not decoration.
const EXIT_BAD_INPUT: i32 = 1;
const EXIT_DAEMON: i32 = 2;
const EXIT_AUTH: i32 = 3;
const EXIT_OTHER: i32 = 4;
const EXIT_IDEMPOTENCY_CONFLICT: i32 = 5;

/// One structured failure: what went wrong, whether retrying can help, and the
/// command that answers the question next.
struct CliFailure {
    kind: &'static str,
    message: String,
    retryable: bool,
    exit_code: i32,
    next: Option<String>,
}

impl CliFailure {
    fn bad_input(message: impl Into<String>) -> Self {
        Self {
            kind: "bad_input",
            message: message.into(),
            retryable: false,
            exit_code: EXIT_BAD_INPUT,
            next: None,
        }
    }

    /// Print the failure as JSON on stderr and exit with its semantic code.
    fn exit(self) -> ! {
        let mut payload = serde_json::json!({
            "error": {
                "kind": self.kind,
                "message": self.message,
                "retryable": self.retryable,
                "exit_code": self.exit_code,
            }
        });
        if let Some(next) = self.next {
            payload["error"]["next"] = serde_json::Value::String(next);
        }
        eprintln!("{payload}");
        std::process::exit(self.exit_code);
    }
}

impl From<DaemonError> for CliFailure {
    fn from(error: DaemonError) -> Self {
        let message = error.to_string();
        match error {
            DaemonError::Token(_) => Self {
                kind: "auth",
                message,
                retryable: false,
                exit_code: EXIT_AUTH,
                next: Some("ainb hangar daemon status".to_string()),
            },
            DaemonError::NoHome => Self {
                kind: "other",
                message,
                retryable: false,
                exit_code: EXIT_OTHER,
                next: None,
            },
            DaemonError::Connect { .. } | DaemonError::Io(_) | DaemonError::Timeout(_) => Self {
                kind: "daemon",
                message,
                retryable: true,
                exit_code: EXIT_DAEMON,
                next: Some("ainb hangar daemon status".to_string()),
            },
            DaemonError::Decode(_) => Self {
                kind: "other",
                message,
                retryable: false,
                exit_code: EXIT_OTHER,
                next: None,
            },
            DaemonError::Rpc { code, .. } => {
                if code == ainb_hangar_proto::auth::UNAUTHORIZED {
                    Self {
                        kind: "auth",
                        message,
                        retryable: false,
                        exit_code: EXIT_AUTH,
                        next: Some("ainb hangar daemon status".to_string()),
                    }
                } else if message.contains("request_id was reused") {
                    Self {
                        kind: "idempotency_conflict",
                        message,
                        retryable: false,
                        exit_code: EXIT_IDEMPOTENCY_CONFLICT,
                        next: Some("ainb fleet msg list --limit 20".to_string()),
                    }
                } else if code == -32602 || code == -32601 {
                    Self {
                        kind: "bad_input",
                        message,
                        retryable: false,
                        exit_code: EXIT_BAD_INPUT,
                        next: None,
                    }
                } else {
                    Self {
                        kind: "other",
                        message,
                        retryable: false,
                        exit_code: EXIT_OTHER,
                        next: None,
                    }
                }
            }
        }
    }
}

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("send", sub)) => send(sub, format).await,
        Some(("list", sub)) => list(sub, format).await,
        Some(("follow", sub)) => follow(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet msg` verb: try `ainb fleet msg --help`")
            .exit(),
    }
}

/// Resolve a free-text argument, reading stdin when it is `-`.
fn resolve_text(raw: Option<&String>) -> Result<String, CliFailure> {
    let Some(raw) = raw else {
        return Err(CliFailure::bad_input(
            "--text is required (use `-` to read stdin)",
        ));
    };
    if raw != "-" {
        return Ok(raw.clone());
    }
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|error| CliFailure::bad_input(format!("reading stdin: {error}")))?;
    Ok(buffer)
}

fn client() -> DaemonClient {
    match DaemonClient::from_env() {
        Ok(client) => client,
        Err(error) => CliFailure::from(error).exit(),
    }
}

async fn send(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let targets: Vec<String> = matches
        .get_many::<String>("target")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    if targets.is_empty() {
        CliFailure::bad_input("at least one --target is required").exit();
    }
    let text = match resolve_text(matches.get_one::<String>("text")) {
        Ok(text) => text,
        Err(failure) => failure.exit(),
    };
    if text.trim().is_empty() {
        CliFailure::bad_input("message text must not be empty").exit();
    }
    // A caller that supplies no --request-id gets a fresh one, so a retried
    // shell command is a NEW message rather than a surprise idempotent replay.
    let request_id = matches.get_one::<String>("request-id").cloned().unwrap_or_else(|| {
        format!(
            "cli:{}",
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen)
        )
    });

    let result = client()
        .message_send(FleetMessageSendParams {
            scope_key: matches.get_one::<String>("scope").cloned(),
            targets,
            text,
            request_id,
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => CliFailure::from(error).exit(),
    };

    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("message {}", result.message_id);
        for delivery in &result.deliveries {
            println!("  {} {:?}", delivery.session_key, delivery.state);
        }
    }
    Ok(())
}

async fn list(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let limit = matches.get_one::<u32>("limit").copied().unwrap_or(20);
    let result = client()
        .message_list(FleetMessageListParams {
            scope_key: matches.get_one::<String>("scope").cloned(),
            origin_id: matches.get_one::<String>("origin").cloned(),
            after_id: matches.get_one::<String>("after").cloned(),
            limit: limit.clamp(1, FLEET_MESSAGE_LIST_MAX),
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => CliFailure::from(error).exit(),
    };

    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for message in &result.messages {
            println!("{}", render_message(message));
        }
    }
    Ok(())
}

async fn follow(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let (ack, mut subscription) = match client()
        .open_message_subscription(matches.get_one::<String>("after").cloned())
        .await
    {
        Ok(opened) => opened,
        Err(error) => CliFailure::from(error).exit(),
    };
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&ack)?);
    } else if let Some(head) = &ack.head_id {
        println!("following from {head}");
    } else {
        println!("following an empty log");
    }

    // NDJSON: one committed message per line, until the operator stops us or
    // the daemon goes away.
    loop {
        match subscription.next_message().await {
            Ok(message) => {
                if format == OutputFormat::Json {
                    println!("{}", serde_json::to_string(&message)?);
                } else {
                    println!("{}", render_message(&message));
                }
            }
            Err(error) => CliFailure::from(error).exit(),
        }
    }
}

fn render_message(message: &FleetMessage) -> String {
    format!(
        "{} [{}] {}: {}",
        message.id, message.scope_key, message.sender, message.body
    )
}
