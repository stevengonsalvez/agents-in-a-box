// ABOUTME: Host Fleet subscription, daemon health, and detached control RPCs.
//
// The sessions screen lets the human ACT on what a session needs: answer an
// interview (ASK), decide a permission request, or send a message. The send
// itself is async (`fleet::send::send`, the same tmux send-keys path `fleet
// broadcast` uses) and the TUI key path is sync, so this module owns the
// bridge: a detached worker thread builds a current-thread tokio runtime,
// discovers the live session matching the row, sends the text, and publishes a
// human-readable outcome into a shared cell the surface renders.
//
// Living outside `src/components/` keeps the (legitimately async) send logic
// clear of the render-thread `.await` lint — the worker thread is NOT the
// render path; the component only ever touches the shared feedback cell under a
// microsecond lock.
//
// SAFETY (C1): an ASK answer must reach the EXACT agent that asked. The hook's
// claude session id usually differs from a discovered `Session.id`, so an exact
// id match often fails and we'd fall to a cwd correlation — but two agents can
// share a cwd, so a cwd guess could send the answer to the WRONG agent. We
// therefore REFUSE to answer on an ambiguous cwd (more than one discovered
// session in that cwd, or a merged session that aggregated 2+ sources) rather
// than silently mis-route a safety-critical interview answer. Broadcasts apply
// the same guard (a ping to the wrong agent is lower-harm, but we still refuse —
// the safe call).
//
// CONCURRENCY (C3): each dispatch refuses while another is in flight (an
// `AtomicBool` guard) so key-repeat can't spawn unbounded worker threads or
// double-send an answer to an agent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ainb_hangar_proto::fleet::FleetSnapshot;

use crate::fleet::bridge::daemon::FleetStreamEvent;

/// The JSON-RPC "method not found" code.
///
/// A daemon older than this client answers the confirm/activity reads with it,
/// and that is a MISSING HALF of the page rather than a failed page: the
/// timeline still renders, and the pane says which half it could not get.
const RPC_METHOD_NOT_FOUND: i32 = -32601;

/// Page ONE session's own chat thread on a worker thread.
///
/// Three of the copilot page's four calls are absent on purpose: a session
/// thread has no channel to resolve (part 1 mints `session:<key>` for it), no
/// ACP session to create (it IS the session), and no guardrail cards or copilot
/// activity (that machinery belongs to the copilot channel). What is left is
/// the timeline, in commit order.
///
/// The scope comes from [`ChatTopic::scope_key`], the ONE place a topic is
/// turned into a scope on this side of the socket, rather than being re-derived
/// here. The daemon derives the same string when a single-target
/// `fleet/message_send` omits `scope_key`, and the tripwire is what proves the
/// two agree: the thread reads what the composer wrote.
///
/// [`ChatTopic::scope_key`]: ainb_plugin_hangar::screen::fleet_chat::ChatTopic::scope_key
pub fn chat_thread_page_blocking(
    topic: &ainb_plugin_hangar::screen::fleet_chat::ChatTopic,
) -> Result<ainb_plugin_hangar::screen::fleet_chat::ChatSnapshot, ChatPageFailure> {
    use ainb_hangar_proto::fleet::{FLEET_MESSAGE_LIST_MAX, FleetMessageListParams};
    use ainb_plugin_hangar::screen::fleet_chat::ChatOpenStep;

    let scope = topic.scope_key().ok_or_else(|| {
        ChatPageFailure::new(ChatOpenStep::Connecting, "this topic has no session scope")
    })?;
    let target_session_key = topic.target_session_key();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ChatPageFailure::new(ChatOpenStep::Connecting, error))?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| ChatPageFailure::new(ChatOpenStep::Connecting, error))?;
        let messages = client
            .message_list(FleetMessageListParams {
                scope_key: Some(scope.clone()),
                origin_id: None,
                after_id: None,
                limit: FLEET_MESSAGE_LIST_MAX,
            })
            .await
            .map_err(|error| ChatPageFailure::new(ChatOpenStep::LoadingMessages, error))?
            .messages;
        Ok(ainb_plugin_hangar::screen::fleet_chat::ChatSnapshot {
            scope_key: Some(scope),
            target_session_key,
            messages,
            confirms: Vec::new(),
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
            // A tmux thread has no ACP session and therefore no turn deadline.
            // Reporting one would put a bound on a wait nothing enforces.
            turn_deadline_ms: None,
        })
    })
}

