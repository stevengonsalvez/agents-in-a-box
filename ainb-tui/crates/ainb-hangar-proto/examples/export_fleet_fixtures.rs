//! Export deterministic Fleet protocol fixtures for cross-language decoders.

use std::{env, fs, path::PathBuf};

use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, auth, fleet::*, jsonrpc_version, methods};
use serde_json::{Value, json};

const FIXTURE_DIRECTORY: &str = "../../fleet-parity/fixtures/v1";

fn fixture_session() -> FleetSession {
    FleetSession {
        session_key: "fleet-session-001".to_string(),
        provider: FleetProvider::Codex,
        provider_session_id: Some("provider-session-001".to_string()),
        tmux_target: Some("fleet:1.0".to_string()),
        process_start_fingerprint: Some("proc-001".to_string()),
        cwd: "/workspace/fleet-fixture".to_string(),
        display_name: Some("Fixture Fleet".to_string()),
        lifecycle: LifecycleState::Running,
        attention: AttentionState::Ask,
        current_request_fingerprint: Some("sha256:request-001".to_string()),
        current_request: Some(json!({"kind": "question", "opaque": "fixture"})),
        management: ManagementState::Managed,
        transport_health: TransportHealth::Healthy,
        capabilities: FleetCapabilities {
            structured_answer: true,
            approvals: true,
            send_prompt: true,
            continue_turn: true,
            retry: true,
            interrupt: true,
            start: false,
            stop: true,
            restart: true,
            kill: true,
            archive: true,
            tmux_attach: true,
            tmux_text: true,
            verified_picker: true,
        },
        provenance: FleetProvenance::Authoritative,
        confidence: FleetConfidence::High,
        discovered_at: 1_700_000_000_000,
        last_observed_at: 1_700_000_000_100,
        lifecycle_updated_at: 1_700_000_000_050,
        attention_updated_at: 1_700_000_000_075,
        version: 7,
        updated_revision: 42,
    }
}

fn fixture_snapshot() -> FleetSnapshot {
    FleetSnapshot {
        head_revision: 42,
        sessions: vec![fixture_session()],
    }
}

fn fixture_event() -> FleetEvent {
    FleetEvent {
        revision: 42,
        event_id: "fleet-event-042".to_string(),
        session_key: "fleet-session-001".to_string(),
        observed_at: 1_700_000_000_100,
        provenance: FleetProvenance::Authoritative,
        event_type: "request_raised".to_string(),
        payload: json!({"opaque": "fixture-event"}),
        session_version: 7,
        applied: true,
    }
}

fn fixture_receipt() -> FleetActionReceipt {
    FleetActionReceipt {
        request_id: "action-request-001".to_string(),
        session_key: "fleet-session-001".to_string(),
        action_kind: "structured_answer".to_string(),
        action_fingerprint: "sha256:action-001".to_string(),
        expected_version: 7,
        idempotency_key: Some("broadcast-001".to_string()),
        status: ActionReceiptStatus::Delivered,
        detail: Some("fixture delivery".to_string()),
        session_version: Some(8),
        created_at: 1_700_000_000_200,
        updated_at: 1_700_000_000_300,
    }
}

fn fixture_action() -> FleetActionParams {
    FleetActionParams {
        session_key: "fleet-session-001".to_string(),
        expected_version: 7,
        request_id: "action-request-001".to_string(),
        action: ControlAction::StructuredAnswer {
            request_fingerprint: "sha256:request-001".to_string(),
            request_identity: Some(FleetRequestIdentity {
                request_id: json!(73),
                thread_id: "thread-001".to_string(),
                turn_id: "turn-001".to_string(),
                item_id: "item-001".to_string(),
            }),
            answers: vec![FleetQuestionAnswer {
                question_id: "question-001".to_string(),
                selected_options: vec!["option-a".to_string()],
                text: Some("fixture answer".to_string()),
            }],
        },
    }
}

