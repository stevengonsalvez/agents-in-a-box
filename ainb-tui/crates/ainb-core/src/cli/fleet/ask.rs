// ABOUTME: CLI read and reply surface for Fleet AskUserQuestion.
//
// `ask` exposes full provider questions, including version and request
// fingerprint. `answer` accepts only a complete structured answer document and
// calls `fleet/action`; it never sends option text through prompt or tmux paths.

use ainb_hangar_proto::fleet::{
    AttentionState, ControlAction, FleetQuestionAnswer, FleetRequestIdentity, FleetSession,
    ManagementState,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::cli::OutputFormat;
use crate::fleet::bridge::daemon::DaemonClient;

#[derive(Debug, Serialize, PartialEq, Eq)]
struct AskList {
    head_revision: i64,
    questions: Vec<AskSession>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct AskSession {
    session_key: String,
    provider: ainb_hangar_proto::fleet::FleetProvider,
    version: i64,
    request_fingerprint: Option<String>,
    answerable: bool,
    questions: Vec<AskQuestion>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct AskQuestion {
    id: String,
    question: String,
    options: Vec<String>,
    multi_select: bool,
}

/// List open structured questions with the exact concurrency fields required
/// by `ainb fleet answer`.
pub async fn execute_ask(_matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let client = DaemonClient::from_env().context("Fleet daemon unavailable")?;
    let snapshot = client.fleet_snapshot().await.context("reading Fleet snapshot")?;
    let list = AskList {
        head_revision: snapshot.head_revision,
        questions: snapshot.sessions.iter().filter_map(ask_session).collect(),
    };
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&list)?),
        _ => print_text(&list),
    }
    Ok(())
}

/// Submit a complete answer set through the versioned structured-action RPC.
///
/// The command verifies the caller's snapshot version and request fingerprint
/// before dispatch. Hangar repeats both checks atomically when it accepts the
/// action, preventing a stale shell invocation from answering a newer prompt.
pub async fn execute_answer(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let session_key = required(matches, "session-key")?;
    let expected_version = *matches.get_one::<i64>("version").context("--version is required")?;
    let request_fingerprint = required(matches, "fingerprint")?;
    let answers: Vec<FleetQuestionAnswer> = serde_json::from_str(&required(matches, "answers")?)
        .context("--answers must be a JSON array of structured question answers")?;

    let client = DaemonClient::from_env().context("Fleet daemon unavailable")?;
    let snapshot = client.fleet_snapshot().await.context("reading Fleet snapshot")?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.session_key == session_key)
        .context("Fleet session no longer exists")?;
    let action = structured_action(session, expected_version, &request_fingerprint, answers)?;
    let receipt = client
        .fleet_action(ainb_hangar_proto::fleet::FleetActionParams {
            session_key,
            expected_version,
            request_id: matches
                .get_one::<String>("request-id")
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            action,
        })
        .await
        .context("submitting structured Fleet answer")?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&receipt)?),
        _ => println!(
            "answered {}: {:?}{}",
            receipt.session_key,
            receipt.status,
            receipt.detail.as_deref().map_or(String::new(), |detail| format!(": {detail}")),
        ),
    }
    Ok(())
}

fn required(matches: &clap::ArgMatches, name: &str) -> Result<String> {
    matches
        .get_one::<String>(name)
        .cloned()
        .with_context(|| format!("--{name} is required"))
}

fn ask_session(session: &FleetSession) -> Option<AskSession> {
    if session.attention != AttentionState::Ask {
        return None;
    }
    let questions = questions(session.current_request.as_ref()?).ok()?;
    Some(AskSession {
        session_key: session.session_key.clone(),
        provider: session.provider,
        version: session.version,
        request_fingerprint: session.current_request_fingerprint.clone(),
        answerable: session.management == ManagementState::Managed
            && session.capabilities.structured_answer
            && session.current_request_fingerprint.is_some(),
        questions,
    })
}

fn structured_action(
    session: &FleetSession,
    expected_version: i64,
    request_fingerprint: &str,
    answers: Vec<FleetQuestionAnswer>,
) -> Result<ControlAction> {
    if session.attention != AttentionState::Ask
        || session.management != ManagementState::Managed
        || !session.capabilities.structured_answer
    {
        bail!("session has no authoritative structured question")
    }
    if session.version != expected_version {
        bail!(
            "stale session version: supplied {expected_version}, current {}",
            session.version
        )
    }
    if session.current_request_fingerprint.as_deref() != Some(request_fingerprint) {
        bail!("stale request fingerprint")
    }
    let request = session
        .current_request
        .as_ref()
        .context("current structured request payload unavailable")?;
    let questions = questions(request)?;
    validate_answers(&questions, &answers)?;
    Ok(ControlAction::StructuredAnswer {
        request_fingerprint: request_fingerprint.to_string(),
        request_identity: request_identity(request),
        answers,
    })
}