/// One failed step of the sequence that opens a conversation.
///
/// Carries the CALL as well as the detail because the pane's whole job in this
/// phase is to name it: "connection refused" is four different bugs depending
/// on whether it came from the dial, the channel read, the channel mint or the
/// session mint, and only the last of those is fixed by changing directory.
#[derive(Debug, Clone)]
pub struct ChatPageFailure {
    /// The call that failed.
    pub step: ainb_plugin_hangar::screen::fleet_chat::ChatOpenStep,
    /// The daemon's own words, never a friendlier summary.
    pub detail: String,
}

impl ChatPageFailure {
    fn new(
        step: ainb_plugin_hangar::screen::fleet_chat::ChatOpenStep,
        detail: impl std::fmt::Display,
    ) -> Self {
        Self {
            step,
            detail: detail.to_string(),
        }
    }
}

/// Page the copilot chat surface on a worker thread.
///
/// One worker, four calls, because the surface needs all four to say anything
/// true: the channel (to learn its MINTED `channel:<ulid>` scope), the
/// timeline, the open confirm cards, and the activity feed.
///
/// The scope is RESOLVED, never assumed. `fleet/channel_create` mints the id,
/// so a client that hardcodes `channel:copilot` reads an empty timeline forever
/// against a real daemon while every one of its unit tests stays green. That is
/// the same shape as the ACP provider label that shipped as `UNKNOWN`.
///
/// The confirm feed is fetched RAW so one card carrying a state token this
/// build predates cannot blank the whole list; the screen decodes card by card
/// and refuses to answer any it could not decode.
pub fn chat_page_blocking(
    scope_key: Option<String>,
    progress: &dyn Fn(ainb_plugin_hangar::screen::fleet_chat::ChatOpenStep),
) -> Result<ainb_plugin_hangar::screen::fleet_chat::ChatSnapshot, ChatPageFailure> {
    use ainb_hangar_proto::fleet::{
        FLEET_ACTIVITY_LIST_MAX, FLEET_MESSAGE_LIST_MAX, FleetActivityListParams,
        FleetChannelCreateParams, FleetChannelKind, FleetConfirmListParams, FleetMessageListParams,
    };
    use ainb_plugin_hangar::screen::fleet_chat::ChatOpenStep;

    progress(ChatOpenStep::Connecting);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ChatPageFailure::new(ChatOpenStep::Connecting, error))?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| ChatPageFailure::new(ChatOpenStep::Connecting, error))?;

        // Resolve the copilot channel. NEWEST wins, matching the daemon's own
        // `newest_of_kind` resolution in `fleet/copilot_configure`, so the two
        // never disagree about which channel is "the" copilot channel.
        //
        // Each step is announced BEFORE its call, not after: the point of the
        // announcement is the time spent inside the call, and a fresh install
        // can sit in the mint below for seconds while the pane, before this,
        // showed an ordinary empty composer.
        progress(ChatOpenStep::ListingChannels);
        let channels = client
            .channel_list()
            .await
            .map_err(|error| ChatPageFailure::new(ChatOpenStep::ListingChannels, error))?;
        let existing = channels
            .channels
            .into_iter()
            .filter(|channel| channel.kind == FleetChannelKind::Copilot)
            .filter(|channel| scope_key.as_ref().is_none_or(|wanted| &channel.scope_key == wanted))
            .next_back();
        let channel = match existing {
            Some(channel) => channel,
            None => {
                // Create-if-absent, on a read path, deliberately: the copilot
                // channel is a singleton an operator expects to simply exist,
                // and there is no other door to it in the TUI. A race creates a
                // duplicate at worst, and newest-wins keeps every reader on the
                // same one.
                progress(ChatOpenStep::CreatingChannel);
                client
                    .channel_create(FleetChannelCreateParams {
                        kind: FleetChannelKind::Copilot,
                        name: "copilot".to_string(),
                        recipients: None,
                    })
                    .await
                    .map_err(|error| ChatPageFailure::new(ChatOpenStep::CreatingChannel, error))?
                    .channel
            }
        };
        let scope = channel.scope_key.clone();

        // A COPILOT channel carries no recipient list: `fleet/channel_create`
        // rejects one, because its membership is the ACP session that ANSWERS
        // on the scope. So the recipient is resolved the way the contract says
        // it is minted, by creating that session against this scope. The call
        // is idempotent per live scope, so a poll returns the standing session
        // rather than minting a second one every second.
        //
        // The daemon's refusal is KEPT, not swallowed: its wording is the only
        // actionable thing an operator gets ("scope_key ... is already held by
        // a session whose cwd is X, not Y" is how a TUI launched from a
        // different directory than the one that first opened the chat reads).
        // Dropping it leaves the screen saying "no copilot session yet" forever
        // with the explanation discarded.
        progress(ChatOpenStep::CreatingSession);
        let cwd = std::env::current_dir()
            .map(|cwd| cwd.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let mint = |provider: Option<String>| {
            client.acp_session_create(ainb_hangar_proto::fleet::FleetAcpSessionCreateParams {
                provider,
                cwd: cwd.clone(),
                scope_key: Some(scope.clone()),
            })
        };
        // Deliberately unnamed: this call wants THE copilot session, not a
        // particular engine. Naming one reverted an adapter the operator had
        // swapped, and once the scope was held by that other adapter the mint
        // was refused outright, so opening the chat page after a swap failed
        // instead of attaching.
        let created = match mint(None).await {
            // A daemon older than this binary still REQUIRES `provider`, and an
            // absent one is a missing field it refuses. That is the ordinary
            // upgrade-the-binary-keep-the-daemon window, and leaving it would
            // make the copilot page unopenable across it. Retried once naming
            // the built-in adapter, which is exactly what this call sent before
            // the parameter became optional: no worse than it was, against a
            // daemon that cannot do better.
            Err(error) if names_the_provider_field(&error.to_string()) => {
                mint(Some(LEGACY_DAEMON_ADAPTER.to_string())).await
            }
            other => other,
        };
        let (target_session_key, session_detail, turn_deadline_ms) = match created {
            // The pool's turn ceiling rides back on the mint and is the only
            // door it has onto a client, so it is carried through to the pane
            // that has to bound a PENDING leg's wait.
            Ok(created) => (Some(created.session_key), None, created.turn_deadline_ms),
            // A channel that DOES carry members (a broadcast channel reusing
            // this page) keeps its first member as the recipient.
            Err(error) => (
                channel.recipients.first().cloned(),
                Some(error.to_string()),
                None,
            ),
        };

        progress(ChatOpenStep::LoadingMessages);
        let messages = client
            .message_list(FleetMessageListParams {
                scope_key: Some(scope.clone()),
                origin_id: None,
                after_id: None,
                limit: FLEET_MESSAGE_LIST_MAX,
            })
            .await
            .map_err(|error| ChatPageFailure::new(ChatOpenStep::LoadingMessages, error))?
            .messages;

        // The confirm and activity feeds are NOT fatal to the page: a daemon
        // built between phases answers -32601 here, and a chat that refuses to
        // render its timeline over that is a worse surface than one that says
        // which half is missing.
        let (confirms, confirms_detail) = match client
            .confirm_list_raw(FleetConfirmListParams {
                scope_key: Some(scope.clone()),
            })
            .await
        {
            Ok(confirms) => (confirms, None),
            Err(crate::fleet::bridge::daemon::DaemonError::Rpc { code, .. })
                if code == RPC_METHOD_NOT_FOUND =>
            {
                (
                    Vec::new(),
                    Some("not served by this daemon yet".to_string()),
                )
            }
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let activity = client
            .activity_list(FleetActivityListParams {
                scope_key: Some(scope.clone()),
                after_seq: None,
                limit: FLEET_ACTIVITY_LIST_MAX,
            })
            .await
            .map(|page| page.activities)
            .unwrap_or_default();

        Ok(ainb_plugin_hangar::screen::fleet_chat::ChatSnapshot {
            scope_key: Some(scope),
            target_session_key,
            messages,
            confirms,
            confirms_detail,
            session_detail,
            activity,
            turn_deadline_ms,
        })
    })
}

