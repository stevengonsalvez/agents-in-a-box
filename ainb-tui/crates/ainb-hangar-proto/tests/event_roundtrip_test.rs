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
    CommentRow, HangarEvent, IssueRow, MessageKind, PresenceState, TaskResult,
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
        workspace_id: "default".to_string(),
        title: "Refactor API".to_string(),
        description: Some("split the monolith".to_string()),
        state: "open".to_string(),
        assignee: Some("agent:claude".to_string()),
        creator: "member:alice".to_string(),
        created_at: 1_700_000_000_000,
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
