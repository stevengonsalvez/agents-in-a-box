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

    let corr_id = shared.next_correlation_id();
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
            return write_bytes_to_caller(caller, out_ptr, out_len, &reply);
        }
        if Instant::now() >= deadline {
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
        s.put_reply(id, b"hello".to_vec());
        assert_eq!(s.take_reply(id), Some(b"hello".to_vec()));
        // Second take is empty — replies aren't re-deliverable.
        assert_eq!(s.take_reply(id), None);
    }
}
