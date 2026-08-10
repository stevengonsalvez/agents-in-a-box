//! Tool execution: every tool is a call BACK into the daemon over `hangar.sock`.
//!
//! Nothing here touches tmux, the store, or a provider process. A copilot write
//! is the same `fleet/message_send` / `attention/answer` an operator's CLI
//! issues, through the same client (`ainb-hangar-client`), so it inherits the
//! receipts, the FIFO ordering, the idempotency and the delivery states for
//! free — and a bug fixed on the human path is fixed here by construction.
//!
//! Read results come back inside [`crate::envelope`]. Daemon-authored metadata
//! (ids, counts, cursors, delivery states) rides alongside as structured JSON,
//! which is what the copilot should branch on; the fenced text is for reading,
//! never for obeying.

use std::time::Duration;

use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_proto::events::AttentionRow;
use ainb_hangar_proto::fleet::{
    FLEET_CONFIRM_TTL_MS, FLEET_TRANSCRIPT_LIST_MAX, FleetCopilotGateParams,
    FleetCopilotGateResult, FleetMessageSendParams, FleetTranscriptListParams,
};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::reprime::{REPRIME_ROWS, rows_that_fit};
use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use serde_json::{Map, Value, json};

use crate::envelope::{observed, row};

/// The actor recorded on every write this server performs.
///
/// One constant, because "who did this" must never be MODEL-supplied: it is not
/// a tool argument, so nothing the copilot reads can change it. It reaches every
/// surface a human or an agent actually looks at:
///
/// * `attention/answer` → `answered_by`, and
/// * `fleet/message_send` → [`FleetMessage::sender`], which is what the chat UIs
///   render and what the recipient's re-prime corpus attributes the message to.
///
/// Without the second one a copilot steered by an injected transcript could ask
/// another agent to act while wearing the operator's name.
///
/// [`FleetMessage::sender`]: ainb_hangar_proto::fleet::FleetMessage::sender
pub const ACTOR: &str = "copilot";

/// One successful tool call: fenced text for the model, structured metadata for
/// its control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// What the copilot reads (already enveloped when it carries fleet text).
    pub text: String,
    /// Daemon-authored facts: ids, counts, cursors, delivery states.
    pub structured: Value,
}

/// A typed tool failure. The copilot branches on `kind`; the message is for the
/// human reading the activity feed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolFailure {
    /// The named session has no open need to answer.
    #[error("session `{session}` has no open need")]
    NoOpenNeed {
        /// The session that was asked.
        session: String,
    },
    /// More than one open need matched: answering "the" need would be a guess.
    #[error("session `{session}` has {} open needs; answer one explicitly", .need_ids.len())]
    AmbiguousNeed {
        /// The session that was asked.
        session: String,
        /// The candidate attention ids.
        need_ids: Vec<String>,
    },
    /// Another surface answered first (first-answer-wins).
    #[error("already answered by {by}")]
    AlreadyAnswered {
        /// The surface that won.
        by: String,
    },
    /// The daemon refused to route the answer (C1 ambiguity guard, dead target,
    /// failed last-mile send).
    #[error("{reason}")]
    NotDelivered {
        /// Which of the daemon's honest outcomes this was.
        outcome: &'static str,
        /// The daemon's explanation.
        reason: String,
    },
    /// The arguments the GATE returned do not satisfy the tool's shape.
    ///
    /// Reachable through the `edit` answer: the card's own arguments were
    /// classified on the way in, but an operator who edits the buffer can drop a
    /// required key. Failing here beats substituting a default, which for
    /// `answer_need` would resolve another agent's need with an empty answer.
    #[error("`{tool}`: {detail}")]
    BadArguments {
        /// The tool that could not run.
        tool: String,
        /// Which argument was missing or malformed.
        detail: String,
    },
    /// The tool exists and was allowed, but its execution arm ships with the
    /// confirm card in A2 (`spawn_session`, `interrupt`, `kill`, `archive`).
    /// Only reachable through an operator override.
    #[error("`{tool}` has no execution path yet")]
    NotWired {
        /// The tool that was allowed but cannot run.
        tool: String,
    },
    /// The daemon could not be reached or refused the call.
    ///
    /// `code` is the daemon's own JSON-RPC code when it answered (`-32602` for
    /// an unknown or malformed session, `-32601` for a method this daemon build
    /// does not serve, `-32000` for auth), and `None` when the failure was
    /// transport-level. It is carried so the copilot can tell "your argument was
    /// wrong" from "the daemon is down" without parsing prose.
    #[error("daemon: {detail}")]
    Daemon {
        /// The error text.
        detail: String,
        /// Whether retrying the same call can plausibly succeed.
        retryable: bool,
        /// The daemon's JSON-RPC error code, when it answered at all.
        code: Option<i32>,
    },
}