fn questions(request: &serde_json::Value) -> Result<Vec<AskQuestion>> {
    let payload = request.get("payload").unwrap_or(request);
    let input = payload.get("tool_input").or_else(|| payload.get("input")).unwrap_or(payload);
    let raw = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .context("structured request has no questions")?;
    raw.iter()
        .enumerate()
        .map(|(index, question)| {
            Ok(AskQuestion {
                id: question
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| index.to_string(), str::to_string),
                question: question
                    .get("question")
                    .or_else(|| question.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .context("structured question text absent")?
                    .to_string(),
                options: question
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        option
                            .as_str()
                            .or_else(|| option.get("label").and_then(serde_json::Value::as_str))
                            .map(str::to_string)
                    })
                    .collect(),
                multi_select: question
                    .get("multiSelect")
                    .or_else(|| question.get("multi_select"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn validate_answers(questions: &[AskQuestion], answers: &[FleetQuestionAnswer]) -> Result<()> {
    if answers.len() != questions.len() {
        bail!("answers must include every current question exactly once")
    }
    for question in questions {
        let matching: Vec<_> =
            answers.iter().filter(|answer| answer.question_id == question.id).collect();
        if matching.len() != 1 {
            bail!("question {} must have exactly one answer", question.id)
        }
        let answer = matching[0];
        if !question.multi_select && answer.selected_options.len() > 1 {
            bail!("question {} accepts one selected option", question.id)
        }
        if answer
            .selected_options
            .iter()
            .any(|selected| !question.options.iter().any(|option| option == selected))
        {
            bail!(
                "answer contains an option not offered by question {}",
                question.id
            )
        }
        let text = answer.text.as_deref().filter(|text| !text.trim().is_empty());
        if answer.selected_options.is_empty() && text.is_none() {
            bail!(
                "question {} needs an option or structured text",
                question.id
            )
        }
        if text.is_some()
            && !answer
                .selected_options
                .iter()
                .any(|option| option.eq_ignore_ascii_case("other"))
        {
            bail!(
                "question {} accepts text only with the Other option",
                question.id
            )
        }
    }
    Ok(())
}

fn request_identity(request: &serde_json::Value) -> Option<FleetRequestIdentity> {
    let payload = request.get("payload").unwrap_or(request);
    let identity = payload.get("identity").unwrap_or(payload);
    let request_id = identity
        .get("requestId")
        .or_else(|| identity.get("request_id"))
        .or_else(|| identity.get("tool_use_id"))
        .or_else(|| identity.get("id"))?
        .clone();
    let field = |camel: &str, snake: &str| {
        identity
            .get(camel)
            .or_else(|| identity.get(snake))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(FleetRequestIdentity {
        request_id,
        thread_id: field("threadId", "thread_id"),
        turn_id: field("turnId", "turn_id"),
        item_id: field("itemId", "item_id"),
    })
}

fn print_text(list: &AskList) {
    if list.questions.is_empty() {
        println!("no structured questions");
        return;
    }
    for session in &list.questions {
        println!(
            "{} [{} v{}{}]",
            session.session_key,
            session.request_fingerprint.as_deref().unwrap_or("no fingerprint"),
            session.version,
            if session.answerable {
                ""
            } else {
                ", read-only"
            },
        );
        for question in &session.questions {
            println!("  {}: {}", question.id, question.question);
            for option in &question.options {
                println!("    - {option}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::fleet::{
        FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider, LifecycleState,
        ManagementState, TransportHealth,
    };

    fn session() -> FleetSession {
        FleetSession {
            session_key: "claude:session-1".into(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("session-1".into()),
            tmux_target: None,
            process_start_fingerprint: None,
            cwd: "/work".into(),
            display_name: None,
            lifecycle: LifecycleState::Idle,
            attention: AttentionState::Ask,
            current_request_fingerprint: Some("fnv1a64:question".into()),
            current_request: Some(serde_json::json!({
                "payload": {
                    "tool_use_id": "toolu-1",
                    "tool_input": {"questions": [
                        {"id": "regions", "question": "Regions?", "multiSelect": true,
                         "options": [{"label": "eu"}, {"label": "us"}]},
                        {"id": "mode", "question": "Mode?", "options": ["safe", "fast"]}
                    ]}
                }
            })),
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities {
                structured_answer: true,
                ..Default::default()
            },
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 1,
            lifecycle_updated_at: 1,
            attention_updated_at: 1,
            version: 7,
            updated_revision: 11,
        }
    }

    fn answers() -> Vec<FleetQuestionAnswer> {
        vec![
            FleetQuestionAnswer {
                question_id: "regions".into(),
                selected_options: vec!["eu".into(), "us".into()],
                text: None,
            },
            FleetQuestionAnswer {
                question_id: "mode".into(),
                selected_options: vec!["safe".into()],
                text: None,
            },
        ]
    }

    #[test]
    fn ask_exposes_complete_question_contract() {
        let ask = ask_session(&session()).expect("ask session");
        assert_eq!(ask.version, 7);
        assert_eq!(ask.questions.len(), 2);
        assert_eq!(ask.questions[0].options, ["eu", "us"]);
        assert!(ask.answerable);
    }

    #[test]
    fn ask_keeps_degraded_question_visible_but_read_only() {
        let mut degraded = session();
        degraded.management = ManagementState::Degraded;
        let ask = ask_session(&degraded).expect("degraded ask session");
        assert!(!ask.answerable);
        assert_eq!(ask.questions.len(), 2);
    }

    #[test]
    fn answer_builds_structured_action_without_prompt_text() {
        let action = structured_action(&session(), 7, "fnv1a64:question", answers()).unwrap();
        let ControlAction::StructuredAnswer { answers, .. } = action else {
            panic!("CLI must submit structured_answer")
        };
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].selected_options, ["eu", "us"]);
    }

    #[test]
    fn answer_refuses_stale_or_incomplete_contract() {
        assert!(structured_action(&session(), 8, "fnv1a64:question", answers()).is_err());
        assert!(structured_action(&session(), 7, "fnv1a64:stale", answers()).is_err());
        assert!(
            structured_action(&session(), 7, "fnv1a64:question", answers()[..1].to_vec()).is_err()
        );
    }
}
