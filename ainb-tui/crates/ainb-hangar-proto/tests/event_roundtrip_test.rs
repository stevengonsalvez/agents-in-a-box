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
    }
}

fn sample_comment() -> CommentRow {
    CommentRow {
        id: comment_id("comment-1"),
        issue_id: issue_id("issue-1"),
        author: "member:alice".to_string(),
        body: "looks good".to_string(),
        created_at: 1_700_000_001_000,
    }
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
