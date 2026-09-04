// ABOUTME: `ainb fleet channel|copilot|confirm|activity`, the CLI half of the
// part-2 chat surface (channels, copilot config, guardrail confirm cards, the
// activity feed).
//
// Same contract as `ainb fleet msg`: `--format json` prints JSON on stdout,
// text is the default rendering, and every failure is JSON on stderr with the
// same semantic exit codes (`CliFailure`, reused from `msg` rather than
// re-declared). Four verb families in one module because they are one surface
// and each one is a handful of lines.

use ainb_hangar_proto::fleet::{
    FLEET_ACTIVITY_LIST_MAX, FleetActivityListParams, FleetActivityListResult, FleetChannel,
    FleetChannelCreateParams, FleetChannelCreateResult, FleetChannelKind, FleetChannelListResult,
    FleetConfirm, FleetConfirmAnswer, FleetConfirmAnswerParams, FleetConfirmAnswerResult,
    FleetConfirmListParams, FleetConfirmListResult, FleetCopilotConfigureParams,
    FleetCopilotConfigureResult, FleetCopilotMode,
};
use ainb_hangar_proto::methods;
use anyhow::Result;

use crate::cli::OutputFormat;
use crate::cli::fleet::msg::{CliFailure, client, escape_control};

/// Refuse the tabular formats up front, like `fleet msg` does: there is no
/// honest csv rendering of a confirm card, and silently printing prose for a
/// script that asked for csv is how a caller ends up parsing help text.
fn reject_tabular(format: OutputFormat, verb: &str) {
    if matches!(format, OutputFormat::Csv | OutputFormat::Markdown) {
        CliFailure::bad_input(format!(
            "`ainb fleet {verb}` renders text or `--format json`; csv and markdown are not supported"
        ))
        .exit();
    }
}

/// One typed round trip through the shared daemon client.
async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
    method: &'static str,
    params: &P,
) -> R {
    match client().call_typed::<P, R>(method, params).await {
        Ok(decoded) => decoded,
        Err(error) => CliFailure::from(error).exit(),
    }
}

// -------------------------------------------------------------- fleet channel

