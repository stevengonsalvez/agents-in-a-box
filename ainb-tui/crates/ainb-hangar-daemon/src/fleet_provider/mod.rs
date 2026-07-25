//! Provider control adapters for Fleet-managed sessions.
//!
//! This module keeps provider wire details below the Fleet reducer and RPC
//! surface. Callers pass stable request identity and session versions. Each
//! adapter then chooses an authoritative provider control path or a narrowly
//! verified degraded fallback.

pub mod claude;
pub mod codex;
pub mod codex_manager;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Provider adapter failure with enough structure for an honest action receipt.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Local transport failed.
    #[error("provider transport failed: {0}")]
    Transport(String),
    /// Provider returned a response that violates its negotiated protocol.
    #[error("provider protocol error: {0}")]
    Protocol(String),
    /// Requested action is not available through the negotiated capabilities.
    #[error("provider capability unavailable: {0}")]
    Unsupported(String),
    /// Request identity or version no longer matches current provider state.
    #[error("stale provider request: {0}")]
    Stale(String),
    /// IO failure while starting or talking to a provider transport.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON failure while talking to a schema-compatible provider transport.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Stable identity shared by every provider action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// Fleet registry key, never cwd-derived.
    pub session_key: String,
    /// Provider-native session or thread identifier when known.
    pub provider_session_id: Option<String>,
    /// Optimistic concurrency version from the authoritative Fleet registry.
    pub expected_version: i64,
}

/// Opaque fingerprint over exact provider request identity and ordered payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestFingerprint(pub String);

/// One selectable answer shown by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    /// Provider-visible option label.
    pub label: String,
    /// Provider-visible option explanation.
    pub description: String,
}

/// One complete structured question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredQuestion {
    /// Provider-native question identifier.
    pub id: String,
    /// Short display header.
    pub header: String,
    /// Full question text.
    pub question: String,
    /// Ordered options. Empty means free-form input.
    pub options: Vec<QuestionOption>,
    /// Whether provider accepts more than one selected option.
    #[serde(default)]
    pub multi_select: bool,
    /// Whether provider offers a free-form alternative.
    #[serde(default)]
    pub is_other: bool,
    /// Whether rendered input must be concealed.
    #[serde(default)]
    pub is_secret: bool,
}

/// Answers for one provider-native question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswer {
    /// Provider-native question identifier.
    pub question_id: String,
    /// Selected labels or free-form answer values.
    pub answers: Vec<String>,
}

/// Provider-independent approval decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    /// Approve once.
    Approve,
    /// Approve and cache for current provider session when supported.
    ApproveForSession,
    /// Deny while allowing current turn to continue.
    Deny,
    /// Deny and interrupt current turn when supported.
    DenyAndInterrupt,
}

/// Result returned by a provider execution adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReceipt {
    /// Whether provider acknowledged action authoritatively.
    pub authoritative: bool,
    /// Human-readable transport used for diagnostics.
    pub transport: &'static str,
}

/// Convert authoritative Fleet registry identity into provider transport identity.
pub fn session_identity_from_core(
    session: &ainb_fleet_core::types::FleetSession,
) -> Result<SessionIdentity, ProviderError> {
    let expected_version = i64::try_from(session.version).map_err(|_| {
        ProviderError::Protocol("Fleet session version exceeds provider transport range".into())
    })?;
    Ok(SessionIdentity {
        session_key: session.session_key.as_str().to_owned(),
        provider_session_id: session.provider_session_id.clone(),
        expected_version,
    })
}

/// Convert provider-neutral answers without flattening multi-select values.
pub fn question_answers_from_core(
    answers: &[ainb_fleet_core::fleet::provider::QuestionAnswer],
) -> Vec<QuestionAnswer> {
    answers
        .iter()
        .map(|answer| {
            let mut values = answer.selected_options.clone();
            if let Some(text) = &answer.text {
                values.push(text.clone());
            }
            QuestionAnswer {
                question_id: answer.question_id.clone(),
                answers: values,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ainb_fleet_core::fleet::provider::QuestionAnswer as CoreQuestionAnswer;
    use ainb_fleet_core::types::{
        AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
        Provenance, Provider, SessionKey, TransportHealth,
    };

    use super::*;

    #[test]
    fn core_conversion_preserves_stable_key_version_and_multiselect() {
        let session = FleetSession {
            session_key: SessionKey::managed(Provider::Claude, "session-1"),
            provider: Provider::Claude,
            provider_session_id: Some("session-1".into()),
            cwd: "/repo".into(),
            exact_tmux_target: None,
            pane_pid: None,
            process_start_fingerprint: None,
            lifecycle: LifecycleState::Idle,
            attention: AttentionState::Ask,
            management: ManagementState::Managed,
            capabilities: Capabilities::default(),
            provenance: BTreeSet::from([Provenance::Hook]),
            confidence: Confidence::Authoritative,
            transport_health: TransportHealth::Healthy,
            first_seen_ms: Some(1),
            last_seen_ms: Some(2),
            version: 9,
        };
        let identity = session_identity_from_core(&session).expect("session identity");
        assert_eq!(identity.session_key, "claude:session-1");
        assert_eq!(identity.expected_version, 9);

        let answers = question_answers_from_core(&[CoreQuestionAnswer {
            question_id: "tools".into(),
            selected_options: vec!["rg".into(), "ast-grep".into()],
            text: Some("custom".into()),
        }]);
        assert_eq!(answers[0].answers, vec!["rg", "ast-grep", "custom"]);
    }
}
