//! Round-trip the SHARED chat fixture set (`fixtures/chat/*.json`).
//!
//! One fixture set, two contract suites: these same files are decoded by the
//! Swift `FleetDaemonContractTests`, so a field that drifts on one side fails
//! on the other rather than being discovered by an operator. The Rust half
//! asserts every frame decodes into its typed params/result AND re-encodes
//! byte-equivalently (key order aside), which catches a renamed field, a
//! wrongly-cased enum token, and an `Option` that serialises `null` where the
//! fixture omits the key.
//!
//! `every_fixture_is_claimed_by_a_type` is HALF the drift guard: it reads the
//! directory, so it catches a fixture added or deleted without a Rust binding,
//! and it is structurally blind to the Swift side. Its mirror,
//! `FleetDaemonContractTests.testEveryChatFixtureIsDecodedBySwift`, does the
//! same directory read against the Swift-decoded set. Both halves are needed:
//! either one alone leaves the other suite free to stop covering a frame.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ainb_hangar_proto::fleet::{
    FleetActivityClass, FleetActivityEventParams, FleetActivityListParams, FleetActivityListResult,
    FleetActivityOutcome, FleetChannelCreateParams, FleetChannelCreateResult, FleetChannelKind,
    FleetChannelListResult, FleetConfirmAnswer, FleetConfirmAnswerParams, FleetConfirmAnswerResult,
    FleetConfirmEventParams, FleetConfirmListResult, FleetConfirmState,
    FleetCopilotConfigureParams, FleetCopilotConfigureResult, FleetCopilotMode, FleetScope,
};

/// Fixture directory, shared with the Swift contract suite.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/chat")
}

fn read(name: &str) -> serde_json::Value {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Decode one fixture into `T`, assert the re-encode matches it exactly, and
/// hand the decoded value back for semantic assertions.
fn round_trip<T>(name: &str) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let original = read(name);
    let decoded: T = serde_json::from_value(original.clone())
        .unwrap_or_else(|e| panic!("decode {name} as {}: {e}", std::any::type_name::<T>()));
    let re_encoded = serde_json::to_value(&decoded).expect("encode");
    assert_eq!(re_encoded, original, "{name} does not round-trip");
    decoded
}

#[test]
fn channel_frames_round_trip_and_mint_a_channel_scope() {
    let params: FleetChannelCreateParams = round_trip("channel_create_params.json");
    assert_eq!(params.kind, FleetChannelKind::Broadcast);
    assert_eq!(params.recipients.as_ref().map(Vec::len), Some(3));

    let created: FleetChannelCreateResult = round_trip("channel_create_result.json");
    // The whole point of the method: the minted scope IS `channel:<id>`, which
    // is the prefix part 1's send validation must be taught to admit.
    assert_eq!(
        FleetScope::parse(&created.channel.scope_key),
        Some(FleetScope::Channel(created.channel.id.as_str()))
    );

    let listed: FleetChannelListResult = round_trip("channel_list_result.json");
    assert_eq!(listed.channels[0].kind, FleetChannelKind::Copilot);
    assert!(
        listed.channels[0].recipients.is_empty(),
        "a copilot channel has no recipient set; it IS the ACP session's scope"
    );
    for channel in &listed.channels {
        assert!(
            matches!(
                FleetScope::parse(&channel.scope_key),
                Some(FleetScope::Channel(_))
            ),
            "every channel scope must parse as a channel: {}",
            channel.scope_key
        );
    }
}

#[test]
fn copilot_configure_frames_round_trip_and_carry_no_permission_mode() {
    let params: FleetCopilotConfigureParams = round_trip("copilot_configure_params.json");
    // The registry KEY, not an enum token: `claude` would not name an adapter.
    assert_eq!(params.provider, "claude-agent-acp");
    assert_eq!(params.copilot_mode, Some(FleetCopilotMode::Guarded));
    assert!(params.persona.is_some());

    // The absence is the contract: a settable permission mode would be a
    // remote off-switch for the guardrails, so the wire must not carry one.
    let raw = read("copilot_configure_params.json");
    for forbidden in ["permission_mode", "permissionMode", "mode"] {
        assert!(
            raw.get(forbidden).is_none(),
            "{forbidden:?} must never appear on copilot_configure"
        );
    }

    let result: FleetCopilotConfigureResult = round_trip("copilot_configure_result.json");
    assert_eq!(result.provider, "claude-agent-acp");
    assert_eq!(result.copilot_mode, FleetCopilotMode::Guarded);
    assert!(!result.session_replaced);
    assert!(
        result.persona_set,
        "the result reports the persona, never echoes it"
    );
    assert!(
        read("copilot_configure_result.json").get("persona").is_none(),
        "the privileged persona blob must not be read back"
    );
}

