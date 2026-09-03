//! The ACP half of the shared classifier (move 1 step A6, test T8's unit half).
//!
//! Every fixture here is the EXACT `raw_payload` `ainb-acp` writes to
//! `fleet_provider_event`: the reducer's coalesced-text and verbatim-update
//! shapes, the pool's approval envelope, and the store writer's truncation
//! marker. A shape that drifts from what the writer produces would leave these
//! green while the real read showed nothing, so each is annotated with the
//! producer it was copied from.

use ainb_hangar_proto::events::MessageKind;
use ainb_hangar_proto::transcript::AcpClassifier;

/// Classify a whole session's rows, oldest first, through one classifier.
fn classify(rows: &[(&str, &str)]) -> Vec<(MessageKind, String)> {
    let mut classifier = AcpClassifier::default();
    rows.iter()
        .flat_map(|(event_type, payload)| classifier.classify_row(event_type, payload))
        .collect()
}

/// A whole turn in the shape the reducer + store writer actually commit it:
/// a thought, a tool call, its completion, agent prose, the closing marker.
/// Every lane of the taxonomy the ACP side can produce shows up exactly once.
#[test]
fn a_whole_acp_turn_fills_the_same_lanes_a_process_run_does() {
    let lanes = classify(&[
        // reducer::flush — coalesced text.
        (
            "acp.thought",
            r#"{"kind":"acp.thought","text":"The handler is probably unregistered.","coalescedDeltas":3}"#,
        ),
        // reducer::push, Structural — the verbatim `SessionUpdate`.
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call","toolCallId":"call-1","title":"Edit","kind":"edit","status":"pending","rawInput":{"file_path":"api/src/routes.ts"}}"#,
        ),
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"1 file changed"}}]}"#,
        ),
        (
            "acp.message",
            r#"{"kind":"acp.message","text":"Route registered.","coalescedDeltas":7}"#,
        ),
        // store_writer::lifecycle.
        (
            "acp.turn_completed",
            r#"{"turnId":"msg-1","durationMs":4200}"#,
        ),
    ]);

    assert_eq!(
        lanes,
        vec![
            (
                MessageKind::Thinking,
                "The handler is probably unregistered.".to_string()
            ),
            (MessageKind::ToolCall, "Edit  api/src/routes.ts".to_string()),
            (MessageKind::ToolResult, "Edit  1 file changed".to_string()),
            (MessageKind::Agent, "Route registered.".to_string()),
            (
                MessageKind::ToolResult,
                "· turn_completed · 4.2s".to_string()
            ),
        ],
        "the whole turn, lane for lane"
    );
}

/// The one piece of cross-row state, and the reason this classifier is not a
/// pure function: an update carries only its `toolCallId`, so it names its tool
/// solely because the call before it was remembered.
#[test]
fn an_update_names_the_tool_its_call_declared() {
    let named = classify(&[
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Bash","status":"pending"}"#,
        ),
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"ok"}}]}"#,
        ),
    ]);
    assert_eq!(named[1], (MessageKind::ToolResult, "Bash  ok".to_string()));

    // And the tail-boundary degradation, which is what the ACP read does when
    // the call fell outside the returned window: the SAME unnamed `tool` form
    // the stream-json tail produces, never a wrong name and never a drop.
    let orphaned = classify(&[(
        "acp.tool_call",
        r#"{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"ok"}}]}"#,
    )]);
    assert_eq!(
        orphaned,
        vec![(MessageKind::ToolResult, "tool  ok".to_string())]
    );
}

/// A failed tool lands in the RED lane, not the slate one, so a broken run
/// reads as broken. Same treatment `is_error` gets on the stream-json side.
#[test]
fn a_failed_tool_call_is_the_error_lane() {
    let lanes = classify(&[
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Bash","status":"pending","rawInput":{"command":"cargo test"}}"#,
        ),
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"failed","content":[{"type":"content","content":{"type":"text","text":"exit 101"}}]}"#,
        ),
    ]);
    assert_eq!(
        lanes[1],
        (MessageKind::Error, "Bash  exit 101  [error]".to_string())
    );
}

