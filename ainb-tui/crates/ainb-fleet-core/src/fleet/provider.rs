// ABOUTME: Provider-neutral Fleet event normalization and control contract.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::fleet::types::{
    AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, Provenance, Provider,
    SessionKey,
};

/// Raw semantic event supplied by a provider transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEvent {
    pub event_id: String,
    pub provider_session_id: String,
    pub event_type: String,
    pub at_ms: i64,
    pub payload: serde_json::Value,
}

/// Provider request identity required for an exact structured response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredRequestIdentity {
    /// Exact provider RPC request id. Required for Codex app-server responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub item_id: String,
    pub question_ids: Vec<String>,
    pub fingerprint: String,
}

/// One structured answer. Multiple selected options preserve multi-select input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub selected_options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Action requested through a provider adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlAction {
    StructuredAnswer {
        request: StructuredRequestIdentity,
        answers: Vec<QuestionAnswer>,
    },
    Approve {
        request_id: String,
    },
    Deny {
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    SendPrompt {
        text: String,
    },
    Continue,
    Retry,
    Interrupt,
    Start,
    Restart,
    Stop,
    Kill,
    Archive,
}

/// Provider-normalized event consumed by a Fleet reducer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub event_id: String,
    pub session_key: SessionKey,
    pub provider: Provider,
    pub at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<AttentionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<StructuredRequestIdentity>,
    pub payload: serde_json::Value,
    pub provenance: Provenance,
    pub confidence: Confidence,
}

/// Pane and transcript evidence used when semantic provider events are absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackObservation {
    pub session: FleetSession,
    pub observed_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_event: Option<serde_json::Value>,
}

/// Result of a provider action attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub request_id: String,
    pub status: DeliveryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    Delivered,
    Failed,
    Unknown,
}

/// Adapter failure before an honest action receipt can be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdapterError {
    message: String,
}

impl ProviderAdapterError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProviderAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderAdapterError {}

pub type AdapterResult<T> = Result<T, ProviderAdapterError>;
pub type ActionFuture<'a> = Pin<Box<dyn Future<Output = AdapterResult<ActionReceipt>> + Send + 'a>>;

/// Provider boundary for normalization, fallback observation, and control.
pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> Provider;

    fn normalize_event(&self, event: ProviderEvent) -> AdapterResult<NormalizedEvent>;

    fn observe_fallback(
        &self,
        observation: FallbackObservation,
    ) -> AdapterResult<Vec<NormalizedEvent>>;

    fn capabilities(&self, management: crate::fleet::types::ManagementState) -> Capabilities;

    fn execute<'a>(
        &'a self,
        session: &'a FleetSession,
        action: &'a ControlAction,
    ) -> ActionFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_answer_preserves_question_ids_and_multi_select() {
        let action = ControlAction::StructuredAnswer {
            request: StructuredRequestIdentity {
                request_id: Some(serde_json::json!(41)),
                thread_id: Some("thread-1".into()),
                turn_id: Some("turn-2".into()),
                item_id: "item-3".into(),
                question_ids: vec!["q1".into(), "q2".into()],
                fingerprint: "fp-4".into(),
            },
            answers: vec![QuestionAnswer {
                question_id: "q2".into(),
                selected_options: vec!["alpha".into(), "beta".into()],
                text: None,
            }],
        };

        let encoded = serde_json::to_value(action).expect("serialize action");
        assert_eq!(encoded["request"]["item_id"], "item-3");
        assert_eq!(encoded["request"]["request_id"], 41);
        assert_eq!(encoded["answers"][0]["selected_options"][1], "beta");
    }
}
