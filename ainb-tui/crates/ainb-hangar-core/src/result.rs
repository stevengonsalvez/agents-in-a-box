//! The structured `agent_task_queue.result` JSON shape (P9.1).
//!
//! `result` is a free-form JSON/TEXT column (migration 0004 — no schema change
//! needed for P9). [`TaskResult`] is the canonical typed view of what the daemon
//! writes there on a successful completion, so producers and the TUI agree on
//! the field names without a hand-maintained `json!({…})` literal.
//!
//! # Shape
//!
//! ```json
//! { "content": "<trailing stdout tail>", "exit_code": 0, "pr_url": "https://…/pull/42" }
//! ```
//!
//! - `content` — the provider's trailing stdout (the audit/UI tail).
//! - `exit_code` — the process exit code (`null` when killed by signal/timeout).
//! - `pr_url` — the canonical `gh pr create` URL captured from stdout (P9.1), or
//!   absent when the run created no PR. **Serialized only when present**
//!   (`skip_serializing_if`), so a no-PR run's JSON is byte-identical to the
//!   pre-P9 shape — no existing `result` reader sees a new `"pr_url": null` key,
//!   and a NULL pr_url is "no key", never `""`.
//!
//! The type is intentionally additive: new fields land as `Option<_>` with
//! `skip_serializing_if = "Option::is_none"` so old readers keep working and the
//! on-the-wire shape only grows when there's something to say.

use serde::{Deserialize, Serialize};

/// Typed view of the `agent_task_queue.result` JSON blob a successful task
/// completion writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskResult {
    /// The provider's trailing stdout (the bounded audit/UI tail).
    #[serde(default)]
    pub content: String,
    /// The process exit code, or `None` if the run was killed by signal/timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The canonical `gh pr create` PR URL captured from stdout (P9.1), or
    /// `None` when the run opened no PR. Omitted from the JSON entirely when
    /// `None` — never serialized as `""` or `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
}

impl TaskResult {
    /// Build a completion result from the captured provider output and the PR
    /// URL parsed from stdout (P9.1).
    #[must_use]
    pub const fn new(content: String, exit_code: Option<i32>, pr_url: Option<String>) -> Self {
        Self {
            content,
            exit_code,
            pr_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-PR result serializes WITHOUT a `pr_url` key (byte-identical to the
    /// pre-P9 shape), so no existing reader sees a new field.
    #[test]
    fn no_pr_omits_pr_url_key() {
        let r = TaskResult::new("ok".into(), Some(0), None);
        let json = serde_json::to_string(&r).expect("serialize");
        assert!(!json.contains("pr_url"), "no-PR result must omit pr_url, got {json}");
        assert!(json.contains("\"content\":\"ok\""));
        assert!(json.contains("\"exit_code\":0"));
    }

    /// A captured PR URL is serialized into the `pr_url` key.
    #[test]
    fn pr_url_present_serializes() {
        let r = TaskResult::new(
            "ok".into(),
            Some(0),
            Some("https://github.com/o/r/pull/9".into()),
        );
        let json = serde_json::to_string(&r).expect("serialize");
        assert!(json.contains("\"pr_url\":\"https://github.com/o/r/pull/9\""), "got {json}");
    }

    /// A killed run (`exit_code = None`) omits `exit_code` too — additive,
    /// no `null` noise.
    #[test]
    fn killed_run_omits_exit_code() {
        let r = TaskResult::new("partial".into(), None, None);
        let json = serde_json::to_string(&r).expect("serialize");
        assert_eq!(json, "{\"content\":\"partial\"}");
    }

    /// Round-trips: the pre-P9 `{content, exit_code}` JSON still deserializes
    /// (the new `pr_url` field defaults to `None`).
    #[test]
    fn legacy_shape_deserializes() {
        let legacy = r#"{"content":"hi","exit_code":0}"#;
        let r: TaskResult = serde_json::from_str(legacy).expect("deserialize legacy");
        assert_eq!(r.content, "hi");
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.pr_url, None);
    }
}
