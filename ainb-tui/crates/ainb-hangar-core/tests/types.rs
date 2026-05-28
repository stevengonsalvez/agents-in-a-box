//! Integration tests for the public domain vocabulary exported by
//! `ainb-hangar-core`: polymorphic actor parsing, strongly-typed id newtypes,
//! and the `TaskStatus` serde contract.

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::task_status::TaskStatus;

#[test]
fn actor_ref_parses_member_and_agent_forms() {
    // The two valid kinds parse.
    assert!("member:abc".parse::<ActorRef>().is_ok());
    assert!("agent:xyz".parse::<ActorRef>().is_ok());
    // An unknown kind is rejected.
    assert!("frog:abc".parse::<ActorRef>().is_err());
}

#[test]
fn workspace_id_rejects_empty_string() {
    // A populated id constructs.
    assert!(WorkspaceId::from_str("ws-1").is_ok());
    // The empty string is rejected at construction.
    assert!(WorkspaceId::from_str("").is_err());
}

#[test]
fn task_status_serializes_to_snake_case() {
    let v = serde_json::to_value(TaskStatus::Queued).expect("serialize");
    assert_eq!(v, serde_json::Value::String("queued".to_string()));

    // Round-trip a multi-word-free variant set to lock the rename rule.
    for (status, token) in [
        (TaskStatus::Queued, "queued"),
        (TaskStatus::Dispatched, "dispatched"),
        (TaskStatus::Running, "running"),
        (TaskStatus::Done, "done"),
        (TaskStatus::Failed, "failed"),
        (TaskStatus::Cancelled, "cancelled"),
    ] {
        let json = serde_json::to_value(status).expect("serialize");
        assert_eq!(json, serde_json::Value::String(token.to_string()));
        let back: TaskStatus =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, status);
    }
}
