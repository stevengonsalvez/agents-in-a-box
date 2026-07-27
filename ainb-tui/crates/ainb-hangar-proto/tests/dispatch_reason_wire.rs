//! Wire-compatibility goldens for the dispatch-reason surface (multica parity #12).
//!
//! The contract these pin is APPEND-ONLY growth: a pre-#12 payload must decode
//! unchanged, and a row with nothing to report must serialize byte-identically
//! to what a pre-#12 daemon emitted. Plus the invariant the reference names
//! explicitly — the handler serializes the SAME vocabulary the service decided,
//! so `DispatchReason` round-trips serde ↔ `as_db_str` ↔ `parse`.

use ainb_hangar_core::dispatch_reason::{DispatchReason, DispatchSource};
use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::IssueRow;
use ainb_hangar_proto::methods::{ALL_METHODS, HANGAR_DISPATCH_ATTEMPTS_LIST};
use ainb_hangar_proto::snapshots::{
    BoardCardRunResult, DispatchAttemptRow, DispatchAttemptsListParams, DispatchAttemptsListResult,
};

fn bare_issue_json() -> &'static str {
    // Exactly the shape a PRE-item-12 daemon emitted: no `last_dispatch_*` keys.
    r#"{
        "id": "issue-1",
        "workspace_id": "ws-1",
        "title": "A card",
        "description": null,
        "state": "open",
        "assignee": null,
        "creator": "member:user-1",
        "created_at": 100
    }"#
}

/// A pre-item-12 `IssueRow` payload decodes, with the three new fields absent.
#[test]
fn pre_item_12_issue_row_decodes() {
    let row: IssueRow = serde_json::from_str(bare_issue_json()).expect("decode legacy row");
    assert_eq!(row.last_dispatch_reason, None);
    assert_eq!(row.last_dispatch_detail, None);
    assert_eq!(row.last_dispatch_at, None);
}

/// Byte-identical growth proof: a row with nothing to report serializes to a
/// JSON object carrying NONE of the three new keys (not `null`s).
#[test]
fn issue_row_without_a_decline_grows_by_zero_keys() {
    let row: IssueRow = serde_json::from_str(bare_issue_json()).expect("decode");
    let value = serde_json::to_value(&row).expect("serialize");
    let obj = value.as_object().expect("object");
    for key in [
        "last_dispatch_reason",
        "last_dispatch_detail",
        "last_dispatch_at",
    ] {
        assert!(!obj.contains_key(key), "{key} must be omitted when empty");
    }
}

/// …and when a decline IS present, the keys appear and round-trip.
#[test]
fn issue_row_with_a_decline_round_trips() {
    let mut row: IssueRow = serde_json::from_str(bare_issue_json()).expect("decode");
    row.last_dispatch_reason = Some(DispatchReason::RuntimeOffline.as_db_str().to_string());
    row.last_dispatch_detail = Some("runtime rt-1 is offline".to_string());
    row.last_dispatch_at = Some(1_700_000_000_000);

    let json = serde_json::to_string(&row).expect("serialize");
    assert!(
        json.contains("\"last_dispatch_reason\":\"runtime_offline\""),
        "{json}"
    );
    let back: IssueRow = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.id, IssueId::from_str("issue-1").unwrap());
    assert_eq!(
        back.last_dispatch_reason.as_deref(),
        Some("runtime_offline")
    );
    assert_eq!(
        back.last_dispatch_detail.as_deref(),
        Some("runtime rt-1 is offline")
    );
    assert_eq!(back.last_dispatch_at, Some(1_700_000_000_000));
}

