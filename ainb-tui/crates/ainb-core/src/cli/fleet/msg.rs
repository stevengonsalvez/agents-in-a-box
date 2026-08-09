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
    FLEET_MESSAGE_BODY_MAX, FLEET_MESSAGE_LIST_MAX, FleetMessage, FleetMessageListParams,
    FleetMessageSendParams,
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
///
/// Shared with the other chat-bus verbs (`fleet acp`, `fleet transcript`) so
/// every one of them exits with the same semantic codes and the same JSON error
/// shape a script parses.
pub(crate) struct CliFailure {
    kind: &'static str,
    message: String,
    retryable: bool,
    exit_code: i32,
    next: Option<String>,
}

impl CliFailure {
    pub(crate) fn bad_input(message: impl Into<String>) -> Self {
        Self {
            kind: "bad_input",
            message: message.into(),
            retryable: false,
            exit_code: EXIT_BAD_INPUT,
            next: None,
        }
    }

    /// Print the failure as JSON on stderr and exit with its semantic code.
    pub(crate) fn exit(self) -> ! {
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
    // `--format csv|markdown` used to render the TEXT shape, so a script asking
    // for csv got single-column prose and no way to tell. There is no tabular
    // rendering of a chat log worth having; say so instead of pretending.
    if matches!(format, OutputFormat::Csv | OutputFormat::Markdown) {
        CliFailure::bad_input(
            "`ainb fleet msg` renders text or `--format json`; csv and markdown are not supported",
        )
        .exit();
    }
    match matches.subcommand() {
        Some(("send", sub)) => send(sub, format).await,
        Some(("list", sub)) => list(sub, format).await,
        Some(("follow", sub)) => follow(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet msg` verb: try `ainb fleet msg --help`")
            .exit(),
    }
}

/// Resolve a free-text argument, reading stdin when it is `-`.
///
/// The stdin read is BOUNDED by [`FLEET_MESSAGE_BODY_MAX`], the same ceiling
/// the daemon enforces: `ainb fleet msg send --text - < some-huge-file` would
/// otherwise buffer the whole file in this process only to have the daemon
/// refuse it. One byte past the limit is read deliberately, so "exactly at the
/// limit" and "over it" are distinguishable without reading the rest.
fn resolve_text(raw: Option<&String>) -> Result<String, CliFailure> {
    let Some(raw) = raw else {
        return Err(CliFailure::bad_input(
            "--text is required (use `-` to read stdin)",
        ));
    };
    let text = if raw == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .take(FLEET_MESSAGE_BODY_MAX as u64 + 1)
            .read_to_string(&mut buffer)
            .map_err(|error| CliFailure::bad_input(format!("reading stdin: {error}")))?;
        buffer
    } else {
        raw.clone()
    };
    if text.len() > FLEET_MESSAGE_BODY_MAX {
        return Err(CliFailure::bad_input(format!(
            "message text must be at most {FLEET_MESSAGE_BODY_MAX} bytes"
        )));
    }
    Ok(text)
}

pub(crate) fn client() -> DaemonClient {
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
            // The CLI IS the operator's hand; the daemon's default says so.
            actor: None,
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
    // Rejected here as well as in the daemon so the operator gets the exit-1
    // bad-input path (and no socket round trip) rather than a -32602 that has
    // to be translated back.
    if matches.get_one::<String>("scope").is_some() && matches.get_one::<String>("origin").is_some()
    {
        CliFailure::bad_input(
            "--origin and --scope are mutually exclusive; replies live in their recipients' scopes",
        )
        .exit();
    }
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

/// Render one message as EXACTLY one line of terminal-safe text.
///
/// Message bodies are attacker-influenced: anything on the bus can send one,
/// and `msg list` / `msg follow` print them straight to a terminal. Left raw, a
/// body could repaint the screen, move the cursor, or embed newlines that forge
/// extra log lines an operator would read as separate messages. Escaping every
/// control character (C0 including ESC, DEL, and C1) keeps the one-message-
/// one-line contract that `follow`'s consumers parse against.
///
/// This is the LOSSY surface by design; `--format json` is the lossless one.
fn render_message(message: &FleetMessage) -> String {
    format!(
        "{} [{}] {}: {}",
        escape_control(&message.id),
        escape_control(&message.scope_key),
        escape_control(&message.sender),
        escape_control(&message.body)
    )
}

/// Replace every control character with a visible escape.
pub(crate) fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            // Covers C0 (ESC included), DEL and C1.
            control if control.is_control() => {
                out.push_str(&format!("\\u{{{:04x}}}", control as u32));
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::fleet::FleetMessageKind;

    fn message(body: &str) -> FleetMessage {
        FleetMessage {
            id: "01J0MSG".to_string(),
            scope_key: "session:claude:one".to_string(),
            origin_message_id: None,
            sender: "operator".to_string(),
            kind: FleetMessageKind::User,
            body: body.to_string(),
            created_at: 1,
        }
    }

    /// An ANSI-laden body must not be able to repaint the operator's terminal
    /// or forge a second log line.
    #[test]
    fn an_ansi_laden_body_renders_as_one_inert_line() {
        let rendered = render_message(&message(
            "\u{1b}[2J\u{1b}[31mred\u{1b}[0m\nforged 01J0FAKE [scope] agent: pwned\u{7}",
        ));

        assert_eq!(
            rendered.lines().count(),
            1,
            "one message must stay one line: {rendered}"
        );
        assert!(
            !rendered.chars().any(char::is_control),
            "no control character may survive to the terminal: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u{001b}[2J"),
            "ESC is escaped: {rendered}"
        );
        assert!(
            rendered.contains("\\nforged"),
            "the newline is escaped: {rendered}"
        );
        assert!(rendered.contains("\\u{0007}"), "BEL is escaped: {rendered}");
        assert!(
            rendered.starts_with("01J0MSG [session:claude:one] operator: "),
            "ordinary text is untouched: {rendered}"
        );
    }

    /// The escape is reversible-looking rather than ambiguous: a literal
    /// backslash in a body must not read as the start of an escape.
    #[test]
    fn a_literal_backslash_is_escaped_too() {
        assert!(render_message(&message(r"C:\n")).ends_with(r"C:\\n"));
    }
}