pub async fn execute_channel(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format, "channel");
    match matches.subcommand() {
        Some(("create", sub)) => channel_create(sub, format).await,
        Some(("list", _)) => channel_list(format).await,
        Some(("send", sub)) => channel_send(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet channel` verb: try --help").exit(),
    }
}

async fn channel_create(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let kind = match matches.get_one::<String>("kind").map(String::as_str) {
        Some("copilot") => FleetChannelKind::Copilot,
        _ => FleetChannelKind::Broadcast,
    };
    let recipients: Vec<String> = matches
        .get_many::<String>("recipient")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let name = matches.get_one::<String>("name").cloned().unwrap_or_default();
    if name.trim().is_empty() {
        CliFailure::bad_input("--name is required").exit();
    }
    let result: FleetChannelCreateResult = call(
        methods::FLEET_CHANNEL_CREATE,
        &FleetChannelCreateParams {
            kind,
            name,
            recipients: (!recipients.is_empty()).then_some(recipients),
        },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("{}", render_channel(&result.channel));
    }
    Ok(())
}

async fn channel_list(format: OutputFormat) -> Result<()> {
    let result: FleetChannelListResult =
        call(methods::FLEET_CHANNEL_LIST, &serde_json::json!({})).await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for channel in &result.channels {
            println!("{}", render_channel(channel));
        }
    }
    Ok(())
}

/// Send one message into a channel, addressed to its whole membership.
///
/// The membership is RESOLVED from the daemon, never asked for on the command
/// line. `fleet/message_send` refuses a target that is not a member, so a
/// caller who had to type `--target` per member would be maintaining a copy of
/// the channel's roster in their shell history, and every send after a
/// membership change would be refused for a reason that reads like a bug.
///
/// The fan-out is ONE send with N legs (part 2's Phase C rule), so the answer
/// is one message id and one receipt per recipient.
async fn channel_send(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    use ainb_hangar_proto::fleet::{FleetMessageSendParams, FleetMessageSendResult};

    let wanted = matches.get_one::<String>("channel").cloned().unwrap_or_default();
    if wanted.trim().is_empty() {
        CliFailure::bad_input("--channel is required").exit();
    }
    let wanted = wanted.trim();
    let text = match crate::cli::fleet::msg::resolve_text(matches.get_one::<String>("text")) {
        Ok(text) => text,
        Err(failure) => failure.exit(),
    };
    if text.trim().is_empty() {
        CliFailure::bad_input("message text must not be empty").exit();
    }

    let listed: FleetChannelListResult =
        call(methods::FLEET_CHANNEL_LIST, &serde_json::json!({})).await;
    // By id OR by the minted scope, because both are printed by `channel list`
    // and an operator will paste whichever their eye landed on.
    let Some(channel) = listed
        .channels
        .into_iter()
        .find(|channel| channel.id == wanted || channel.scope_key == wanted)
    else {
        CliFailure::bad_input(format!(
            "no channel with id or scope {wanted:?}; list them with `ainb fleet channel list`"
        ))
        .exit();
    };
    if channel.recipients.is_empty() {
        CliFailure::bad_input(format!(
            "channel {:?} has no members to send to",
            escape_control(&channel.name)
        ))
        .exit();
    }
    // A caller that supplies no --request-id gets a fresh one, exactly like
    // `msg send`: a retried shell command is a NEW message, not a surprise
    // idempotent replay.
    let request_id = matches.get_one::<String>("request-id").cloned().unwrap_or_else(|| {
        format!(
            "cli:{}",
            ainb_hangar_core::idgen::IdGen::new_ulid(&ainb_hangar_core::idgen::SystemIdGen)
        )
    });
    let result: FleetMessageSendResult = call(
        methods::FLEET_MESSAGE_SEND,
        &FleetMessageSendParams {
            scope_key: Some(channel.scope_key.clone()),
            // The CLI IS the operator's hand; the daemon's default says so.
            actor: None,
            targets: channel.recipients.clone(),
            origin_message_id: None,
            text,
            request_id,
        },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!(
            "message {} -> {}",
            escape_control(&result.message_id),
            escape_control(&channel.scope_key)
        );
        for delivery in &result.deliveries {
            println!("  {}", crate::cli::fleet::msg::render_delivery(delivery));
        }
    }
    Ok(())
}

/// One channel as EXACTLY one line of terminal-safe text: a channel name is
/// operator-supplied and reaches a terminal, so it is escaped like a message
/// body (see `msg::render_message`).
fn render_channel(channel: &FleetChannel) -> String {
    format!(
        "{} [{}] {} -> {}",
        escape_control(&channel.scope_key),
        match channel.kind {
            FleetChannelKind::Copilot => "copilot",
            FleetChannelKind::Broadcast => "broadcast",
        },
        escape_control(&channel.name),
        if channel.recipients.is_empty() {
            "(no members)".to_string()
        } else {
            channel
                .recipients
                .iter()
                .map(|key| escape_control(key))
                .collect::<Vec<_>>()
                .join(",")
        }
    )
}

// -------------------------------------------------------------- fleet copilot

pub async fn execute_copilot(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format, "copilot");
    match matches.subcommand() {
        Some(("configure", sub)) => copilot_configure(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet copilot` verb: try --help").exit(),
    }
}