/// A pre-#12 `BoardCardRunResult` (no `reason`) decodes, and an empty one
/// serializes without the new key.
#[test]
fn board_card_run_result_reason_is_append_only() {
    let legacy = r#"{"task_id":"t-1","agent_id":"a-1","runtime_id":"rt-1","mode":"headless"}"#;
    let decoded: BoardCardRunResult = serde_json::from_str(legacy).expect("decode legacy result");
    assert_eq!(decoded.reason, None);

    let json = serde_json::to_string(&decoded).expect("serialize");
    assert!(!json.contains("reason"), "{json}");

    let with_reason = BoardCardRunResult {
        reason: Some(DispatchReason::Queued.as_db_str().to_string()),
        ..decoded
    };
    let json = serde_json::to_string(&with_reason).expect("serialize");
    assert!(json.contains("\"reason\":\"queued\""), "{json}");
    let back: BoardCardRunResult = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, with_reason);
}

/// The invariant the reference states: the decider's vocabulary IS the
/// serializer's vocabulary. serde token == `as_db_str` == what `parse` accepts.
#[test]
fn dispatch_reason_round_trips_serde_db_str_and_parse() {
    for reason in DispatchReason::ALL {
        let token = serde_json::to_value(reason)
            .expect("serialize")
            .as_str()
            .expect("string")
            .to_owned();
        assert_eq!(token, reason.as_db_str());
        assert_eq!(DispatchReason::parse(&token), Some(reason));
        let back: DispatchReason =
            serde_json::from_str(&format!("\"{token}\"")).expect("deserialize");
        assert_eq!(back, reason);
    }
    for source in DispatchSource::ALL {
        let token = serde_json::to_value(source)
            .expect("serialize")
            .as_str()
            .expect("string")
            .to_owned();
        assert_eq!(token, source.as_db_str());
        assert_eq!(DispatchSource::parse(&token), Some(source));
    }
}

/// The list method is registered, and its params/result envelopes round-trip
/// with the optional fields omitted when empty.
#[test]
fn dispatch_attempts_list_envelopes_round_trip() {
    assert!(
        ALL_METHODS.contains(&HANGAR_DISPATCH_ATTEMPTS_LIST),
        "the new method must be in the wire catalogue"
    );

    let params = DispatchAttemptsListParams {
        workspace_id: "ws-1".to_string(),
        issue_id: Some("issue-1".to_string()),
        limit: Some(10),
    };
    let json = serde_json::to_string(&params).expect("serialize");
    assert_eq!(
        serde_json::from_str::<DispatchAttemptsListParams>(&json).expect("decode"),
        params
    );

    // A minimal params payload omits the optionals entirely and still decodes.
    let minimal: DispatchAttemptsListParams =
        serde_json::from_str(r#"{"workspace_id":"ws-1"}"#).expect("decode minimal");
    assert_eq!(minimal.issue_id, None);
    assert_eq!(minimal.limit, None);
    let json = serde_json::to_string(&minimal).expect("serialize");
    assert_eq!(json, r#"{"workspace_id":"ws-1"}"#);

    let result = DispatchAttemptsListResult {
        attempts: vec![DispatchAttemptRow {
            id: "att-1".to_string(),
            issue_id: Some("issue-1".to_string()),
            agent_id: None,
            runtime_id: None,
            task_id: None,
            reason: DispatchReason::TargetUnavailable.as_db_str().to_string(),
            detail: Some("no agent in this workspace to run on".to_string()),
            source: DispatchSource::Assign.as_db_str().to_string(),
            created_at: 1_700_000_000_000,
        }],
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains("\"reason\":\"target_unavailable\""), "{json}");
    assert!(json.contains("\"source\":\"assign\""), "{json}");
    assert_eq!(
        serde_json::from_str::<DispatchAttemptsListResult>(&json).expect("decode"),
        result
    );
}

/// A token this binary does not know survives the wire and simply fails to
/// decode into the enum — it is never dropped or coerced.
#[test]
fn unknown_wire_code_survives_as_raw_text() {
    let row: DispatchAttemptRow = serde_json::from_str(
        r#"{"id":"att-1","reason":"a_code_from_2027","source":"a_surface_from_2027","created_at":1}"#,
    )
    .expect("decode unknown code");
    assert_eq!(row.reason, "a_code_from_2027");
    assert_eq!(DispatchReason::parse(&row.reason), None);
    assert_eq!(DispatchSource::parse(&row.source), None);
}