/// Cancel the ACP turns a chat pane is still waiting on, one `fleet/action`
/// each.
///
/// `fleet/action` is versioned, so this reads `fleet/snapshot` ONCE and takes
/// each target's current version from it rather than guessing. One snapshot
/// serves every leg because a fan-out's legs are cancelled in the same breath.
///
/// KNOWN GAP, and it is the reason the sentence above is weaker than it looks:
/// reading the version immediately before the write makes the version check
/// vacuous. Optimistic concurrency exists so an action computed against state X
/// fails once state has moved, and re-reading X here removes exactly that. So
/// nothing in this path can notice "the turn you were waiting on ended; this is
/// a different one", and the cancel lands on whatever the session is doing now.
///
/// The page bounds the exposure by retiring a leg past the turn deadline
/// (`ChatState::unresolved_legs`), so this is one deadline rather than the life
/// of the pane. Closing it needs the version the leg was CREATED at, carried
/// from the send and passed here instead of re-read — which means a field on
/// `FleetMessageDelivery` the daemon populates when it builds the leg.
///
/// Returns the sentence the pane prints. Partial success is reported as such:
/// cancelling three of four turns and saying "cancelled" is the same class of
/// lie as a fan-out that reports "sent to 4".
pub fn chat_cancel_turns_blocking(session_keys: Vec<String>) -> Result<String, String> {
    use ainb_hangar_proto::fleet::{ActionReceiptStatus, ControlAction, FleetActionParams};

    if session_keys.is_empty() {
        return Err("no turn to cancel".to_string());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        let snapshot = client.fleet_snapshot().await.map_err(|error| error.to_string())?;
        let mut cancelled = 0usize;
        let mut refusals: Vec<String> = Vec::new();
        for session_key in &session_keys {
            let Some(session) =
                snapshot.sessions.iter().find(|session| &session.session_key == session_key)
            else {
                // The daemon does not know this recipient at all. Named rather
                // than counted as a generic failure: a leg addressed to a
                // session the daemon has never heard of is a routing bug, not a
                // turn that refused to stop.
                refusals.push(format!("{session_key}: not in the daemon's snapshot"));
                continue;
            };
            let receipt = client
                .fleet_action(FleetActionParams {
                    session_key: session_key.clone(),
                    expected_version: session.version,
                    request_id: format!("fleet-chat-cancel-{}", uuid::Uuid::new_v4()),
                    action: ControlAction::Interrupt,
                })
                .await;
            match receipt {
                Ok(receipt) if receipt.status == ActionReceiptStatus::Delivered => cancelled += 1,
                Ok(receipt) => refusals.push(format!(
                    "{session_key}: {}",
                    receipt.detail.unwrap_or_else(|| {
                        ainb_hangar_proto::fleet::receipt_status_token(receipt.status).to_string()
                    })
                )),
                Err(error) => refusals.push(format!("{session_key}: {error}")),
            }
        }
        let total = session_keys.len();
        if refusals.is_empty() {
            return Ok(format!("cancelled {cancelled} of {total} turn(s)"));
        }
        Err(format!(
            "cancelled {cancelled} of {total} turn(s) · {}",
            refusals.join(" · ")
        ))
    })
}