/// `pending → in_progress` is bookkeeping every adapter emits per call and the
/// process executor has no counterpart for. It must not become a transcript
/// row, or an ACP run reads with twice the tool lines a process run does.
#[test]
fn an_in_progress_update_is_not_a_transcript_line() {
    let lanes = classify(&[
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Read","status":"pending"}"#,
        ),
        (
            "acp.tool_call",
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"in_progress"}"#,
        ),
    ]);
    assert_eq!(lanes, vec![(MessageKind::ToolCall, "Read".to_string())]);
}

/// The three row types that are deliberately silent, asserted as a SET against
/// the exact payloads their producers write. A count or a "some rows are
/// skipped" assertion would survive any one of them starting to render.
#[test]
fn usage_prompt_echo_and_session_bookkeeping_render_nothing() {
    let silent = [
        // reducer, `UsageUpdate` — A7's input, not a transcript lane.
        (
            "acp.usage",
            r#"{"sessionUpdate":"usage_update","used":1200,"size":200000}"#,
        ),
        // reducer, `UserMessageChunk` — the adapter echoing our own prompt.
        (
            "acp.user_message",
            r#"{"kind":"acp.user_message","text":"do the work","coalescedDeltas":1}"#,
        ),
        // store_writer::lifecycle.
        ("acp.turn_started", r#"{"turnId":"msg-1"}"#),
        (
            "acp.context_rebuilt",
            r#"{"mode":"loaded","acpSessionId":"fake-session-1"}"#,
        ),
        // A row type this taxonomy has never seen at all.
        ("acp.something_new", r#"{"anything":true}"#),
    ];
    for (event_type, payload) in silent {
        assert_eq!(
            classify(&[(event_type, payload)]),
            Vec::new(),
            "{event_type} must render nothing"
        );
    }
}

/// The writer's truncation marker is the one row the transcript MUST show:
/// a silently short transcript reads as a complete one.
#[test]
fn a_truncation_marker_admits_the_hole_in_the_error_lane() {
    let lanes = classify(&[(
        "acp.transcript_truncated",
        r#"{"kind":"acp.transcript_truncated","droppedRows":12,"droppedRowsTotal":12}"#,
    )]);
    assert_eq!(
        lanes,
        vec![(
            MessageKind::Error,
            "· transcript truncated · 12 rows dropped".to_string()
        )]
    );
}

/// A parked approval names the tool it is gating, from the pool's own envelope
/// (`raise_permission`), whose tool sits under `toolCall` rather than at the
/// top level the way an adapter update's does.
#[test]
fn a_parked_approval_names_its_tool() {
    let lanes = classify(&[(
        "acp.permission",
        r#"{"kind":"acp_permission","sessionKey":"acp:1","acpSessionId":"fake-session-1","requestFingerprint":"fp","rpcId":"7","options":[{"optionId":"allow"}],"toolCall":{"toolCallId":"c9","title":"Bash"}}"#,
    )]);
    assert_eq!(
        lanes,
        vec![(MessageKind::ToolCall, "[approval] Bash".to_string())]
    );
}

/// A failed or interrupted turn closes in the error lane, carrying the cause
/// the pool recorded, so "why did this stop" is answerable from the transcript.
#[test]
fn a_turn_that_did_not_finish_closes_in_the_error_lane() {
    let failed = classify(&[("acp.turn_failed", r#"{"turnId":"m1","durationMs":900}"#)]);
    assert_eq!(
        failed,
        vec![(MessageKind::Error, "· turn_failed · 900ms".to_string())]
    );

    let interrupted = classify(&[(
        "acp.turn_interrupted",
        r#"{"turnId":"m1","cause":"turn_deadline"}"#,
    )]);
    assert_eq!(
        interrupted,
        vec![(
            MessageKind::Error,
            "· turn_interrupted · turn_deadline".to_string()
        )]
    );
}

/// Total, like its stream-json twin: a payload that is not JSON at all, and a
/// known type missing every field it reads, both degrade rather than panic.
#[test]
fn a_malformed_row_degrades_instead_of_failing() {
    assert_eq!(classify(&[("acp.message", "{not json")]), Vec::new());
    assert_eq!(classify(&[("acp.tool_call", "{}")]), Vec::new());
    assert_eq!(
        classify(&[("acp.turn_completed", "{}")]),
        vec![(MessageKind::ToolResult, "· turn_completed".to_string())]
    );
}

/// A multi-line reply is one entry PER LINE, the same as the stream-json side,
/// so a renderer painting one entry per row never overflows.
#[test]
fn a_multi_line_reply_is_one_entry_per_line() {
    let lanes = classify(&[(
        "acp.message",
        r#"{"kind":"acp.message","text":"first\n\nsecond","coalescedDeltas":2}"#,
    )]);
    assert_eq!(
        lanes,
        vec![
            (MessageKind::Agent, "first".to_string()),
            (MessageKind::Agent, "second".to_string()),
        ],
        "blank lines dropped, one entry per remaining line"
    );
}

/// A non-text content block (the reducer's `NonText` shape: an image, audio, an
/// embedded resource) has no text to render, and is NAMED rather than dropped.
#[test]
fn a_non_text_block_is_named_not_dropped() {
    let lanes = classify(&[(
        "acp.message",
        r#"{"kind":"acp.message","text":"","coalescedDeltas":0,"block":{"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"…","mimeType":"image/png"}}}"#,
    )]);
    assert_eq!(
        lanes,
        vec![(MessageKind::Agent, "· image content".to_string())]
    );
}

/// The plan reads as the checklist it is: one blue line per entry.
#[test]
fn a_plan_is_one_line_per_entry() {
    let lanes = classify(&[(
        "acp.plan",
        r#"{"sessionUpdate":"plan","entries":[{"content":"read the file","priority":"high","status":"completed"},{"content":"patch it","priority":"high","status":"pending"}]}"#,
    )]);
    assert_eq!(
        lanes,
        vec![
            (
                MessageKind::ToolCall,
                "plan · completed · read the file".to_string()
            ),
            (
                MessageKind::ToolCall,
                "plan · pending · patch it".to_string()
            ),
        ]
    );
}

/// The ACP path goes through the SAME `capped()` backstop the stream-json path
/// does, asserted rather than assumed.
///
/// The gate is shared in code today, but nothing pinned it from this side: a
/// refactor dropping the `capped(out)` call from `classify_row` broke no test,
/// which is the exact "agrees by discipline" failure this whole step exists to
/// remove. `BODY_MAX` is 8192, so the assertion is that a huge body comes back
/// SHORTER than it went in and ends in the shared ellipsis.
#[test]
fn a_huge_acp_body_is_capped_by_the_same_gate() {
    let huge = "x".repeat(50_000);
    let lanes = classify(&[(
        "acp.message",
        &serde_json::json!({ "kind": "acp.message", "text": huge, "coalescedDeltas": 1 })
            .to_string(),
    )]);
    assert_eq!(lanes.len(), 1, "one line in, one line out: {lanes:?}");
    assert_eq!(lanes[0].0, MessageKind::Agent);
    assert_eq!(
        lanes[0].1.chars().count(),
        8192,
        "capped at BODY_MAX by the shared gate"
    );
    assert!(
        lanes[0].1.ends_with('…'),
        "and capped by the SHARED truncator, which marks what it cut"
    );
}

/// The entry-count backstop, from the ACP side: one row carrying more lines
/// than `ENTRIES_PER_LINE_MAX` is truncated and SAYS how many it dropped,
/// rather than silently returning a short transcript.
#[test]
fn an_acp_row_of_many_lines_admits_what_the_gate_dropped() {
    let many = (0..600).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
    let lanes = classify(&[(
        "acp.message",
        &serde_json::json!({ "kind": "acp.message", "text": many, "coalescedDeltas": 1 })
            .to_string(),
    )]);
    assert_eq!(lanes.len(), 512, "capped at ENTRIES_PER_LINE_MAX");
    assert_eq!(
        lanes[511],
        (MessageKind::ToolResult, "… 89 more lines".to_string()),
        "the shared gate's own marker closes the run"
    );
}