#[test]
fn confirm_frames_round_trip_through_the_full_lifecycle() {
    let list: FleetConfirmListResult = round_trip("confirm_list_result.json");
    assert_eq!(list.confirms.len(), 2);
    assert_eq!(list.confirms[0].state, FleetConfirmState::Open);
    assert!(
        list.confirms[0].expires_at > list.confirms[0].created_at,
        "a card must carry a server-side expiry"
    );
    assert!(
        list.confirms[1].target_session_key.is_none(),
        "a tool that names no session omits the key rather than sending null"
    );

    let approve: FleetConfirmAnswerParams = round_trip("confirm_answer_approve_params.json");
    assert_eq!(approve.answer, FleetConfirmAnswer::Approve);

    let edit: FleetConfirmAnswerParams = round_trip("confirm_answer_edit_params.json");
    match edit.answer {
        FleetConfirmAnswer::Edit { arguments } => {
            assert_eq!(arguments["provider"], "codex");
        }
        other => panic!("expected an edit answer, got {other:?}"),
    }

    let answered: FleetConfirmAnswerResult = round_trip("confirm_answer_result.json");
    assert_eq!(answered.state, FleetConfirmState::Approved);

    // The expiry leg: a card left unanswered reaches the operator as an
    // `expired` event, not as silence.
    let event: FleetConfirmEventParams = round_trip("confirm_event.json");
    assert_eq!(event.confirm.state, FleetConfirmState::Expired);
}

#[test]
fn activity_frames_round_trip_and_page_by_commit_seq() {
    let params: FleetActivityListParams = round_trip("activity_list_params.json");
    assert_eq!(params.after_seq, Some(41));

    let result: FleetActivityListResult = round_trip("activity_list_result.json");
    assert_eq!(result.activities[0].class, FleetActivityClass::Write);
    assert_eq!(result.activities[1].outcome, FleetActivityOutcome::Expired);
    // The cursor is the commit-ordered seq of the last row on the page, never
    // a client-minted or wall-clock value (part 1's cursor rule).
    assert_eq!(
        result.next_after_seq,
        result.activities.last().map(|row| row.seq)
    );
    let seqs: Vec<i64> = result.activities.iter().map(|row| row.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "rows page in ascending commit order");

    let event: FleetActivityEventParams = round_trip("activity_event.json");
    assert_eq!(event.activity.class, FleetActivityClass::Read);
}

/// Drift guard, Rust half: the fixture directory and the bound-type list must
/// agree, so a fixture added (or deleted) without a Rust binding is RED.
///
/// It cannot see the Swift suite at all — a Swift decoder that quietly stops
/// covering a fixture leaves this test green, which is exactly why
/// `FleetDaemonContractTests.testEveryChatFixtureIsDecodedBySwift` runs the
/// same directory read on that side.
#[test]
fn every_fixture_is_claimed_by_a_type() {
    let claimed: BTreeSet<&str> = [
        "channel_create_params.json",
        "channel_create_result.json",
        "channel_list_result.json",
        "copilot_configure_params.json",
        "copilot_configure_result.json",
        "confirm_list_result.json",
        "confirm_answer_approve_params.json",
        "confirm_answer_edit_params.json",
        "confirm_answer_result.json",
        "confirm_event.json",
        "activity_list_params.json",
        "activity_list_result.json",
        "activity_event.json",
    ]
    .into_iter()
    .collect();

    let present: BTreeSet<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures/chat exists")
        .map(|entry| entry.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();

    let claimed_owned: BTreeSet<String> = claimed.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        present, claimed_owned,
        "fixtures/chat and the bound-type list disagree"
    );
}