/// Post one operator message into the copilot channel on a worker thread.
///
/// No `actor` rides this: an operator send omits the key, which is exactly what
/// the daemon defaults to. A copilot-authored write is the daemon's own MCP
/// path and never originates at a human's keyboard.
pub fn chat_send_blocking(
    params: ainb_hangar_proto::fleet::FleetMessageSendParams,
) -> Result<ainb_hangar_proto::fleet::FleetMessageSendResult, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client.message_send(params).await.map_err(|error| error.to_string())
    })
}

/// Answer one guardrail confirm card on a worker thread.
pub fn chat_confirm_answer_blocking(
    params: ainb_hangar_proto::fleet::FleetConfirmAnswerParams,
) -> Result<ainb_hangar_proto::fleet::FleetConfirmAnswerResult, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client.confirm_answer(params).await.map_err(|error| error.to_string())
    })
}

/// [`resolve_and_send`], with success and failure told apart.
///
/// The string form is what a status line wants; a surface that has to decide
/// whether a chip CLEARS or reverts to ASK needs the verdict, and inferring it
/// by matching on prose is how the two drift. `Ok` carries the delivery
/// description, `Err` the reason nothing was delivered.
async fn resolve_and_send_typed(
    session_id: &str,
    cwd: &str,
    text: &str,
    is_answer: bool,
) -> Result<String, String> {
    use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
    use crate::fleet::send::send;
    use crate::fleet::types::Session;

    // C4: real concurrency. `discover_from_ainb` is async; `discover_from_peers`
    // is a SYNC blocking SQLite read, so run it on a blocking thread instead of
    // a bare `async {}` block (which would run it inline on this thread — the
    // previous illusory-concurrency bug). Both then make progress in parallel.
    let ainb_fut = discover_from_ainb();
    let peers_fut = tokio::task::spawn_blocking(discover_from_peers);
    let (ainb, peers_join) = tokio::join!(ainb_fut, peers_fut);
    let ainb: Vec<Session> = ainb.unwrap_or_default();
    let peers: Vec<Session> = peers_join.ok().and_then(Result::ok).unwrap_or_default();

    // Count raw (pre-merge) sessions sharing the target cwd across BOTH sources,
    // so two distinct claude sessions in the same dir register as ambiguous even
    // though `merge_sessions` would coalesce them onto one cwd-keyed row.
    let raw_cwd_count = if cwd.is_empty() {
        0
    } else {
        ainb.iter().chain(peers.iter()).filter(|s| s.cwd == cwd).count()
    };

    let merged = merge_sessions(vec![ainb, peers]);

    // 1. Exact session-id match is always unambiguous — send it.
    if let Some(session) = merged.iter().find(|s| s.id == session_id) {
        return outcome_result(send(session, text).await);
    }

    // 2. No exact match → cwd correlation, guarded by ambiguity.
    let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
        return Err("no live session matched (target may have exited)".to_string());
    };

    // Ambiguous when >1 raw session shared the cwd, OR the merged session for
    // the cwd aggregated 2+ sources (we can't tell which underlying agent it is).
    let ambiguous = raw_cwd_count > 1 || by_cwd.sources.len() > 1;
    if ambiguous {
        let label = if is_answer {
            "cannot safely answer"
        } else {
            "refusing to send"
        };
        return Err(format!(
            "ambiguous target — {label} ({} sessions in this cwd)",
            raw_cwd_count.max(by_cwd.sources.len())
        ));
    }

    outcome_result(send(by_cwd, text).await)
}

