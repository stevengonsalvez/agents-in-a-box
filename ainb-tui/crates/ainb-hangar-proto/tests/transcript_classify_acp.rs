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

/// A GitHub token, built at run time so no scanner mistakes the fixture for a
/// leak. Its body is one repeated letter, so any piece of it shows as a run.
fn token() -> String {
    format!("ghp_{}", "Q".repeat(40))
}

/// No piece of [`token`] survives in `body`: not its prefix, not a run of its
/// body. A cut that ran before the scrub leaves the prefix plus a few body
/// characters, which is exactly the fragment no credential shape matches.
fn assert_no_token_piece(body: &str, what: &str) {
    assert!(
        !body.contains("ghp_"),
        "{what}: the token's prefix survived: {body}"
    );
    assert!(
        !body.contains("QQQQ"),
        "{what}: the token's body survived: {body}"
    );
}

/// #1187: every cut the classifier makes runs after the scrub. Each fixture
/// starts the token a few characters before its cut, where a cut-then-scrub
/// leaves `ghp_` plus a handful of characters that match no shape.
#[test]
fn a_token_straddling_each_cut_never_survives_it() {
    let token = token();
    let lead = "x".repeat(70);

    // The tool result's one-line summary (SUMMARY_MAX).
    let result = classify(&[(
        "acp.tool_call",
        &serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "c1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": format!("{lead} {token}")}}],
        })
        .to_string(),
    )]);
    assert_eq!(result.len(), 1, "one result line: {result:?}");
    assert_no_token_piece(&result[0].1, "tool result");

    // The tool call's compact input (SUMMARY_MAX), telling field and flat form.
    for raw_input in [
        serde_json::json!({"command": format!("{lead} {token}")}),
        serde_json::json!({"note": format!("{lead} {token}")}),
    ] {
        let call = classify(&[(
            "acp.tool_call",
            &serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "c2",
                "title": "Bash",
                "status": "pending",
                "rawInput": raw_input,
            })
            .to_string(),
        )]);
        assert_eq!(call.len(), 1, "one call line: {call:?}");
        assert_no_token_piece(&call[0].1, "tool input");
    }

    // A plan entry (SUMMARY_MAX, after its `plan · status · ` prefix).
    let plan = classify(&[(
        "acp.plan",
        &serde_json::json!({
            "entries": [{"status": "pending", "content": format!("{} {token}", "x".repeat(50))}],
        })
        .to_string(),
    )]);
    assert_eq!(plan.len(), 1, "one plan line: {plan:?}");
    assert_no_token_piece(&plan[0].1, "plan entry");

    // A body at the hard ceiling (BODY_MAX, 8192 chars).
    let message = classify(&[(
        "acp.message",
        &serde_json::json!({
            "kind": "acp.message",
            "text": format!("{} {token}", "x".repeat(8180)),
            "coalescedDeltas": 1,
        })
        .to_string(),
    )]);
    assert_eq!(message.len(), 1, "one message line: {message:?}");
    assert_no_token_piece(&message[0].1, "message body");
}

/// One `acp.message` row carrying `text`, as the reducer writes it.
fn message(text: &str) -> Vec<(MessageKind, String)> {
    classify(&[(
        "acp.message",
        &serde_json::json!({"kind": "acp.message", "text": text, "coalescedDeltas": 1}).to_string(),
    )])
}

/// One completed tool-call update whose output is `text`.
fn tool_result(text: &str) -> Vec<(MessageKind, String)> {
    classify(&[(
        "acp.tool_call",
        &serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "c1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": text}}],
        })
        .to_string(),
    )])
}

/// The cuts count characters, not bytes: a lead of 3-byte characters puts the
/// token across the same cuts and it still goes whole.
#[test]
fn a_token_straddling_a_cut_after_multibyte_text_never_survives_it() {
    let token = token();
    let result = tool_result(&format!("{} {token}", "日".repeat(70)));
    assert_eq!(result.len(), 1, "one result line: {result:?}");
    assert_no_token_piece(&result[0].1, "tool result after multi-byte text");

    let body = message(&format!("{} {token}", "日".repeat(8180)));
    assert_eq!(body.len(), 1, "one message line: {body:?}");
    assert_no_token_piece(&body[0].1, "message body after multi-byte text");
}

