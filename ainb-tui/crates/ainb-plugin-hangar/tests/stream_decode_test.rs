//! P4.2 stream-client tests: notification-frame decoding, partial reads,
//! reconnect-with-backoff replaying the subscribe, and graceful handling of an
//! unknown event kind.
//!
//! The [`StreamClient`] is the pure, IO-free heart of the daemon event-stream
//! wrapper. It takes raw socket byte chunks (fed by the plugin glue from the
//! host `unix_socket_dial` cap), reassembles Content-Length JSON-RPC
//! notification frames, decodes each into a typed [`HangarEvent`], and yields
//! them in order. Reconnect/backoff is modelled as pure state so it is
//! exhaustively testable without a real socket or tokio.

use ainb_hangar_proto::events::{HangarEvent, MessageKind};
use ainb_plugin_hangar::stream::{Backoff, StreamClient, StreamError};

/// Frame an arbitrary JSON-RPC notification body in Content-Length framing,
/// the same wire format the daemon emits.
fn notification_frame(method: &str, params: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
    let bytes = serde_json::to_vec(&body).unwrap();
    let mut out = format!("Content-Length: {}\r\n\r\n", bytes.len()).into_bytes();
    out.extend_from_slice(&bytes);
    out
}

/// A `hangar/event` notification carrying the given event payload.
fn event_frame(ev: &HangarEvent) -> Vec<u8> {
    notification_frame("hangar/event", &serde_json::to_value(ev).unwrap())
}

fn sample_message() -> HangarEvent {
    HangarEvent::TaskMessage {
        task_id: ainb_hangar_core::ids::TaskId::from_str("task-1").unwrap(),
        kind: MessageKind::Agent,
        body: "hello".to_string(),
    }
}

#[test]
fn stream_decodes_a_whole_event_frame() {
    let mut client = StreamClient::new("sub-1");
    let ev = sample_message();
    let out = client.push(&event_frame(&ev)).expect("decode");
    assert_eq!(out, vec![ev]);
}

#[test]
fn stream_decodes_progressive_partial_frames() {
    // The host may split a single frame across multiple reads. The client must
    // buffer and only yield once the whole body has arrived.
    let mut client = StreamClient::new("sub-1");
    let ev = sample_message();
    let frame = event_frame(&ev);

    // Feed the frame one byte at a time; nothing yields until the last byte.
    for (i, b) in frame.iter().enumerate() {
        let out = client.push(&[*b]).expect("decode");
        if i + 1 < frame.len() {
            assert!(out.is_empty(), "yielded early at byte {i}");
        } else {
            assert_eq!(out, vec![ev.clone()], "final byte should complete frame");
        }
    }
}

#[test]
fn stream_yields_two_coalesced_frames_in_order() {
    let mut client = StreamClient::new("sub-1");
    let a = HangarEvent::TaskProgress {
        task_id: ainb_hangar_core::ids::TaskId::from_str("task-1").unwrap(),
        tool_calls: 1,
        elapsed_ms: 10,
    };
    let b = sample_message();
    let mut bytes = event_frame(&a);
    bytes.extend(event_frame(&b));
    let out = client.push(&bytes).expect("decode");
    assert_eq!(out, vec![a, b]);
}

#[test]
fn unknown_event_kind_returns_decode_err_doesnt_panic() {
    // A forward-compat daemon emits an event the plugin doesn't know. This must
    // surface as a decode error, never a panic, and never silently drop.
    let mut client = StreamClient::new("sub-1");
    let frame = notification_frame(
        "hangar/event",
        &serde_json::json!({"event": "quantum_entangled", "foo": 1}),
    );
    let err = client.push(&frame).expect_err("unknown kind must error");
    assert!(matches!(err, StreamError::Decode(_)), "got {err:?}");
}

#[test]
fn non_event_notifications_are_ignored_not_errored() {
    // Frames whose method is not `hangar/event` (e.g. a daemon-level ping) are
    // simply skipped — they are not malformed.
    let mut client = StreamClient::new("sub-1");
    let frame = notification_frame("daemon/heartbeat", &serde_json::json!({}));
    let out = client.push(&frame).expect("ignore non-event");
    assert!(out.is_empty());
}

#[test]
fn stream_reconnect_on_eof_replays_subscribe() {
    // On EOF the client schedules a reconnect; reconnecting must re-issue the
    // exact subscribe request so the daemon resumes the same stream.
    let mut client = StreamClient::with_subscribe(
        "sub-1",
        "workspace/subscribe",
        serde_json::json!({"workspace_id": "default"}),
    );
    // Feed a partial frame, then EOF mid-frame: buffer must be discarded.
    client.push(b"Content-Length: 99\r\n\r\n{partial").unwrap();

    let replay = client.on_eof();
    assert_eq!(replay.method, "workspace/subscribe");
    assert_eq!(replay.params["workspace_id"], "default");

    // After reconnect the decoder is reset — a stale partial body can't poison
    // the fresh stream.
    let ev = sample_message();
    let out = client.push(&event_frame(&ev)).expect("decode after reconnect");
    assert_eq!(out, vec![ev]);
}

#[test]
fn backoff_schedule_caps_at_five_seconds() {
    // 50ms / 200ms / 1s / 5s, then holds at 5s.
    let mut b = Backoff::new();
    assert_eq!(b.next_delay().as_millis(), 50);
    assert_eq!(b.next_delay().as_millis(), 200);
    assert_eq!(b.next_delay().as_millis(), 1000);
    assert_eq!(b.next_delay().as_millis(), 5000);
    assert_eq!(b.next_delay().as_millis(), 5000, "caps at 5s");
    // A successful connect resets the schedule.
    b.reset();
    assert_eq!(b.next_delay().as_millis(), 50);
}

#[test]
fn stream_id_namespaces_distinct_subscriptions() {
    // Two subscriptions don't cross-talk: each client carries its own id.
    let a = StreamClient::new("issues");
    let b = StreamClient::new("task:42");
    assert_eq!(a.stream_id(), "issues");
    assert_eq!(b.stream_id(), "task:42");
    assert_ne!(a.stream_id(), b.stream_id());
}