/// Map a `send()` result to a verdict plus its description.
fn outcome_result(
    result: anyhow::Result<crate::fleet::types::SendOutcome>,
) -> Result<String, String> {
    use crate::fleet::types::SendOutcome;
    match result {
        Ok(SendOutcome::Tmux { tmux_session }) => Ok(format!("sent via tmux ({tmux_session})")),
        Ok(SendOutcome::Broker { peer_id }) => Ok(format!("sent via broker ({peer_id})")),
        // `Failed` is the send path reporting a dead pane or a stale tmux
        // identity: an ERROR, not a delivery. Folding it in with the successes
        // is how a chip clears on an answer that never arrived.
        Ok(SendOutcome::Failed { reason }) => Err(format!("not delivered: {reason}")),
        Err(e) => Err(format!("send error: {e}")),
    }
}

/// Deliver one answer into a session's own pane, blocking, with the C1
/// ambiguity guard.
///
/// The verified last-mile send for a row the daemon knows nothing about. This
/// is what keeps the sessions screen answerable with the hangar daemon stopped.
///
/// # Errors
///
/// Returns the reason nothing was delivered.
pub fn answer_via_tmux_blocking(session_id: &str, cwd: &str, text: &str) -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    // `is_answer` is always true here: this path exists only for answers, and
    // an answer routed to the wrong agent by a cwd guess is the failure the
    // guard exists to prevent.
    runtime.block_on(resolve_and_send_typed(session_id, cwd, text, true))
}