impl ToolFailure {
    /// The stable wire token the copilot branches on.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NoOpenNeed { .. } => "no_open_need",
            Self::AmbiguousNeed { .. } => "ambiguous_need",
            Self::AlreadyAnswered { .. } => "already_answered",
            Self::NotDelivered { .. } => "not_delivered",
            Self::BadArguments { .. } => "bad_arguments",
            Self::NotWired { .. } => "not_wired",
            Self::Daemon { .. } => "daemon",
        }
    }

    /// Whether the same call can be retried.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Daemon { retryable, .. } => *retryable,
            _ => false,
        }
    }

    /// The structured payload attached to the failing tool result.
    #[must_use]
    pub fn structured(&self) -> Value {
        let mut error = json!({
            "kind": self.kind(),
            "message": self.to_string(),
            "retryable": self.retryable(),
        });
        match self {
            Self::AmbiguousNeed { need_ids, .. } => error["need_ids"] = json!(need_ids),
            Self::Daemon {
                code: Some(code), ..
            } => error["code"] = json!(code),
            _ => {}
        }
        json!({ "error": error })
    }
}

impl From<DaemonError> for ToolFailure {
    fn from(error: DaemonError) -> Self {
        let detail = error.to_string();
        // Same taxonomy the CLI uses (`cli/fleet/msg.rs`): transport trouble is
        // retryable, a rejected or malformed call is not.
        let retryable = matches!(
            error,
            DaemonError::Connect { .. } | DaemonError::Io(_) | DaemonError::Timeout(_)
        );
        let code = match error {
            DaemonError::Rpc { code, .. } => Some(code),
            _ => None,
        };
        Self::Daemon {
            detail,
            retryable,
            code,
        }
    }
}

/// The daemon-backed implementation of the tool table.
#[derive(Debug, Clone)]
pub struct FleetTools {
    client: DaemonClient,
}

/// How long a gate call waits before it gives up on the daemon.
///
/// Deliberately OUTSIDE the daemon's own card lifetime, computed from the same
/// shared constant rather than restated: the daemon expires an unanswered card
/// and answers `expired`, so anything this bound catches is a daemon that died
/// holding the card, never a card an operator is still reading. A shorter bound
/// would surface a live dialog as a retryable transport failure, and the retry
/// would mint a second card for the same action.
pub const GATE_TIMEOUT: Duration =
    Duration::from_millis(FLEET_CONFIRM_TTL_MS.saturating_add(60_000));

impl FleetTools {
    /// Wrap an authenticated daemon client.
    #[must_use]
    pub const fn new(client: DaemonClient) -> Self {
        Self { client }
    }

    /// Offer one tool call to the daemon's guardrail and wait for its verdict.
    ///
    /// The ONE classification in the system. This process deliberately does not
    /// pre-classify: a second copy of the rules here would be a second thing to
    /// keep in step with the daemon's, and the copy that is easiest to soften is
    /// the one running downstream of every transcript the copilot has read.
    ///
    /// Blocks for as long as the operator's confirm card is open. A failure here
    /// fails CLOSED at the call site, because a verdict that never arrived is
    /// not an approval.
    pub async fn gate(
        &self,
        tool: &str,
        arguments: &Map<String, Value>,
    ) -> Result<FleetCopilotGateResult, ToolFailure> {
        Ok(self
            .client
            .call_typed_within(
                methods::FLEET_COPILOT_GATE,
                &FleetCopilotGateParams {
                    tool: tool.to_string(),
                    arguments: arguments.clone(),
                },
                GATE_TIMEOUT,
            )
            .await?)
    }

    /// `fleet_status`: the authoritative fleet snapshot, one record per session.
    pub async fn fleet_status(&self) -> Result<ToolOutcome, ToolFailure> {
        let snapshot = self.client.fleet_snapshot().await?;
        let rows: Vec<_> = snapshot
            .sessions
            .iter()
            .map(|session| {
                row(
                    session.session_key.clone(),
                    "fleet.session",
                    json!({
                        "session_key": session.session_key,
                        "provider": session.provider,
                        "lifecycle": session.lifecycle,
                        "attention": session.attention,
                        "cwd": session.cwd,
                        "display_name": session.display_name,
                        "version": session.version,
                    })
                    .to_string(),
                )
            })
            .collect();
        let (text, shown) = observed("fleet_status", &rows);
        // The keys the copilot can act on are the ones it was shown; naming a
        // session whose record the fence dropped would invite a call about a
        // session it never read.
        let keys: Vec<&String> = snapshot.sessions[snapshot.sessions.len() - shown..]
            .iter()
            .map(|session| &session.session_key)
            .collect();
        Ok(ToolOutcome {
            text,
            structured: json!({
                "head_revision": snapshot.head_revision,
                "session_count": shown,
                "fleet_session_count": snapshot.sessions.len(),
                "session_keys": keys,
            }),
        })
    }

