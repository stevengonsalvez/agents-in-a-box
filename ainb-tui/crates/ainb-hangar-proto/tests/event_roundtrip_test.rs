//! Round-trip golden tests for every [`HangarEvent`] wire variant.
//!
//! P4.2 acceptance: *every event variant has a round-trip golden*. These prove
//! the typed event enum serialises to JSON and decodes back byte-identically in
//! shape, that the internal `kind` tag is stable per variant, and that the
//! 5-colour [`MessageKind`] taxonomy + [`PresenceState`] + [`TaskResult`]
//! enums round-trip.
//!
//! Per `reference_msgpack_byte_determinism_vec_over_hashmap` the wire types
//! carry no `HashMap` (whose iteration order varies per process); the structs
//! here are field-ordered and deterministic.

use ainb_hangar_core::ids::{AgentId, CommentId, IssueId, TaskId};
use ainb_hangar_proto::events::{
    AutopilotRow, CommentRow, HangarEvent, IssueRow, MessageKind, PresenceState, TaskResult,
};
use chrono::{TimeZone, Utc};

fn issue_id(s: &str) -> IssueId {
    IssueId::from_str(s).unwrap()
}
fn task_id(s: &str) -> TaskId {
    TaskId::from_str(s).unwrap()
}
fn agent_id(s: &str) -> AgentId {
    AgentId::from_str(s).unwrap()
}
fn comment_id(s: &str) -> CommentId {
    CommentId::from_str(s).unwrap()
}

fn sample_issue() -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: issue_id("issue-1"),
        display_id: None,
        workspace_id: "default".to_string(),
        title: "Refactor API".to_string(),
        description: Some("split the monolith".to_string()),
        state: "open".to_string(),
        assignee: Some("agent:claude".to_string()),
        creator: "member:alice".to_string(),
        created_at: 1_700_000_000_000,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: Some("https://github.com/o/r/pull/42".to_string()),
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

fn sample_comment() -> CommentRow {
    CommentRow {
        id: comment_id("comment-1"),
        issue_id: issue_id("issue-1"),
        author: "member:alice".to_string(),
        body: "looks good".to_string(),
        created_at: 1_700_000_001_000,
    },
    parent_id: None,
}

fn all_variants() -> Vec<HangarEvent> {
    let ts = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let ts2 = Utc.timestamp_opt(1_700_000_500, 0).unwrap();
    vec![
        HangarEvent::IssueCreated(sample_issue()),
        HangarEvent::IssueUpdated(sample_issue()),
        HangarEvent::IssueDeleted {
            issue_id: issue_id("issue-1"),
        },
        HangarEvent::TaskQueued {
            task_id: task_id("task-1"),
            issue_id: issue_id("issue-1"),
            agent_id: agent_id("claude"),
        },
        HangarEvent::TaskStarted {
            task_id: task_id("task-1"),
            started_at: ts,
        },
        HangarEvent::TaskProgress {
            task_id: task_id("task-1"),
            tool_calls: 14,
            elapsed_ms: 582_000,
        },
        HangarEvent::TaskMessage {
            task_id: task_id("task-1"),
            kind: MessageKind::Agent,
            body: "Analyzing middleware structure...".to_string(),
        },
        HangarEvent::TaskFinished {
            task_id: task_id("task-1"),
            result: TaskResult::Success,
            ended_at: ts2,
        },
        HangarEvent::CommentAdded(sample_comment()),
        HangarEvent::AgentPresence {
            agent_id: agent_id("claude"),
            state: PresenceState::Online,
        },
        HangarEvent::SkillUpdated {
            skill: "commit".to_string(),
            updated_at: 1_700_000_002_000,
        },
        HangarEvent::AutopilotUpdated(AutopilotRow {
            id: "ap-1".to_string(),
            workspace_id: "default".to_string(),
            agent_id: "agent-1".to_string(),
            name: "daily-triage".to_string(),
            cron_expr: "0 9 * * 1-5".to_string(),
            next_tick_at: Some(1_700_000_300_000),
            enabled: true,
            last_run_status: Some("completed".to_string()),
            last_run_at: Some(1_699_999_000_000),
            api_trigger_enabled: true,
            ..Default::default()
        }),
        HangarEvent::AutopilotRunChanged {
            autopilot_id: "ap-1".to_string(),
            status: "running".to_string(),
        },
        HangarEvent::WorkspaceChanged {
            from: Some("01J9ZX8QK7".to_string()),
            to: "01J9ZX8QK8".to_string(),
        },
    ]
}