/// Answer one parked permission request through notifyd's approve broker.
///
/// The blocked hook is sitting in `client_await` on the approve socket; this
/// hands it the human's verdict, which flows back to the agent as its
/// permission decision. Local and independent of the hangar daemon, so it is
/// the route that keeps an APPROVE answerable with the daemon stopped.
///
/// # Errors
///
/// Returns the reason the decision was not delivered.
pub fn answer_via_broker_blocking(
    session_id: &str,
    approve: bool,
    reason: &str,
) -> Result<String, String> {
    use ainb_plugin_notifyd::broker::{DecisionKind, client_decide};

    let socket = ainb_plugin_notifyd::paths::Paths::from_home()
        .map_err(|error| format!("approve socket unavailable: {error}"))?
        .approve_socket;
    let kind = if approve {
        DecisionKind::Approve
    } else {
        DecisionKind::Deny
    };
    // Blocking `std::os::unix` I/O, no runtime needed.
    match client_decide(&socket, session_id, kind, reason) {
        Ok(true) => Ok(format!(
            "{} the waiting hook",
            if approve { "approved" } else { "denied" }
        )),
        // NOT an error: the request resolved some other way, or timed out. The
        // agent is no longer waiting, so the chip should clear.
        Ok(false) => Ok("no waiter left (already resolved or timed out)".to_string()),
        Err(error) => Err(format!("approve broker unreachable: {error}")),
    }
}

/// Answer one open attention row through the daemon, blocking.
///
/// The daemon runs first-answer-wins and performs its own verified last-mile
/// send, so this reports what it decided rather than deciding anything.
///
/// # Errors
///
/// Returns the reason the answer was not delivered.
pub fn answer_via_daemon_blocking(attention_id: String, answer: String) -> Result<String, String> {
    use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| format!("attention/answer unavailable: {error}"))?;
        let socket = client.socket().display().to_string();
        let result = client
            .answer(AnswerParams {
                attention_id,
                answer,
                answered_by: "tui".to_string(),
                is_answer: true,
            })
            .await
            .map_err(|error| format!("attention/answer via {socket}: {error}"))?;
        match result {
            AnswerResult::Delivered { via } => Ok(via),
            // Not a failure of THIS answer: another surface got there first and
            // the session already has its reply. The chip clears either way,
            // which is why it reads as delivered with a note rather than an
            // error the operator would retry.
            AnswerResult::AlreadyAnswered { by } => Ok(format!("already answered by {by}")),
            // Every remaining variant means the session did NOT get the answer,
            // so each carries its own reason and the chip must go back to ASK.
            // Matched exhaustively rather than debug-formatted: `{other:?}`
            // would put a Rust struct literal in front of the operator.
            AnswerResult::Ambiguous { reason } => Err(format!(
                "ambiguous target, refused rather than mis-routed: {reason}"
            )),
            AnswerResult::NoTarget { reason } => Err(format!("no live target: {reason}")),
            // The row IS answered (the race winner is recorded) but nothing
            // reached the agent. Reported as a failure because the agent is
            // still waiting, which is the fact the operator has to act on.
            AnswerResult::DeliveryFailed { reason } => {
                Err(format!("recorded, but the send did not land: {reason}"))
            }
        }
    })
}

/// The named BROADCAST channels, by name, in creation order.
///
/// Copilot channels are filtered out: there is exactly one and the pane the
/// operator is reading IS it, so listing it would offer them the conversation
/// they are already in.
pub fn broadcast_channels_blocking() -> Result<Vec<String>, String> {
    use ainb_hangar_proto::fleet::FleetChannelKind;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client
            .channel_list()
            .await
            .map(|result| {
                result
                    .channels
                    .into_iter()
                    .filter(|channel| channel.kind == FleetChannelKind::Broadcast)
                    .map(|channel| channel.name)
                    .collect()
            })
            .map_err(|error| error.to_string())
    })
}