    /// `session_needs`: the OPEN attention inbox, optionally one session's.
    pub async fn session_needs(&self, session: Option<&str>) -> Result<ToolOutcome, ToolFailure> {
        let needs = self.open_needs(session).await?;
        let rows: Vec<_> = needs
            .iter()
            .map(|need| {
                row(
                    need.session_id.clone(),
                    "fleet.need",
                    json!({
                        "need_id": need.id,
                        "session_id": need.session_id,
                        "kind": need.kind,
                        "cwd": need.cwd,
                        "degraded": need.degraded,
                        "created_at": need.created_at,
                        "payload": need.payload,
                    })
                    .to_string(),
                )
            })
            .collect();
        let (text, shown) = observed("session_needs", &rows);
        let ids: Vec<&String> = needs[needs.len() - shown..].iter().map(|need| &need.id).collect();
        Ok(ToolOutcome {
            text,
            structured: json!({
                "count": shown,
                "open_need_count": needs.len(),
                "need_ids": ids,
            }),
        })
    }

    /// `session_transcript`: one page of a session's ACP transcript.
    ///
    /// Pages FORWARD from `after_order` (the daemon's commit-ordered cursor),
    /// exactly like `ainb fleet transcript`. The page is capped at the envelope's
    /// row budget so nothing is fetched only to be dropped, and the next cursor
    /// comes back as structured data.
    ///
    /// The row cap is not the only one: the fence also has a BYTE budget, and a
    /// forward reader must drop from the END of the page rather than the front,
    /// because the cursor below moves past whatever it reports and never comes
    /// back. So the page is trimmed to the leading rows that fit and the cursor
    /// is clamped to the last of them — the remainder arrives on the next call
    /// instead of vanishing.
    pub async fn session_transcript(
        &self,
        session: &str,
        after_order: Option<i64>,
    ) -> Result<ToolOutcome, ToolFailure> {
        let limit = u32::try_from(REPRIME_ROWS).unwrap_or(FLEET_TRANSCRIPT_LIST_MAX);
        let page = self
            .client
            .transcript_list(FleetTranscriptListParams {
                session_key: session.to_string(),
                after_order,
                limit: limit.clamp(1, FLEET_TRANSCRIPT_LIST_MAX),
            })
            .await?;
        let rows: Vec<_> = page
            .chunks
            .iter()
            .map(|chunk| {
                row(
                    chunk.session_key.clone(),
                    "fleet.transcript",
                    json!({
                        "ingest_order": chunk.ingest_order,
                        "event_type": chunk.event_type,
                        "payload": chunk.payload,
                    })
                    .to_string(),
                )
            })
            .collect();
        let fits = rows_that_fit(rows.iter());
        let (text, shown) = observed("session_transcript", &rows[..fits]);
        let next_after_order = if shown == page.chunks.len() {
            page.next_after_order
        } else {
            // Resume AT the first chunk that did not fit. `shown == 0` can only
            // happen on an empty page (a single row is clamped well under the
            // budget), and leaving the caller's own cursor alone is the honest
            // answer if it ever did.
            shown
                .checked_sub(1)
                .and_then(|last| page.chunks.get(last))
                .map_or(after_order, |chunk| Some(chunk.ingest_order))
        };
        Ok(ToolOutcome {
            text,
            structured: json!({
                "session": session,
                "chunk_count": shown,
                "next_after_order": next_after_order,
            }),
        })
    }

    /// `send_prompt`: one chat-bus message to one session.
    pub async fn send_prompt(&self, session: &str, text: &str) -> Result<ToolOutcome, ToolFailure> {
        self.deliver(&[session.to_string()], text, "send_prompt").await
    }

    /// `broadcast`: ONE chat-bus message to several sessions.
    ///
    /// Deliberately a single `fleet/message_send` rather than N sends: part 1
    /// mints one `broadcast:<ulid>` scope for the fan-out, so the recipients
    /// share an origin and the receipts stay joinable.
    pub async fn broadcast(
        &self,
        sessions: &[String],
        text: &str,
    ) -> Result<ToolOutcome, ToolFailure> {
        self.deliver(sessions, text, "broadcast").await
    }