async fn copilot_configure(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    // Passed through UNVALIDATED on purpose: the adapter registry lives in the
    // daemon's config, so a list baked in here would refuse an adapter the
    // daemon can already spawn, and would go stale the moment one is added.
    // `required(true)` in the registry, so the fallback is unreachable; spelled
    // out rather than unwrapped so a future `required(false)` cannot panic here.
    let Some(provider) = matches.get_one::<String>("provider").cloned() else {
        CliFailure::bad_input("--provider is required; `ainb fleet adapter list` names them")
            .exit();
    };
    let copilot_mode = matches
        .get_one::<String>("copilot-mode")
        .and_then(|raw| FleetCopilotMode::parse(raw));
    // The persona is a FILE, never an inline flag: it is a system prompt for an
    // agent holding destructive tools, and a multi-line one on a command line
    // ends up in shell history verbatim.
    let persona = match matches.get_one::<String>("persona-file") {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) => {
                CliFailure::bad_input(format!("reading {path}: {error}")).exit();
            }
        },
        None => None,
    };
    let result: FleetCopilotConfigureResult = call(
        methods::FLEET_COPILOT_CONFIGURE,
        &FleetCopilotConfigureParams {
            provider,
            copilot_mode,
            model: matches.get_one::<String>("model").cloned(),
            reasoning_effort: matches.get_one::<String>("reasoning-effort").cloned(),
            persona,
        },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!(
            "{} provider={} mode={} model={} reasoning={} persona={}{}",
            escape_control(&result.session_key),
            escape_control(&result.provider),
            result.copilot_mode.as_str(),
            result.model.as_deref().unwrap_or("-"),
            result.reasoning_effort.as_deref().unwrap_or("-"),
            if result.persona_set { "set" } else { "-" },
            // The session key CHANGED, and a caller holding the old one is
            // holding a dead session. Saying so is cheaper than letting the
            // next command fail with "no live session".
            if result.session_replaced {
                " (engine swapped; new session)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

// -------------------------------------------------------------- fleet adapter

pub async fn execute_adapter(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format, "adapter");
    match matches.subcommand() {
        Some(("list", _)) => adapter_list(format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet adapter` verb: try --help").exit(),
    }
}

async fn adapter_list(format: OutputFormat) -> Result<()> {
    use ainb_hangar_proto::fleet::FleetAdapterListResult;

    let result: FleetAdapterListResult =
        call(methods::FLEET_ADAPTER_LIST, &serde_json::json!({})).await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for adapter in &result.adapters {
            println!(
                "{}\t{}\tpermission_mode={}\t{}",
                escape_control(&adapter.name),
                escape_control(&adapter.command),
                escape_control(&adapter.permission_mode),
                if adapter.built_in {
                    "built-in"
                } else {
                    "config"
                }
            );
        }
    }
    Ok(())
}

// -------------------------------------------------------------- fleet confirm

pub async fn execute_confirm(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format, "confirm");
    match matches.subcommand() {
        Some(("list", sub)) => confirm_list(sub, format).await,
        Some(("answer", sub)) => confirm_answer(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet confirm` verb: try --help").exit(),
    }
}

async fn confirm_list(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let result: FleetConfirmListResult = call(
        methods::FLEET_CONFIRM_LIST,
        &FleetConfirmListParams {
            scope_key: matches.get_one::<String>("scope").cloned(),
        },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for card in &result.confirms {
            println!("{}", render_confirm(card));
        }
    }
    Ok(())
}

async fn confirm_answer(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let confirm_id = matches.get_one::<String>("confirm_id").cloned().unwrap_or_default();
    if confirm_id.trim().is_empty() {
        CliFailure::bad_input("a confirm id is required").exit();
    }
    let deny = matches.get_flag("deny");
    let edit = matches.get_one::<String>("edit");
    if deny && edit.is_some() {
        CliFailure::bad_input("--deny and --edit are mutually exclusive").exit();
    }
    let answer = match (deny, edit) {
        (true, _) => FleetConfirmAnswer::Deny,
        (false, Some(raw)) => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(arguments) if arguments.is_object() => FleetConfirmAnswer::Edit { arguments },
            Ok(_) => CliFailure::bad_input("--edit must be a JSON object").exit(),
            Err(error) => CliFailure::bad_input(format!("--edit is not JSON: {error}")).exit(),
        },
        (false, None) => FleetConfirmAnswer::Approve,
    };
    let result: FleetConfirmAnswerResult = call(
        methods::FLEET_CONFIRM_ANSWER,
        &FleetConfirmAnswerParams { confirm_id, answer },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("{} {:?}", escape_control(&result.confirm_id), result.state);
    }
    Ok(())
}

/// One card as EXACTLY one line. The arguments are ALREADY projected to the
/// tool's declared keys by the daemon, so what prints here is what the tool
/// would run, with nothing the model added to argue its own case.
fn render_confirm(card: &FleetConfirm) -> String {
    format!(
        "{} [{}] {} {} {}",
        escape_control(&card.confirm_id),
        escape_control(&card.scope_key),
        escape_control(&card.tool),
        escape_control(&card.arguments.to_string()),
        card.target_session_key.as_deref().map_or(String::new(), escape_control)
    )
}

// ------------------------------------------------------------- fleet activity

pub async fn execute_activity(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    reject_tabular(format, "activity");
    match matches.subcommand() {
        Some(("list", sub)) => activity_list(sub, format).await,
        _ => CliFailure::bad_input("unknown `ainb fleet activity` verb: try --help").exit(),
    }
}

async fn activity_list(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let limit = matches.get_one::<u32>("limit").copied().unwrap_or(50);
    let result: FleetActivityListResult = call(
        methods::FLEET_ACTIVITY_LIST,
        &FleetActivityListParams {
            scope_key: matches.get_one::<String>("scope").cloned(),
            after_seq: matches.get_one::<i64>("after").copied(),
            limit: limit.clamp(1, FLEET_ACTIVITY_LIST_MAX),
        },
    )
    .await;
    if format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for row in &result.activities {
            println!(
                "{} [{}] {} {:?} {:?} {}",
                row.seq,
                escape_control(&row.scope_key),
                escape_control(&row.tool),
                row.class,
                row.outcome,
                row.detail.as_deref().map_or(String::new(), escape_control)
            );
        }
    }
    Ok(())
}