/// One message to N sessions, with a receipt per recipient.
///
/// `fleet/broadcast`, not N `fleet/message_send` calls: the daemon fans out
/// under ONE idempotency key, so a retry cannot deliver a second copy to the
/// recipients the first attempt already reached.
pub fn broadcast_blocking(
    target_keys: Vec<String>,
    text: String,
    idempotency_key: String,
) -> Result<Vec<ainb_hangar_proto::fleet::FleetActionReceipt>, String> {
    use ainb_hangar_proto::fleet::FleetBroadcastParams;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client
            .fleet_broadcast(FleetBroadcastParams {
                target_keys,
                text,
                idempotency_key,
            })
            .await
            .map(|result| result.receipts)
            .map_err(|error| error.to_string())
    })
}

/// The ACP adapters the daemon's registry can spawn, in name order.
///
/// The engine picker's only source: a list compiled into the TUI would refuse
/// an adapter an operator has already put in `[acp.adapters]`.
pub fn adapter_list_blocking() -> Result<Vec<ainb_hangar_proto::fleet::FleetAdapter>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client
            .adapter_list()
            .await
            .map(|result| result.adapters)
            .map_err(|error| error.to_string())
    })
}

/// Write the copilot's engine, guardrail dial, model and reasoning.
///
/// A changed provider retires the running session and mints a new one on the
/// same channel, so the RESULT is what the header must believe, not the params:
/// the daemon may answer with a different session key than the caller held.
pub fn copilot_configure_blocking(
    params: ainb_hangar_proto::fleet::FleetCopilotConfigureParams,
) -> Result<ainb_hangar_proto::fleet::FleetCopilotConfigureResult, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client.copilot_configure(params).await.map_err(|error| error.to_string())
    })
}

/// The adapter named when, and ONLY when, the daemon is too old to accept an
/// absent one.
///
/// `ainb_acp::config::CLAUDE_ADAPTER` by value, because `ainb-core` does not
/// depend on `ainb-acp` and one string is not worth a crate edge. It is a
/// built-in the pool always seeds, and the daemon re-validates the name
/// regardless, so a drift here fails closed rather than spawning something
/// arbitrary.
const LEGACY_DAEMON_ADAPTER: &str = "claude-agent-acp";