    /// `answer_need`: resolve one open need inside another agent's session.
    ///
    /// Resolution fails CLOSED. Zero matching needs is an error and so is more
    /// than one: picking "the newest" would let a session that raises needs in a
    /// burst decide which one the copilot answers.
    pub async fn answer_need(
        &self,
        session: &str,
        answer: &str,
    ) -> Result<ToolOutcome, ToolFailure> {
        let needs = self.open_needs(Some(session)).await?;
        let need = match needs.as_slice() {
            [] => {
                return Err(ToolFailure::NoOpenNeed {
                    session: session.to_string(),
                });
            }
            [only] => only,
            many => {
                return Err(ToolFailure::AmbiguousNeed {
                    session: session.to_string(),
                    need_ids: many.iter().map(|need| need.id.clone()).collect(),
                });
            }
        };

        let outcome = self
            .client
            .answer(AnswerParams {
                attention_id: need.id.clone(),
                answer: answer.to_string(),
                answered_by: ACTOR.to_string(),
                // The safety-critical route: on an ambiguous target the daemon
                // REFUSES rather than guessing which session gets the answer.
                is_answer: true,
            })
            .await?;

        match outcome {
            AnswerResult::Delivered { via } => Ok(ToolOutcome {
                text: format!("answered need {} on {session} via {via}", need.id),
                structured: json!({
                    "need_id": need.id,
                    "session": session,
                    "outcome": "delivered",
                    "via": via,
                }),
            }),
            AnswerResult::AlreadyAnswered { by } => Err(ToolFailure::AlreadyAnswered { by }),
            AnswerResult::Ambiguous { reason } => Err(ToolFailure::NotDelivered {
                outcome: "ambiguous",
                reason,
            }),
            AnswerResult::NoTarget { reason } => Err(ToolFailure::NotDelivered {
                outcome: "no_target",
                reason,
            }),
            AnswerResult::DeliveryFailed { reason } => Err(ToolFailure::NotDelivered {
                outcome: "delivery_failed",
                reason,
            }),
        }
    }

    /// The open attention inbox, optionally narrowed to one session.
    async fn open_needs(&self, session: Option<&str>) -> Result<Vec<AttentionRow>, ToolFailure> {
        let mut needs = self.client.attention_list_fleet().await?;
        if let Some(session) = session {
            needs.retain(|need| need.session_id == session);
        }
        Ok(needs)
    }

    async fn deliver(
        &self,
        targets: &[String],
        text: &str,
        tool: &'static str,
    ) -> Result<ToolOutcome, ToolFailure> {
        // A fresh ULID per call: a retried tool call is a NEW message, never a
        // surprise idempotent replay of the previous one (the CLI's rule).
        let request_id = format!("{ACTOR}:{}", IdGen::new_ulid(&SystemIdGen));
        let result = self
            .client
            .message_send(FleetMessageSendParams {
                // No supplied scope: the daemon mints `session:<key>` for one
                // recipient and `broadcast:<ulid>` for many, and part 1 requires
                // a supplied scope to name a recipient of its own send.
                scope_key: None,
                // The whole point of the constant: what the recipient's corpus
                // and the chat UIs render as the sender is `copilot`, never the
                // operator whose turn happened to trigger this call.
                actor: Some(ACTOR.to_string()),
                targets: targets.to_vec(),
                // Unthreaded: a copilot prompt opens a conversation rather than
                // answering one. The agent's reply is what carries the origin,
                // and the daemon sets that itself at turn end.
                origin_message_id: None,
                text: text.to_string(),
                request_id: request_id.clone(),
            })
            .await?;
        let deliveries: Vec<Value> = result
            .deliveries
            .iter()
            .map(|delivery| json!({ "session": delivery.session_key, "state": delivery.state }))
            .collect();
        Ok(ToolOutcome {
            text: format!(
                "{tool}: message {} to {} recipient(s)",
                result.message_id,
                result.deliveries.len()
            ),
            structured: json!({
                "message_id": result.message_id,
                "request_id": request_id,
                "deliveries": deliveries,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_carry_a_stable_kind_and_retryability() {
        let dead = ToolFailure::NoOpenNeed {
            session: "claude:one".to_string(),
        };
        assert_eq!(dead.structured()["error"]["kind"], "no_open_need");
        assert_eq!(dead.structured()["error"]["retryable"], false);

        let down = ToolFailure::from(DaemonError::Io("broken pipe".to_string()));
        assert_eq!(down.kind(), "daemon");
        assert!(down.retryable(), "transport trouble is retryable");

        let refused = ToolFailure::from(DaemonError::Rpc {
            code: -32602,
            message: "targets must name at least one session".to_string(),
        });
        assert!(!refused.retryable(), "a rejected call is not retryable");
        assert_eq!(
            refused.structured()["error"]["code"],
            -32602,
            "a bad argument is distinguishable from a dead daemon: {refused:?}"
        );
    }

    #[test]
    fn an_ambiguous_need_lists_the_candidates() {
        let failure = ToolFailure::AmbiguousNeed {
            session: "claude:one".to_string(),
            need_ids: vec!["01A".to_string(), "01B".to_string()],
        };
        assert_eq!(
            failure.structured()["error"]["need_ids"],
            json!(["01A", "01B"])
        );
        assert!(failure.to_string().contains("2 open needs"));
    }
}
