//! Prettied JSONL timeline (tcp T3 / F6, discharges the P10 §4.9 deferral).
//!
//! A provider run tees its CLI stream-json to disk (`{logs}/claude.jsonl` /
//! `{logs}/codex.jsonl`, see the daemon runner). This module turns that on-disk
//! transcript into the same [`ViewEntry`](crate::screen::task_detail::ViewEntry)
//! shape the *live* task-detail transcript renders through
//! ([`crate::widgets::transcript::render_transcript`]), so a card's history reads
//! with the identical 5-colour taxonomy.
//!
//! The classification itself lives in
//! [`ainb_hangar_proto::transcript`], shared with the daemon's live
//! `TaskMessage` producer and durable timeline read, so a line streamed live
//! and the same line re-read from disk are byte-identical. That module also
//! documents the provider shapes and the truncated / mid-write robustness
//! contract; this one only wraps its output in the plugin's view type.

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

    /// A claude-shape run: the wrapper yields one plain `ViewEntry::Line` per
    /// classified line, in the classifier's lanes and order (system status,
    /// prose, tool call, tool result, run status, LAST REPLY marker + text).
    #[test]
    fn wrapper_yields_line_entries_in_the_classifier_lanes() {
        let jsonl = r#"
{"type":"system","subtype":"init","session_id":"abc123session"}
{"type":"assistant","timestamp":1000,"message":{"content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test"}}]}}
{"type":"user","timestamp":2500,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}
{"type":"result","subtype":"success","duration_ms":4200,"result":"All green."}
"#;
        let kinds: Vec<MessageKind> = parse_timeline(jsonl)
            .iter()
            .map(|e| match e {
                ViewEntry::Line(l) => l.kind(),
                ViewEntry::CollapsedThinking { .. } => panic!("the wrapper never folds: {e:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                MessageKind::ToolResult,
                MessageKind::Agent,
                MessageKind::ToolCall,
                MessageKind::ToolResult,
                MessageKind::ToolResult,
                MessageKind::Agent,
                MessageKind::Agent,
            ]
        );
    }
}
