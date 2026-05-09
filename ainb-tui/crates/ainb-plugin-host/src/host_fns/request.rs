//! Synchronous request/response over the event bus.
//!
//! Wire shape:
//! `ainb_request_data(topic, payload, timeout_ms, out) -> i32`
//!
//! Returns the byte count written into `out`, or a negative
//! [`HostStatus`].
//!
//! Semantics (foundation drop — Phase 6c-cli expands this):
//!
//! 1. The host fn allocates a fresh correlation id.
//! 2. It enqueues a request event on `req:<topic>` with the correlation
//!    id + caller payload encoded in the queue payload.
//! 3. It then polls `HostShared::take_reply(corr_id)` with a tiny sleep
//!    between polls until either a reply lands or the deadline passes.
//!
//! Population of replies is the responsibility of the cross-plugin
//! event pump: a subscriber plugin replies on `rep:<topic>` with the
//! correlation id, and `pump_events` (host main loop) routes that into
//! `HostShared::pending_replies`. Phase 6c-cli wires the loop;
//! Phase 6a (this commit) wires the host-fn primitive + the reply
//! ledger so the wiring layer has somewhere to drop bytes.

use std::time::{Duration, Instant};

use ainb_plugin_api::host::{HostStatus, HOST_MODULE};
use ainb_plugin_api::CapabilitySet;
use anyhow::Result;
use wasmi::{Caller, Linker};

use crate::runtime::HostState;

use super::{read_bytes, read_string};

/// Wire `ainb_request_data` and `ainb_publish_reply` into `linker` if
/// the plugin holds `event_bus`. Without that capability the imports
/// stay unresolved (axis-6 static refusal).
pub fn link(linker: &mut Linker<HostState>, caps: &CapabilitySet) -> Result<()> {
    if !caps.event_bus {
        return Ok(());
    }
    linker.func_wrap(
        HOST_MODULE,
        "ainb_request_data",
        |mut caller: Caller<'_, HostState>,
         topic_ptr: i32,
         topic_len: i32,
         payload_ptr: i32,
         payload_len: i32,
         timeout_ms: i32,
         out_ptr: i32,
         out_len: i32|
         -> i32 {
            request_data_impl(
                &mut caller,
                topic_ptr,
                topic_len,
                payload_ptr,
                payload_len,
                timeout_ms,
                out_ptr,
                out_len,
            )
        },
    )?;
    linker.func_wrap(
        HOST_MODULE,
        "ainb_publish_reply",
        |mut caller: Caller<'_, HostState>,
         correlation_id: u64,
         payload_ptr: i32,
         payload_len: i32|
         -> i32 { publish_reply_impl(&mut caller, correlation_id, payload_ptr, payload_len) },
    )?;
    Ok(())
}

/// Park a reply payload keyed by `correlation_id` so the requester's
/// blocking `ainb_request_data` call wakes up. Returns 0 on success or
/// a negative [`HostStatus`] on bad input.
fn publish_reply_impl(
    caller: &mut Caller<'_, HostState>,
    correlation_id: u64,
    payload_ptr: i32,
    payload_len: i32,
) -> i32 {
    let payload = match read_bytes(caller, payload_ptr, payload_len) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_publish_reply: read payload: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    caller.data().shared.put_reply(correlation_id, payload);
    0
}

