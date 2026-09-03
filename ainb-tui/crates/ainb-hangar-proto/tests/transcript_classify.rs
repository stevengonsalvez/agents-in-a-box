//! The stream-json classifier's behaviour, pinned at its new home (move 1 step
//! 1, test T0). These are the plugin's `jsonl_timeline` tests moved verbatim,
//! asserting on `(MessageKind, body)` pairs instead of `ViewEntry`s: the
//! classifier is now shared by the daemon's live stream and durable read and
//! the plugin's on-disk render, so a regression here is attributable to the
//! move, not to a later consumer.

use ainb_hangar_proto::events::MessageKind;
use ainb_hangar_proto::transcript::{StreamJsonClassifier, classify_stream_json};

/// A claude-shape transcript: system init, an assistant text + a timestamped
/// tool_use, the tool_result (also timestamped → a duration), and the result
/// line with a LAST REPLY. The classifier surfaces each lane, names the tool on
/// its result, and attaches the per-tool duration.
#[test]
fn parses_claude_shape_with_tool_durations_and_last_reply() {
    let jsonl = r#"
{"type":"system","subtype":"init","session_id":"abc123session","tools":["Bash"]}
{"type":"assistant","timestamp":1000,"message":{"content":[{"type":"text","text":"Let me check the tests."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test --workspace"}}]}}
{"type":"user","timestamp":2500,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"test result: ok. 42 passed"}]}}
{"type":"result","subtype":"success","total_cost_usd":0.1234,"duration_ms":4200,"result":"All 42 tests pass."}
"#;
    let lanes = classify_stream_json(jsonl);

    // The tool call reads its name + compact command.
    assert!(
        lanes.iter().any(|(k, b)| *k == MessageKind::ToolCall
            && b.contains("Bash")
            && b.contains("cargo test --workspace")),
        "tool call surfaces name + command: {lanes:?}"
    );
    // The tool result names the tool AND carries the per-tool duration
    // (2500ms - 1000ms = 1.5s).
    assert!(
        lanes.iter().any(|(k, b)| *k == MessageKind::ToolResult
            && b.contains("Bash")
            && b.contains("1.5s")),
        "tool result carries the duration: {lanes:?}"
    );
    // The assistant prose lands in the agent lane.
    assert!(
        lanes
            .iter()
            .any(|(k, b)| *k == MessageKind::Agent && b.contains("check the tests")),
        "assistant prose in the agent lane: {lanes:?}"
    );
    // The result surfaces a LAST REPLY block + a status transition with cost +
    // total duration.
    assert!(
        lanes.iter().any(|(k, b)| *k == MessageKind::Agent && b == "LAST REPLY"),
        "a LAST REPLY marker: {lanes:?}"
    );
    assert!(
        lanes
            .iter()
            .any(|(k, b)| *k == MessageKind::Agent && b.contains("All 42 tests pass")),
        "the final reply text: {lanes:?}"
    );
    assert!(
        lanes.iter().any(|(k, b)| *k == MessageKind::ToolResult
            && b.contains("success")
            && b.contains("$0.1234")
            && b.contains("4.2s")),
        "a run-status transition with cost + duration: {lanes:?}"
    );
}

/// A file the daemon is still appending to ends in a TRUNCATED line (invalid
/// JSON). The classifier yields the complete prefix and skips the partial tail:
/// never a panic, never a half-decoded entry.
#[test]
fn tolerates_a_truncated_mid_write_tail() {
    let jsonl = concat!(
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tex" // cut mid-write
    );
    let lanes = classify_stream_json(jsonl);
    assert_eq!(lanes.len(), 1, "only the complete line parsed: {lanes:?}");
    assert_eq!(lanes[0], (MessageKind::Agent, "done".to_string()));
}

/// Blank lines, non-JSON log noise, and an unknown line type are all skipped
/// without error (robust to a mixed / future-shaped file).
#[test]
fn skips_blank_noise_and_unknown_lines() {
    let jsonl = "\n\nnot json at all\n{\"type\":\"future_kind\",\"x\":1}\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n";
    assert_eq!(
        classify_stream_json(jsonl),
        vec![(MessageKind::Agent, "hi".to_string())]
    );
}

/// A tool_use with no matching (timestamped) result renders the call without a
/// duration, and an `is_error` tool_result lands in the red error lane.
#[test]
fn tool_without_timestamps_has_no_duration_and_errors_are_red() {
    let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Read","input":{"file_path":"/etc/hosts"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"boom","is_error":true}]}}
"#;
    let lanes = classify_stream_json(jsonl);
    assert!(
        lanes.iter().any(|(k, b)| *k == MessageKind::ToolCall
            && b.contains("Read")
            && b.contains("/etc/hosts")
            && !b.contains('(')),
        "no duration parens when timestamps are absent: {lanes:?}"
    );
    assert!(
        lanes
            .iter()
            .any(|(k, b)| *k == MessageKind::Error && b.contains("Read") && b.contains("boom")),
        "an is_error result is in the error lane: {lanes:?}"
    );
}

/// A codex `{"msg":{…}}` agent_message is surfaced as prose; an unknown codex
/// event is skipped.
#[test]
fn best_effort_codex_agent_message() {
    let jsonl = "{\"msg\":{\"type\":\"agent_message\",\"message\":\"shipping it\"}}\n{\"msg\":{\"type\":\"token_count\",\"n\":5}}\n";
    assert_eq!(
        classify_stream_json(jsonl),
        vec![(MessageKind::Agent, "shipping it".to_string())]
    );
}

/// A top-level `{"type":"error"}` line and a codex `{"msg":{"type":"error"}}`
/// both land in the error lane with their message: an error line vanishing
/// from the timeline is the defect these two arms exist to prevent.
#[test]
fn explicit_error_lines_land_in_the_error_lane() {
    let jsonl = "{\"type\":\"error\",\"error\":\"rate limited\"}\n{\"msg\":{\"type\":\"error\",\"message\":\"codex broke\"}}\n";
    assert_eq!(
        classify_stream_json(jsonl),
        vec![
            (MessageKind::Error, "rate limited".to_string()),
            (MessageKind::Error, "codex broke".to_string()),
        ]
    );
}

/// An empty transcript is an empty timeline (not a panic).
#[test]
fn empty_input_is_empty() {
    assert!(classify_stream_json("").is_empty());
    assert!(classify_stream_json("   \n  \n").is_empty());
}

/// The live producer feeds one line at a time through one classifier; that must
/// yield exactly what the whole-file read yields, including the cross-line tool
/// name and duration on the result (the reason the classifier is stateful).
#[test]
fn line_at_a_time_matches_the_whole_file() {
    let jsonl = r#"
{"type":"assistant","timestamp":1000,"message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":1250,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"a b c"}]}}
{"type":"result","subtype":"success","duration_ms":900,"result":"three files"}
"#;
    let mut classifier = StreamJsonClassifier::default();
    let live: Vec<_> = jsonl.lines().flat_map(|l| classifier.classify_line(l)).collect();
    assert_eq!(live, classify_stream_json(jsonl));
    assert!(
        live.iter()
            .any(|(k, b)| *k == MessageKind::ToolResult && b == "Bash  a b c  (250ms)"),
        "the result names its tool and carries the duration: {live:?}"
    );
}