/// A private key spans lines, and each base64 line alone matches no shape: the
/// block goes whole before the text is split into one entry per line.
#[test]
fn a_private_key_across_lines_is_removed_whole() {
    let key_line = format!("MIIEow{}", "Q".repeat(58));
    let text = format!(
        "here is the key\n-----BEGIN RSA PRIVATE KEY-----\n{key_line}\n{key_line}\n-----END RSA PRIVATE KEY-----\nthat was it"
    );
    let lines: Vec<String> = message(&text).into_iter().map(|(_, body)| body).collect();
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("MIIEow") && !line.contains("PRIVATE KEY")),
        "a piece of the key survived: {lines:?}"
    );
    assert_eq!(lines.first().map(String::as_str), Some("here is the key"));
    assert_eq!(lines.last().map(String::as_str), Some("that was it"));
}

/// The scrub runs over a bounded window, and a secret inside that window is
/// still redacted wherever it sits: in a summary past its cut, on a late line
/// of a long block, and in a body just under its ceiling.
#[test]
fn a_secret_inside_the_scrub_window_is_still_redacted() {
    let token = token();

    let result = tool_result(&format!(
        "{} {token} {}",
        "x".repeat(2000),
        "y".repeat(2000)
    ));
    assert_no_token_piece(&result[0].1, "summary window");

    let block: String = (0..400)
        .map(|i| {
            if i == 300 {
                format!("line {i} {token}")
            } else {
                format!("line {i}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (_, line) in message(&block) {
        assert_no_token_piece(&line, "late line of a long block");
    }

    let body = message(&format!("{} {token}", "x".repeat(8000)));
    assert_no_token_piece(&body[0].1, "body just under its ceiling");
}

/// A multi-megabyte line is classified in time that does not grow with it: the
/// scrub sees a bounded window, not the whole line. Scrubbed whole, ~40 regex
/// passes over 32 MiB took 349 s in a debug build; windowed, about 1 s, so the
/// 10 s ceiling holds on a slow runner. Only the classification is timed: the
/// payloads are built and parsed first, as the live producer hands the
/// classifier a parsed `Value`.
#[test]
fn a_huge_line_is_classified_in_bounded_time_and_size() {
    let huge = "x".repeat(32 << 20);
    let rows = [
        (
            "acp.tool_call",
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "c1",
                "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": huge}}],
            }),
        ),
        ("acp.message", serde_json::json!({"text": huge})),
        (
            "acp.message",
            serde_json::json!({"text": "line\n".repeat(8 << 20)}),
        ),
    ];

    let started = std::time::Instant::now();
    let out: Vec<Vec<(MessageKind, String)>> = rows
        .iter()
        .map(|(event_type, payload)| AcpClassifier::default().classify_value(event_type, payload))
        .collect();
    let elapsed = started.elapsed();

    assert!(
        out[0][0].1.chars().count() <= 84 + "tool  ".len(),
        "summary bounded"
    );
    assert_eq!(out[1].len(), 1);
    assert!(out[1][0].1.chars().count() <= 8192, "body bounded");
    assert!(out[2].len() <= 512, "entries bounded: {}", out[2].len());
    assert_eq!(
        out[2].last().unwrap().1,
        format!("… {} more lines", (8 << 20) - 511)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "a 32 MiB line took {elapsed:?}: the scrub is not windowed"
    );
}

/// The summary's scrub window counts the characters the cut counts. Clipped on
/// raw text, a run of whitespace spends the window and then collapses to one
/// space, so a token past it is only partly scrubbed yet lands inside the cut.
#[test]
fn whitespace_before_a_token_cannot_push_it_out_of_the_scrub_window() {
    let result = tool_result(&format!(
        "{} github_pat_{}",
        " ".repeat(4020),
        "a".repeat(80)
    ));
    assert_eq!(result.len(), 1, "one result line: {result:?}");
    assert!(
        !result[0].1.contains("github_pat_"),
        "a truncated token prefix survived: {}",
        result[0].1
    );
}

/// A summary collapses its text to one line before it scrubs, so a private key
/// in a tool's output reaches the scrub with its armour on the same line as
/// its body. The shape still matches there, and no piece of the key is shown.
#[test]
fn a_private_key_collapsed_into_a_summary_is_removed_whole() {
    let key_line = format!("MIIEow{}", "Q".repeat(58));
    let result = tool_result(&format!(
        "-----BEGIN RSA PRIVATE KEY-----\n{key_line}\n{key_line}\n-----END RSA PRIVATE KEY-----\ndone"
    ));
    assert_eq!(result.len(), 1, "one result line: {result:?}");
    let body = &result[0].1;
    assert!(
        !body.contains("MIIEow") && !body.contains("PRIVATE KEY"),
        "a piece of the key survived: {body}"
    );
    assert!(
        body.contains("done"),
        "the text after the key stays: {body}"
    );
}
