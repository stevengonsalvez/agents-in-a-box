//! The wire envelope shared between hook scripts and the daemon.
//!
//! See `.agents/specs/2026-05-27-ainb-hooks-plugin-stub-spec.md`
//! "Hook script JSON payload shape" for the canonical definition.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current envelope wire version. Bumped if and only if a breaking
/// change to the on-the-wire shape is shipped.
pub const PROTOCOL_VERSION: u8 = 1;

/// Errors produced while parsing an envelope from JSON bytes.
#[derive(Debug, Error)]
pub enum EnvelopeError {
    /// The bytes were not valid UTF-8 JSON.
    #[error("malformed json: {0}")]
    Json(#[from] serde_json::Error),
    /// The envelope declared a protocol version we do not support.
    #[error("unsupported protocol_version {got}, expected {expected}")]
    UnsupportedVersion {
        /// Version we saw on the wire.
        got: u8,
        /// Version we support.
        expected: u8,
    },
    /// A required field was empty.
    #[error("missing field {0}")]
    MissingField(&'static str),
}

/// A normalized hook-event envelope.
///
/// Identical fields to the JSON wire format produced by
/// `plugins/ainb-hooks/hooks/notify.sh`. Field names are stable and
/// must not be renamed without bumping [`PROTOCOL_VERSION`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// The envelope schema version this row was produced under.
    pub protocol_version: u8,
    /// The host agent that fired the hook: `"claude"` or `"codex"`.
    pub agent: String,
    /// The raw event name as emitted by the host agent, preserving
    /// any matcher suffix (e.g. `Notification:idle_prompt`). Per
    /// spec we deliberately do NOT canonicalize across agents.
    pub raw_event: String,
    /// The host agent's session identifier. May be empty when the
    /// host agent did not provide one (rare in practice).
    #[serde(default)]
    pub session_id: String,
    /// The session's current working directory at the moment the
    /// hook fired.
    #[serde(default)]
    pub cwd: String,
    /// `basename(cwd)` — pre-computed by the hook for convenience.
    #[serde(default)]
    pub project: String,
    /// Epoch milliseconds at which the hook fired.
    pub ts: i64,
    /// The original hook payload from the host agent, preserved
    /// verbatim for the Inbox detail view.
    pub payload: serde_json::Value,
}

impl Envelope {
    /// Parse one envelope from a JSON byte slice and validate the
    /// protocol version + required string fields.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let env: Envelope = serde_json::from_slice(bytes)?;
        env.validate()?;
        Ok(env)
    }

    /// Validate the parsed envelope. Pulled out so call sites can
    /// re-validate envelopes they've reconstructed from other sources
    /// (fallback replay, tests).
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(EnvelopeError::UnsupportedVersion {
                got: self.protocol_version,
                expected: PROTOCOL_VERSION,
            });
        }
        if self.agent.is_empty() {
            return Err(EnvelopeError::MissingField("agent"));
        }
        if self.raw_event.is_empty() {
            return Err(EnvelopeError::MissingField("raw_event"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(agent: &str, event: &str) -> String {
        format!(
            r#"{{"protocol_version":1,"agent":"{agent}","raw_event":"{event}","session_id":"abc","cwd":"/tmp/x","project":"x","ts":1700000000000,"payload":{{"k":"v"}}}}"#
        )
    }

    #[test]
    fn parses_a_well_formed_claude_envelope() {
        let json = sample("claude", "Stop");
        let env = Envelope::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(env.agent, "claude");
        assert_eq!(env.raw_event, "Stop");
        assert_eq!(env.session_id, "abc");
        assert_eq!(env.payload["k"], "v");
    }

    #[test]
    fn parses_a_well_formed_codex_envelope() {
        let json = sample("codex", "agent-turn-complete");
        let env = Envelope::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(env.agent, "codex");
        assert_eq!(env.raw_event, "agent-turn-complete");
    }

    #[test]
    fn rejects_unsupported_protocol_version() {
        let json = r#"{"protocol_version":99,"agent":"claude","raw_event":"Stop","ts":1,"payload":{}}"#;
        let err = Envelope::from_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, EnvelopeError::UnsupportedVersion { got: 99, .. }));
    }

    #[test]
    fn rejects_missing_agent() {
        let json = r#"{"protocol_version":1,"agent":"","raw_event":"Stop","ts":1,"payload":{}}"#;
        let err = Envelope::from_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, EnvelopeError::MissingField("agent")));
    }

    #[test]
    fn rejects_missing_raw_event() {
        let json = r#"{"protocol_version":1,"agent":"claude","raw_event":"","ts":1,"payload":{}}"#;
        let err = Envelope::from_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, EnvelopeError::MissingField("raw_event")));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = Envelope::from_bytes(b"not json").unwrap_err();
        assert!(matches!(err, EnvelopeError::Json(_)));
    }
}