#[test]
fn event_roundtrip_serde_all_variants() {
    for ev in all_variants() {
        let encoded = serde_json::to_string(&ev).expect("encode");
        let decoded: HangarEvent = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, ev, "round-trip mismatch for {ev:?}");
    }
}

#[test]
fn every_variant_carries_a_stable_event_tag() {
    // The internal `event` tag is the wire discriminant the StreamClient keys on.
    let expected = [
        "issue_created",
        "issue_updated",
        "issue_deleted",
        "task_queued",
        "task_started",
        "task_progress",
        "task_message",
        "task_finished",
        "comment_added",
        "agent_presence",
        "skill_updated",
        "autopilot_updated",
        "autopilot_run_changed",
        "workspace_changed",
    ];
    for (ev, tag) in all_variants().iter().zip(expected) {
        let v = serde_json::to_value(ev).expect("encode");
        assert_eq!(v["event"], serde_json::json!(tag), "wrong tag for {ev:?}");
    }
}

#[test]
fn message_kind_covers_five_color_taxonomy() {
    let kinds = [
        (MessageKind::Agent, "agent"),
        (MessageKind::Thinking, "thinking"),
        (MessageKind::ToolCall, "tool_call"),
        (MessageKind::ToolResult, "tool_result"),
        (MessageKind::Error, "error"),
    ];
    for (k, wire) in kinds {
        let v = serde_json::to_value(k).expect("encode");
        assert_eq!(v, serde_json::json!(wire));
        let back: MessageKind = serde_json::from_value(v).expect("decode");
        assert_eq!(back, k);
    }
}

#[test]
fn presence_state_three_states_roundtrip() {
    for (s, wire) in [
        (PresenceState::Online, "online"),
        (PresenceState::Unstable, "unstable"),
        (PresenceState::Offline, "offline"),
    ] {
        let v = serde_json::to_value(s).expect("encode");
        assert_eq!(v, serde_json::json!(wire));
        let back: PresenceState = serde_json::from_value(v).expect("decode");
        assert_eq!(back, s);
    }
}

/// P9.2: an `IssueRow` with no PR omits the `pr_url` key entirely (additive
/// wire — a pre-P9.2 reader never sees a new `"pr_url": null`), and a legacy
/// row without the key still decodes (`pr_url` defaults to `None`).
#[test]
fn issue_row_pr_url_is_additive() {
    let no_pr = IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        origin_type: None,
        origin_id: None,
        pr_url: None,
        branch: None,
        ..sample_issue()
    };
    let json = serde_json::to_string(&no_pr).expect("encode");
    assert!(
        !json.contains("pr_url"),
        "a no-PR issue row must omit pr_url, got {json}"
    );

    // A present URL serializes into the key.
    let with_pr = sample_issue();
    let json = serde_json::to_string(&with_pr).expect("encode");
    assert!(
        json.contains("\"pr_url\":\"https://github.com/o/r/pull/42\""),
        "got {json}"
    );

    // A legacy row (pre-P9.2, no key) decodes with pr_url == None.
    let legacy = r#"{"id":"issue-1","workspace_id":"default","title":"t","description":null,"state":"open","assignee":null,"creator":"member:alice","created_at":0}"#;
    let row: IssueRow = serde_json::from_str(legacy).expect("decode legacy");
    assert_eq!(row.pr_url, None);
}

/// e38.9: an `IssueRow` carries the create-flow attributes priority, due_date,
/// and labels, and a pre-e38.9 snapshot (no keys) still decodes them to their
/// defaults (priority 0, due_date None, empty labels).
#[test]
fn issue_row_priority_due_date_labels_roundtrip_and_default() {
    let row = IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        origin_type: None,
        origin_id: None,
        priority: 3,
        due_date: Some(1_700_000_500_000),
        labels: vec!["bug".to_string(), "p0".to_string()],
        ..sample_issue()
    };
    let json = serde_json::to_string(&row).expect("encode");
    let back: IssueRow = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, row, "priority/due_date/labels round-trip");

    // A pre-e38.9 snapshot (none of the three keys) decodes to defaults.
    let legacy = r#"{"id":"issue-1","workspace_id":"default","title":"t","description":null,"state":"open","assignee":null,"creator":"member:alice","created_at":0}"#;
    let legacy_row: IssueRow = serde_json::from_str(legacy).expect("decode legacy");
    assert_eq!(legacy_row.priority, 0, "default priority is 0");
    assert_eq!(legacy_row.due_date, None, "default due_date is None");
    assert!(legacy_row.labels.is_empty(), "default labels is empty");
}

