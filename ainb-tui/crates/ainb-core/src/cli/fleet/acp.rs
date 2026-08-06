// ABOUTME: `ainb fleet acp create` and `ainb fleet transcript [--follow]`, the
// Phase 5 half of the chat bus's CLI parity rule: every wire method ships its
// verb in the SAME phase as its dispatch arm, never later.
//
// Transport, output contract and exit codes are `msg.rs`'s: JSON on stdout under
// `--format json`, JSON errors on stderr carrying `retryable`, and the semantic
// exit codes (0 ok, 1 bad input, 2 daemon/network, 3 auth, 4 other, 5
// idempotency conflict). This module deliberately reuses that machinery rather
// than growing a second dialect of it.

use ainb_hangar_proto::fleet::{
    FLEET_TRANSCRIPT_LIST_MAX, FleetAcpSessionCreateParams, FleetTranscriptChunk,
    FleetTranscriptListParams, FleetTranscriptSubscribeParams,
};
use anyhow::Result;

use crate::cli::OutputFormat;
use crate::cli::fleet::msg::{CliFailure, client};

/// `ainb fleet acp create --provider <p> --cwd <dir> [--scope <k>]`
pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format);
    match matches.subcommand() {
        Some(("create", sub)) => create(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet acp` verb: try `ainb fleet acp --help`")
            .exit(),
    }
}

async fn create(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let Some(provider) = matches.get_one::<String>("provider").cloned() else {
        CliFailure::bad_input("--provider is required").exit();
    };
    // Resolved to an ABSOLUTE path here, because the daemon's cwd is not the
    // operator's: a relative `--cwd .` would silently create the session
    // against the daemon's working directory.
    let cwd = match matches.get_one::<String>("cwd") {
        Some(cwd) => std::path::Path::new(cwd).canonicalize().map_or_else(
            |error| CliFailure::bad_input(format!("--cwd {cwd}: {error}")).exit(),
            |path| path.display().to_string(),
        ),
        None => CliFailure::bad_input("--cwd is required").exit(),
    };

    let result = client()
        .acp_session_create(FleetAcpSessionCreateParams {
            provider,
            cwd,
            scope_key: matches.get_one::<String>("scope").cloned(),
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => CliFailure::from(error).exit(),
    };
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("{} {}", result.session_key, result.scope_key);
        println!(
            "next: ainb fleet msg send --target {} --text 'hello'",
            result.session_key
        );
    }
    Ok(())
}

/// `ainb fleet transcript <session_key> [--after N] [--limit N] [--follow]`
pub async fn execute_transcript(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format);
    let Some(session_key) = matches.get_one::<String>("session_key").cloned() else {
        CliFailure::bad_input("a session_key is required").exit();
    };
    let after_order = matches.get_one::<i64>("after").copied();
    let limit = matches
        .get_one::<u32>("limit")
        .copied()
        .unwrap_or(50)
        .clamp(1, FLEET_TRANSCRIPT_LIST_MAX);

    if matches.get_flag("follow") {
        return follow(&session_key, after_order, format).await;
    }
    let result = client()
        .transcript_list(FleetTranscriptListParams {
            session_key,
            after_order,
            limit,
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => CliFailure::from(error).exit(),
    };
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for chunk in &result.chunks {
            println!("{}", render_chunk(chunk));
        }
    }
    Ok(())
}

/// Stream this session's transcript until the operator stops us.
///
/// Subscribing BEFORE the turn is the point: the daemon's pump commits on a
/// cadence and wakes subscribers after every committed batch, so chunks arrive
/// during the turn rather than at its end (I12).
async fn follow(session_key: &str, after_order: Option<i64>, format: OutputFormat) -> Result<()> {
    let (ack, mut subscription) = match client()
        .open_transcript_subscription(FleetTranscriptSubscribeParams {
            session_key: session_key.to_string(),
            after_order,
        })
        .await
    {
        Ok(opened) => opened,
        Err(error) => CliFailure::from(error).exit(),
    };
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&ack)?);
    } else {
        match ack.head_order {
            Some(head) => println!("following {session_key} from {head}"),
            None => println!("following {session_key} from an empty transcript"),
        }
    }
    loop {
        match subscription.next_chunk().await {
            Ok(chunk) => {
                if format == OutputFormat::Json {
                    println!("{}", serde_json::to_string(&chunk)?);
                } else {
                    println!("{}", render_chunk(&chunk));
                }
            }
            Err(error) => CliFailure::from(error).exit(),
        }
    }
}

fn reject_tabular(format: OutputFormat) {
    if matches!(format, OutputFormat::Csv | OutputFormat::Markdown) {
        CliFailure::bad_input(
            "the chat-bus verbs render text or `--format json`; csv and markdown are not supported",
        )
        .exit();
    }
}

/// Render one chunk as EXACTLY one line of terminal-safe text.
///
/// The payload is adapter-authored text (tool output, model text) printed
/// straight to a terminal, so it gets `msg list`'s escaping for the same reason:
/// left raw it could repaint the screen or forge extra lines. `--format json` is
/// the lossless surface.
fn render_chunk(chunk: &FleetTranscriptChunk) -> String {
    format!(
        "{} {} {}",
        chunk.ingest_order,
        crate::cli::fleet::msg::escape_control(&chunk.event_type),
        crate::cli::fleet::msg::escape_control(&chunk.payload.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(payload: serde_json::Value) -> FleetTranscriptChunk {
        FleetTranscriptChunk {
            ingest_order: 7,
            event_id: "evt".to_string(),
            session_key: "acp:one".to_string(),
            event_type: "acp.message".to_string(),
            payload,
            observed_at: 1,
        }
    }

    /// An adapter that emits ANSI must not be able to repaint the operator's
    /// terminal through `transcript --follow`.
    #[test]
    fn an_ansi_laden_chunk_renders_as_one_inert_line() {
        let rendered = render_chunk(&chunk(serde_json::json!({
            "text": "\u{1b}[2Jwiped\nforged 8 acp.message {}"
        })));
        assert_eq!(rendered.lines().count(), 1, "{rendered}");
        assert!(!rendered.chars().any(char::is_control), "{rendered:?}");
        assert!(rendered.starts_with("7 acp.message "), "{rendered}");
    }
}