/// Whether a daemon refusal is the older daemon rejecting an ABSENT `provider`.
///
/// Matched on the wording because that is all a JSON-RPC `invalid_params`
/// carries. Deliberately narrow: it gates one retry that is harmless when the
/// guess is wrong, and a new daemon never produces this message at all, having
/// made the field optional.
fn names_the_provider_field(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    // `missing` only. The field is `skip_serializing_if = "Option::is_none"`,
    // so an omitted provider is absent from the frame rather than `null`, and
    // an older daemon reports it as a MISSING field. An `invalid type` arm was
    // unreachable from this call site and could only have masked a genuine
    // client bug — a caller sending a number — by retrying it against the
    // built-in adapter.
    error.contains("provider") && error.contains("missing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::{Session, SessionSource};

    /// The retry fires for an older daemon's refusal of an absent `provider`,
    /// and for nothing else — an unknown-adapter refusal names the provider too
    /// and must NOT be retried with a different one.
    #[test]
    fn only_a_missing_provider_field_triggers_the_legacy_retry() {
        assert!(names_the_provider_field(
            "invalid params: missing field `provider`"
        ));
        assert!(
            !names_the_provider_field(
                "invalid params: invalid type: number, expected a string at provider"
            ),
            "a caller sending the wrong type is a client bug, not a daemon skew, \
             and must not be retried against a different adapter"
        );
        assert!(
            !names_the_provider_field(
                "unknown adapter \"gemini-acp\"; fleet/adapter_list names the ones this daemon can spawn"
            ),
            "an adapter the daemon does not know is a real refusal, not a skew"
        );
        assert!(
            !names_the_provider_field(
                "scope_key \"channel:c1\" is already held by a session whose provider is codex-acp"
            ),
            "a held scope must not be retried with the built-in adapter"
        );
    }

    fn seed_git_repository(path: &Path) -> (git2::Repository, git2::Oid) {
        std::fs::create_dir_all(path).expect("create repository directory");
        let repository = git2::Repository::init(path).expect("initialize repository");
        std::fs::write(path.join("README.md"), "seed\n").expect("write seed file");
        let mut index = repository.index().expect("open index");
        index.add_path(Path::new("README.md")).expect("stage seed file");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repository.find_tree(tree_id).expect("find tree");
        let signature =
            git2::Signature::now("Fleet Test", "fleet@example.invalid").expect("create signature");
        let commit = repository
            .commit(Some("HEAD"), &signature, &signature, "seed", &tree, &[])
            .expect("create seed commit");
        drop(tree);
        (repository, commit)
    }

    fn session(id: &str, cwd: &str, src: SessionSource) -> Session {
        Session {
            id: id.to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some("tmux".to_string()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![src],
            summary: None,
            last_seen_ms: None,
        }
    }

    /// The C1 resolution decision, isolated from the actual `send()` I/O so it is
    /// deterministically unit-testable: given the discovered roster, what target
    /// does an answer resolve to — or does it refuse?
    #[derive(Debug, PartialEq)]
    enum Target {
        Exact(String),
        Cwd(String),
        Refuse,
        NoMatch,
    }

    /// Mirror of `resolve_and_send`'s resolution logic (steps 1+2 + the C1
    /// ambiguity guard) WITHOUT the send. `raw` is the pre-merge roster (both
    /// sources concatenated); `merged` is `merge_sessions` applied to it.
    fn resolve_target(raw: &[Session], session_id: &str, cwd: &str) -> Target {
        let merged = crate::fleet::discover::merge_sessions(vec![raw.to_vec()]);
        if let Some(s) = merged.iter().find(|s| s.id == session_id) {
            return Target::Exact(s.id.clone());
        }
        let raw_cwd_count = if cwd.is_empty() {
            0
        } else {
            raw.iter().filter(|s| s.cwd == cwd).count()
        };
        let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
            return Target::NoMatch;
        };
        if raw_cwd_count > 1 || by_cwd.sources.len() > 1 {
            return Target::Refuse;
        }
        Target::Cwd(by_cwd.id.clone())
    }

    #[test]
    fn exact_session_id_match_sends() {
        // The hook session id exactly matches a discovered session → send to it,
        // even if another session shares the cwd.
        let raw = vec![
            session("hook-sid", "/work/x", SessionSource::Ainb),
            session("other", "/work/x", SessionSource::Peers),
        ];
        // (These two share /work/x, so merge would coalesce — but the exact id
        // wins before any cwd logic.)
        assert_eq!(
            resolve_target(&raw, "hook-sid", "/work/x"),
            Target::Exact("hook-sid".to_string())
        );
    }

    #[test]
    fn unambiguous_cwd_match_sends() {
        // No exact id match, exactly one session in the cwd, single source → safe
        // to correlate by cwd.
        let raw = vec![session("discovered-id", "/work/x", SessionSource::Ainb)];
        assert_eq!(
            resolve_target(&raw, "hook-session-differs", "/work/x"),
            Target::Cwd("discovered-id".to_string())
        );
    }

    #[test]
    fn ambiguous_cwd_two_raw_sessions_refuses() {
        // Two DISTINCT sessions share the cwd (different ids, both pre-merge).
        // `merge_sessions` coalesces them onto one cwd row, but the raw count is
        // 2 → ambiguous → refuse rather than answer the wrong agent.
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("b", "/work/x", SessionSource::Ainb),
        ];
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::Refuse);
    }

    #[test]
    fn ambiguous_cwd_multi_source_merge_refuses() {
        // One cwd, but the merged session aggregated 2+ sources (ainb + peers) —
        // we can't tell which underlying agent it is → refuse.
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("a", "/work/x", SessionSource::Peers),
        ];
        // raw count is 2 here too, but the multi-source guard is the independent
        // signal; assert refuse.
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::Refuse);
    }

    #[test]
    fn no_session_in_cwd_is_no_match_not_refuse() {
        let raw = vec![session("a", "/other", SessionSource::Ainb)];
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::NoMatch);
    }

    #[test]
    fn empty_cwd_without_exact_match_is_no_match() {
        // An empty cwd never correlates (it would collapse distinct sessions).
        let raw = vec![session("a", "", SessionSource::Ainb)];
        assert_eq!(resolve_target(&raw, "hook-sid", ""), Target::NoMatch);
    }
}