/// 0046: the sub-issue wire fields (`parent_id` + the `child_total`/`child_done`
/// roll-up) round-trip, and a pre-0046 snapshot (none of the three keys) decodes
/// to their defaults — append-only proof (an old client omits, an old daemon
/// ignores).
#[test]
fn issue_row_subtask_fields_roundtrip_and_default() {
    let row = IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        origin_type: None,
        origin_id: None,
        parent_id: Some("parent-issue".to_string()),
        child_total: 3,
        child_done: 1,
        ..sample_issue()
    };
    let json = serde_json::to_string(&row).expect("encode");
    let back: IssueRow = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, row, "parent_id/child_total/child_done round-trip");

    // A top-level issue omits parent_id entirely (skip_serializing_if), and a zero
    // roll-up omits nothing that breaks an old reader.
    let top = IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        origin_type: None,
        origin_id: None,
        child_total: 0,
        child_done: 0,
        ..sample_issue()
    };
    let json = serde_json::to_string(&top).expect("encode");
    assert!(
        !json.contains("parent_id"),
        "a top-level issue omits parent_id, got {json}"
    );

    // A pre-0046 snapshot (no new keys) decodes to defaults.
    let legacy = r#"{"id":"issue-1","workspace_id":"default","title":"t","description":null,"state":"open","assignee":null,"creator":"member:alice","created_at":0}"#;
    let legacy_row: IssueRow = serde_json::from_str(legacy).expect("decode legacy");
    assert_eq!(legacy_row.parent_id, None, "default parent_id is None");
    assert_eq!(legacy_row.child_total, 0, "default child_total is 0");
    assert_eq!(legacy_row.child_done, 0, "default child_done is 0");
}

/// 0046: `IssueCreateParams` carries an optional `parent_issue_id`, omitted from
/// the wire when unset, and a pre-0046 payload (no key) decodes to `None`
/// (append-only).
#[test]
fn issue_create_params_parent_is_additive() {
    use ainb_hangar_proto::snapshots::IssueCreateParams;

    // `..Default::default()` on purpose: this fixture asserts ONE field, so a
    // later append-only field must not red-gate it (the exhaustive literal did).
    let sub = IssueCreateParams {
        workspace_id: "default".to_string(),
        title: "child".to_string(),
        creator: "member:alice".to_string(),
        parent_issue_id: Some("parent-1".to_string()),
        ..Default::default()
    };
    let json = serde_json::to_string(&sub).expect("encode");
    let back: IssueCreateParams = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.parent_issue_id.as_deref(), Some("parent-1"));

    // A top-level create omits the key entirely.
    let top = IssueCreateParams {
        parent_issue_id: None,
        ..sub
    };
    let json = serde_json::to_string(&top).expect("encode");
    assert!(
        !json.contains("parent_issue_id"),
        "omitted when None, got {json}"
    );

    // A pre-0046 payload decodes parent_issue_id to None.
    let legacy = r#"{"workspace_id":"default","title":"t","creator":"member:alice"}"#;
    let legacy_row: IssueCreateParams = serde_json::from_str(legacy).expect("decode legacy");
    assert_eq!(
        legacy_row.parent_issue_id, None,
        "default parent_issue_id is None"
    );
}

/// Parity 28: `parse_calendar_date_ms` is the ONE calendar-date parser every
/// hangar client uses — exact `YYYY-MM-DD`, UTC midnight, loud on anything else
/// (multica's `util.ParseCalendarDate` contract).
#[test]
fn calendar_date_parses_at_utc_midnight_and_rejects_other_shapes() {
    use ainb_hangar_proto::dates::parse_calendar_date_ms;

    assert_eq!(
        parse_calendar_date_ms("2026-08-01"),
        Ok(1_785_542_400_000),
        "2026-08-01 is UTC midnight epoch ms"
    );
    assert_eq!(parse_calendar_date_ms("1970-01-01"), Ok(0));
    for bad in ["31-12-2026", "2026-13-01", "", "2026/08/01"] {
        assert!(
            parse_calendar_date_ms(bad).is_err(),
            "{bad:?} must be rejected, never coerced to a silent no-due-date"
        );
    }
}