/// Canonical, fixed-value Fleet fixtures in stable filename order.
#[must_use]
pub fn fixtures() -> Vec<(&'static str, Value)> {
    let compatible = FleetNegotiateResult {
        daemon_version: "fixture-daemon-1.0.0".to_string(),
        protocol_version: FLEET_PROTOCOL_VERSION,
        read_compatible: true,
        write_compatible: true,
        capability_ids: FLEET_PROTOCOL_CAPABILITY_IDS.iter().map(|id| (*id).to_string()).collect(),
    };
    let read_only = FleetNegotiateResult {
        read_compatible: true,
        write_compatible: false,
        ..compatible.clone()
    };
    let snapshot = fixture_snapshot();
    let event = fixture_event();
    let receipt = fixture_receipt();
    let broadcast = FleetBroadcastParams {
        target_keys: vec![
            "fleet-session-001".to_string(),
            "fleet-session-002".to_string(),
        ],
        text: "fixture broadcast".to_string(),
        idempotency_key: "broadcast-001".to_string(),
    };

    vec![
        (
            "action-receipt.json",
            serde_json::to_value(receipt.clone()).expect("fixture serializes"),
        ),
        (
            "action-request.json",
            serde_json::to_value(fixture_action()).expect("fixture serializes"),
        ),
        (
            "auth-hello-request.json",
            serde_json::to_value(auth::hello_request(1, "mdt_fixture_token"))
                .expect("fixture serializes"),
        ),
        (
            "broadcast-request.json",
            serde_json::to_value(broadcast).expect("fixture serializes"),
        ),
        (
            "broadcast-result.json",
            serde_json::to_value(FleetBroadcastResult {
                receipts: vec![receipt],
            })
            .expect("fixture serializes"),
        ),
        (
            "fleet-event.json",
            serde_json::to_value(event.clone()).expect("fixture serializes"),
        ),
        (
            "negotiate-request.json",
            serde_json::to_value(RpcRequest {
                jsonrpc: jsonrpc_version(),
                id: RpcId::Number(2),
                method: methods::FLEET_NEGOTIATE.to_string(),
                params: serde_json::to_value(FleetNegotiateParams {
                    client_name: "ainb-fleet-macos".to_string(),
                    client_version: "0.1.0".to_string(),
                    read_versions: FleetProtocolRange { min: 1, max: 1 },
                    write_versions: FleetProtocolRange { min: 1, max: 1 },
                })
                .expect("fixture serializes"),
            })
            .expect("fixture serializes"),
        ),
        (
            "negotiate-result-compatible.json",
            serde_json::to_value(compatible).expect("fixture serializes"),
        ),
        (
            "negotiate-result-read-only.json",
            serde_json::to_value(read_only).expect("fixture serializes"),
        ),
        (
            "rpc-error.json",
            serde_json::to_value(RpcError {
                code: -32602,
                message: "invalid params".to_string(),
                data: Some(json!({"field": "after_revision"})),
            })
            .expect("fixture serializes"),
        ),
        (
            "snapshot.json",
            serde_json::to_value(snapshot.clone()).expect("fixture serializes"),
        ),
        (
            "subscribe-bootstrap.json",
            serde_json::to_value(FleetSubscribeResult {
                snapshot,
                replay: Vec::new(),
                replay_state: FleetReplayState::SnapshotReset {
                    reason: FleetReplayResetReason::Bootstrap,
                },
            })
            .expect("fixture serializes"),
        ),
        (
            "subscribe-replay.json",
            serde_json::to_value(FleetSubscribeResult {
                snapshot: fixture_snapshot(),
                replay: vec![event],
                replay_state: FleetReplayState::Complete,
            })
            .expect("fixture serializes"),
        ),
    ]
}

/// Pretty JSON bytes for one canonical fixture value.
#[must_use]
pub fn fixture_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("fixture serializes");
    bytes.push(b'\n');
    bytes
}

/// Repository fixture directory, independent of caller working directory.
#[must_use]
pub fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIRECTORY)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let check = match env::args().nth(1).as_deref() {
        None => false,
        Some("--check") => true,
        Some(argument) => return Err(format!("unknown argument: {argument}").into()),
    };
    let directory = fixture_directory();
    if !check {
        fs::create_dir_all(&directory)?;
    }
    for (filename, value) in fixtures() {
        let path = directory.join(filename);
        let expected = fixture_bytes(&value);
        if check {
            let actual = fs::read(&path)?;
            if actual != expected {
                return Err(format!("fixture drift: {}", path.display()).into());
            }
        } else {
            fs::write(path, expected)?;
        }
    }
    Ok(())
}