/// Implements `ainb_request_data`: synchronously emit a `req:<topic>`
/// event and busy-poll the reply ledger until either a reply lands or
/// the deadline passes.
///
/// ## Limitations
///
/// **The busy-poll runs on the same single thread as
/// [`crate::PluginHost::pump_events`].** That has two consequences:
///
/// 1. **Calling this from inside a plugin's `_handle_event` or `_init`
///    deadlocks.** The host can't drain the event queue while a wasmi
///    call is in flight, so the `req:<topic>` event sits in the queue
///    forever and the loop here ticks until `timeout_ms` elapses with
///    no progress.
/// 2. The CLI dispatch path **does not call this fn for the
///    cli↔session-reader handshake**; instead it uses the broker
///    workaround in
///    [`ainb_core::cli::registry::inject_session_reader_snapshot`],
///    which pre-injects the snapshot before the plugin's
///    `_handle_event` fires. Phase 7+ async pump lifts this constraint
///    by running `pump_events` on a separate thread.
///
/// Safe to call only from the TUI event loop's idle gap (between
/// frames, after `pump_events` has run at least once). When no other
/// plugin is subscribed to anything we emit a one-shot `tracing::warn!`
/// before entering the poll so the user gets a fast signal that the
/// request will time out.
fn request_data_impl(
    caller: &mut Caller<'_, HostState>,
    topic_ptr: i32,
    topic_len: i32,
    payload_ptr: i32,
    payload_len: i32,
    timeout_ms: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    if timeout_ms < 0 {
        caller.data_mut().last_error = Some("ainb_request_data: negative timeout".into());
        return HostStatus::InvalidArgument as i32;
    }
    let topic = match read_string(caller, topic_ptr, topic_len) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            caller.data_mut().last_error = Some("ainb_request_data: empty topic".into());
            return HostStatus::InvalidArgument as i32;
        }
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_request_data: bad topic utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let payload = match read_bytes(caller, payload_ptr, payload_len) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_request_data: read payload: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let plugin_id = caller.data().plugin_id.clone();
    let shared = caller.data().shared.clone();

    // Fail loud when no other plugin is subscribed to anything: the
    // poll loop below would otherwise spin until `timeout_ms` elapsed
    // with no chance of a reply landing. This is the most common
    // misconfiguration (session-reader missing from `dist/plugins/`)
    // and surfacing it once at request time saves the user a
    // multi-second hang.
    if shared
        .subscriptions
        .lock()
        .map(|m| m.values().all(Vec::is_empty))
        .unwrap_or(false)
    {
        tracing::warn!(
            requester = %plugin_id,
            topic = %topic,
            "ainb_request_data: no subscriptions exist; request will time out"
        );
    }

    let corr_id = shared.next_correlation_id();
    // Mark inflight BEFORE publishing the request: a same-thread
    // pump_events between publish and mark would otherwise drop the
    // reply on the floor (put_reply rejects unknown corr_ids).
    shared.mark_inflight(corr_id);
    // Encode the request as `<u64 LE corr_id><payload bytes>` on
    // `req:<topic>` so the pump can route the reply by id.
    let mut wire = Vec::with_capacity(8 + payload.len());
    wire.extend_from_slice(&corr_id.to_le_bytes());
    wire.extend_from_slice(&payload);
    shared.publish(&plugin_id, format!("req:{topic}"), wire);

    // Poll for the reply with a small back-off. The deadline can be
    // zero (one-shot probe); we always check at least once.
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        if let Some(reply) = shared.take_reply(corr_id) {
            shared.clear_inflight(corr_id);
            return write_bytes_to_caller(caller, out_ptr, out_len, &reply);
        }
        if Instant::now() >= deadline {
            shared.clear_inflight(corr_id);
            caller.data_mut().last_error =
                Some(format!("ainb_request_data {topic:?}: timeout after {timeout_ms}ms"));
            return HostStatus::HostError as i32;
        }
        // Yielding sleep: enough that we don't spin a core, short
        // enough that a sub-plugin reply lands quickly. The wasmi
        // call runs on the host main thread; the reply has to come
        // from a different thread (or an explicit pump_events call
        // wired in Phase 6c-cli) for this loop to ever see it.
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn write_bytes_to_caller(
    caller: &mut Caller<'_, HostState>,
    out_ptr: i32,
    out_len: i32,
    bytes: &[u8],
) -> i32 {
    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        return HostStatus::BufferTooSmall as i32;
    }
    let memory = match caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
    {
        Some(m) => m,
        None => return HostStatus::HostError as i32,
    };
    let off = match usize::try_from(out_ptr) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if memory.write(&mut *caller, off, bytes).is_err() {
        return HostStatus::HostError as i32;
    }
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::HostShared;

    #[test]
    fn correlation_ids_are_unique() {
        let s = HostShared::default();
        let a = s.next_correlation_id();
        let b = s.next_correlation_id();
        assert_ne!(a, b);
    }

    #[test]
    fn put_then_take_reply_round_trips() {
        let s = HostShared::default();
        let id = s.next_correlation_id();
        // Mark inflight first so put_reply accepts the id.
        s.mark_inflight(id);
        s.put_reply(id, b"hello".to_vec());
        assert_eq!(s.take_reply(id), Some(b"hello".to_vec()));
        // Second take is empty — replies aren't re-deliverable.
        assert_eq!(s.take_reply(id), None);
    }

    #[test]
    fn timeout_clears_inflight_and_drops_late_reply() {
        let s = HostShared::default();
        let id = s.next_correlation_id();
        s.mark_inflight(id);
        // Simulate timeout: caller gives up and clears its slot.
        s.clear_inflight(id);
        // A late reply for the canceled id is dropped.
        s.put_reply(id, b"late".to_vec());
        assert_eq!(s.take_reply(id), None);
    }

    #[test]
    fn reply_with_corr_id_zero_is_dropped() {
        let s = HostShared::default();
        // 0 is the broker sentinel meaning "one-way push, no reply
        // expected". Even if something marks it inflight, put_reply
        // rejects it.
        s.mark_inflight(0);
        s.put_reply(0, b"sentinel".to_vec());
        assert_eq!(s.take_reply(0), None);
    }

    #[test]
    fn reply_for_uninflight_id_is_dropped() {
        let s = HostShared::default();
        // No mark_inflight call: corresponds to a reply landing for a
        // request the host never made (or already cleared).
        s.put_reply(42, b"orphan".to_vec());
        assert_eq!(s.take_reply(42), None);
    }
}