/// Parity 28: `priority` / `due_date` / `labels` are append-only on
/// `IssueCreateParams` — absent from the wire when defaulted, and a pre-28
/// payload decodes them to `None` / `None` / `[]`.
#[test]
fn issue_create_params_priority_due_labels_are_additive() {
    use ainb_hangar_proto::snapshots::IssueCreateParams;

    let rich = IssueCreateParams {
        workspace_id: "default".to_string(),
        title: "urgent".to_string(),
        creator: "member:alice".to_string(),
        priority: Some(3),
        due_date: Some(1_785_542_400_000),
        labels: vec!["bug".to_string(), "p0".to_string()],
        ..Default::default()
    };
    let json = serde_json::to_string(&rich).expect("encode");
    let back: IssueCreateParams = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.priority, Some(3));
    assert_eq!(back.due_date, Some(1_785_542_400_000));
    assert_eq!(back.labels, vec!["bug".to_string(), "p0".to_string()]);

    // An unadorned create's wire shape is byte-identical to pre-28.
    let plain = IssueCreateParams {
        priority: None,
        due_date: None,
        labels: Vec::new(),
        ..rich
    };
    let json = serde_json::to_string(&plain).expect("encode");
    for key in ["priority", "due_date", "labels"] {
        assert!(
            !json.contains(key),
            "{key} must be omitted when unset, got {json}"
        );
    }

    // A pre-28 payload decodes to the schema defaults.
    let legacy = r#"{"workspace_id":"default","title":"t","creator":"member:alice"}"#;
    let legacy_row: IssueCreateParams = serde_json::from_str(legacy).expect("decode legacy");
    assert_eq!(legacy_row.priority, None, "default priority is None (P3)");
    assert_eq!(legacy_row.due_date, None, "default due_date is None");
    assert!(legacy_row.labels.is_empty(), "default labels is empty");
}

#[test]
fn task_result_variants_roundtrip() {
    for (r, wire) in [
        (TaskResult::Success, "success"),
        (TaskResult::Failure, "failure"),
        (TaskResult::Cancelled, "cancelled"),
    ] {
        let v = serde_json::to_value(r).expect("encode");
        assert_eq!(v, serde_json::json!(wire));
        let back: TaskResult = serde_json::from_value(v).expect("decode");
        assert_eq!(back, r);
    }
}

// ---- ORIGIN PROVENANCE wire back-compat (migration 0056, parity #21) --------

/// A pre-0056 snapshot carries no `origin_*` keys at all: it must decode, not
/// fail, and read as "provenance unknown".
#[test]
fn issue_row_without_origin_keys_decodes_to_none() {
    let mut json = serde_json::to_value(sample_issue()).unwrap();
    let obj = json.as_object_mut().unwrap();
    obj.remove("origin_type");
    obj.remove("origin_id");
    let decoded: IssueRow = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.origin_type, None);
    assert_eq!(decoded.origin_id, None);
}

/// An unstamped row does not GROW the wire: the keys are omitted entirely, so
/// an old reader sees byte-identical JSON to what it saw pre-0056.
#[test]
fn issue_row_with_no_origin_omits_both_keys() {
    let json = serde_json::to_value(sample_issue()).unwrap();
    let obj = json.as_object().unwrap();
    assert!(!obj.contains_key("origin_type"));
    assert!(!obj.contains_key("origin_id"));
}

/// A stamped row round-trips both halves.
#[test]
fn issue_row_with_origin_round_trips() {
    let mut row = sample_issue();
    row.origin_type = Some("comment_mention".to_string());
    row.origin_id = Some("c-7".to_string());
    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("\"origin_type\":\"comment_mention\""));
    let back: IssueRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back, row);
}

/// `manual` carries a kind and no id — the id key stays off the wire.
#[test]
fn manual_origin_serialises_the_kind_without_an_id() {
    let mut row = sample_issue();
    row.origin_type = Some("manual".to_string());
    let json = serde_json::to_value(&row).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(
        obj.get("origin_type").and_then(|v| v.as_str()),
        Some("manual")
    );
    assert!(!obj.contains_key("origin_id"));
    let back: IssueRow = serde_json::from_value(json).unwrap();
    assert_eq!(back, row);
}

/// The CREATE params are append-only in the same way: an old client's payload
/// (no `origin_*`) decodes with both halves absent, which the daemon reads as
/// "stamp `manual`".
#[test]
fn issue_create_params_origin_is_append_only() {
    use ainb_hangar_proto::snapshots::IssueCreateParams;

    let old_client = serde_json::json!({
        "workspace_id": "default",
        "title": "t",
        "creator": "member:u-1",
    });
    let decoded: IssueCreateParams = serde_json::from_value(old_client).unwrap();
    assert_eq!(decoded.origin_type, None);
    assert_eq!(decoded.origin_id, None);

    let stamped = serde_json::json!({
        "workspace_id": "default",
        "title": "t",
        "creator": "member:u-1",
        "origin_type": "autopilot",
        "origin_id": "ap-1",
    });
    let decoded: IssueCreateParams = serde_json::from_value(stamped).unwrap();
    assert_eq!(decoded.origin_type.as_deref(), Some("autopilot"));
    assert_eq!(decoded.origin_id.as_deref(), Some("ap-1"));
}
