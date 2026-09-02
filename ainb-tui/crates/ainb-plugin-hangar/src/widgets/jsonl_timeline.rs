//! Prettied JSONL timeline (tcp T3 / F6, discharges the P10 §4.9 deferral).
//!
//! A provider run tees its CLI stream-json to disk (`{logs}/claude.jsonl` /
//! `{logs}/codex.jsonl`, see the daemon runner). This module turns that on-disk
//! transcript into the same [`ViewEntry`](crate::screen::task_detail::ViewEntry)
//! shape the *live* task-detail transcript renders through
//! ([`crate::widgets::transcript::render_transcript`]), so a card's history reads
//! with the identical 5-colour taxonomy.
//!
//! The classification itself lives in [`ainb_hangar_proto::transcript`] so
//! the daemon can call the same function when it starts producing the live
//! `TaskMessage` stream (track A step A2): from then on a line streamed live
//! and the same line re-read from disk classify identically. Today this
//! wrapper is the only caller. That module documents the provider shapes and
//! the truncated / mid-write robustness contract; this one only wraps its
//! output in the plugin's view type.

use ainb_hangar_proto::transcript::classify_stream_json;

use crate::screen::task_detail::ViewEntry;

/// Parse a provider `stream-json` transcript into the timeline view entries the
/// shared transcript renderer paints.
///
/// Line-oriented and total: unparseable / unknown lines are skipped (a
/// mid-write tail never breaks the render), so the result is the transcript's
/// complete decoded prefix in stream order.
#[must_use]
pub fn parse_timeline(jsonl: &str) -> Vec<ViewEntry> {
    classify_stream_json(jsonl)
        .into_iter()
        .map(|(kind, body)| ViewEntry::line(kind, body))
        .collect()
}

#[cfg(test)]
mod tests {
    use ainb_hangar_proto::events::MessageKind;

    use super::*;

    /// The wrapper is a map over `classify_stream_json`, so this pins only what
    /// the map can get wrong: every entry is a plain `ViewEntry::Line` (never a
    /// thinking fold, never a comment) carrying the classifier's kind and body
    /// unchanged, across every lane of the taxonomy. The move itself is pinned
    /// against the pre-move behaviour in `ainb-hangar-proto`'s
    /// `tests/transcript_classify.rs` (T0).
    #[test]
    fn wrapper_preserves_kind_and_body_and_folds_nothing() {
        let jsonl = r#"
{"type":"system","subtype":"init","session_id":"abc123session"}
{"type":"assistant","timestamp":1000,"message":{"content":[{"type":"thinking","thinking":"plan\nthen act"},{"type":"text","text":"Let me check."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test"}}]}}
{"type":"user","timestamp":2500,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"boom","is_error":true}]}}
{"msg":{"type":"agent_message","message":"shipping it"}}
{"type":"result","subtype":"success","total_cost_usd":0.1234,"duration_ms":4200,"result":"All green."}
"#;
        let via_wrapper: Vec<(MessageKind, String)> = parse_timeline(jsonl)
            .iter()
            .map(|e| match e {
                ViewEntry::Line(l) => {
                    assert!(!l.is_comment(), "a disk line is never a comment: {l:?}");
                    (l.kind(), l.body().to_string())
                }
                ViewEntry::CollapsedThinking { .. } => panic!("the wrapper never folds: {e:?}"),
            })
            .collect();
        let via_proto = classify_stream_json(jsonl);
        assert_eq!(via_wrapper, via_proto);
        for kind in [
            MessageKind::Agent,
            MessageKind::Thinking,
            MessageKind::ToolCall,
            MessageKind::ToolResult,
            MessageKind::Error,
        ] {
            assert!(
                via_proto.iter().any(|(k, _)| *k == kind),
                "fixture must exercise {kind:?}: {via_proto:?}"
            );
        }
    }
}
