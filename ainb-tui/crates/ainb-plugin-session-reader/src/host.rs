//! Safe Rust wrappers around the host-fn imports.
//!
//! Plugins talk to the host through `extern "C"` imports declared in
//! `ainb_plugin_api::host`. Those functions take raw `i32` pointer +
//! length pairs — direct callers have to allocate buffers, retry on
//! `BufferTooSmall`, and decode `HostStatus`. This module hides that
//! layer behind idiomatic Rust signatures the rest of the plugin uses.
//!
//! Errors map onto a single [`HostError`] enum. Buffer growth follows
//! a doubling strategy capped by [`MAX_RESPONSE_BYTES`] so a runaway
//! host fn doesn't OOM the wasm linear memory.

use ainb_plugin_api::host::{HostStatus, LogLevel};

/// Hard ceiling on a single host-fn response (8 MiB).
///
/// Rationale: the largest expected payload is the full `UsageData`
/// snapshot encoded as msgpack — well under 1 MiB on any realistic
/// install. The 8 MiB headroom is paranoia, not capacity planning.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Initial buffer size for grow-and-retry calls (32 KiB).
const INITIAL_BUFFER_BYTES: usize = 32 * 1024;

/// Errors returned by every safe wrapper in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// Plugin asked the host for something outside its capabilities or
    /// the host's allowlist (e.g. a path outside `~/.claude/projects`).
    NotPermitted,
    /// One of the wrapper inputs was malformed (e.g. a non-utf8 path).
    InvalidArgument,
    /// Generic host-side failure; details (when available) live in the
    /// host's log via `ainb_log`.
    HostError,
    /// Response would have exceeded [`MAX_RESPONSE_BYTES`].
    ResponseTooLarge,
}

impl HostError {
    fn from_status(status: HostStatus) -> Self {
        match status {
            HostStatus::NotPermitted => Self::NotPermitted,
            HostStatus::InvalidArgument => Self::InvalidArgument,
            HostStatus::Ok | HostStatus::BufferTooSmall | HostStatus::HostError => Self::HostError,
        }
    }
}

/// Convenience alias for the wrapper module.
pub type Result<T> = core::result::Result<T, HostError>;

/// Log a line to the host. Best-effort: failures are swallowed because
/// logging is allowed to be unreliable.
pub fn log(level: LogLevel, msg: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        ainb_plugin_api::host::ainb_log(
            level as i32,
            msg.as_ptr() as i32,
            i32::try_from(msg.len()).unwrap_or(0),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (level, msg);
    }
}

/// Wall-clock unix milliseconds. Falls back to 0 on a non-wasm build
/// so unit tests of the wrappers still work.
pub fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        ainb_plugin_api::host::ainb_now_ms()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// `ainb_fs_read_dir(path) -> Vec<String>`. The host returns canonical
/// absolute paths joined by `\n`; we split into a Vec.
pub fn fs_read_dir(path: &str) -> Result<Vec<String>> {
    let bytes = call_with_growable_buffer(|out_ptr, out_len| unsafe {
        #[cfg(target_arch = "wasm32")]
        {
            ainb_plugin_api::host::ainb_fs_read_dir(
                path.as_ptr() as i32,
                i32::try_from(path.len()).unwrap_or(0),
                out_ptr,
                out_len,
            )
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (path, out_ptr, out_len);
            HostStatus::HostError as i32
        }
    })?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// `ainb_fs_read_file(path) -> Vec<u8>`. Bytes are returned as-is.
pub fn fs_read_file(path: &str) -> Result<Vec<u8>> {
    call_with_growable_buffer(|out_ptr, out_len| unsafe {
        #[cfg(target_arch = "wasm32")]
        {
            ainb_plugin_api::host::ainb_fs_read_file(
                path.as_ptr() as i32,
                i32::try_from(path.len()).unwrap_or(0),
                out_ptr,
                out_len,
            )
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (path, out_ptr, out_len);
            HostStatus::HostError as i32
        }
    })
}

/// `ainb_cache_get(key) -> Option<Vec<u8>>`. `Ok(None)` distinguishes
/// "key unset" from "value happened to be empty".
pub fn cache_get(key: &str) -> Result<Option<Vec<u8>>> {
    let bytes = call_with_growable_buffer(|out_ptr, out_len| unsafe {
        #[cfg(target_arch = "wasm32")]
        {
            ainb_plugin_api::host::ainb_cache_get(
                key.as_ptr() as i32,
                i32::try_from(key.len()).unwrap_or(0),
                out_ptr,
                out_len,
            )
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (key, out_ptr, out_len);
            HostStatus::HostError as i32
        }
    })?;
    // The host returns 0 bytes for an unset key (or genuinely-empty
    // value). The session-reader stores msgpack blobs which are never
    // empty for a valid `UsageData`, so the distinction collapses.
    if bytes.is_empty() {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

/// `ainb_cache_put(key, val, ttl_secs)`.
pub fn cache_put(key: &str, value: &[u8], ttl_secs: i64) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let rc = unsafe {
        ainb_plugin_api::host::ainb_cache_put(
            key.as_ptr() as i32,
            i32::try_from(key.len()).unwrap_or(0),
            value.as_ptr() as i32,
            i32::try_from(value.len()).unwrap_or(0),
            ttl_secs,
        )
    };
    #[cfg(not(target_arch = "wasm32"))]
    let rc = {
        let _ = (key, value, ttl_secs);
        HostStatus::HostError as i32
    };
    if rc >= 0 {
        Ok(())
    } else {
        Err(HostError::from_status(HostStatus::from_i32(rc)))
    }
}

/// `ainb_event_publish(topic, payload)`. Fire-and-forget.
pub fn event_publish(topic: &str, payload: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        ainb_plugin_api::host::ainb_event_publish(
            topic.as_ptr() as i32,
            i32::try_from(topic.len()).unwrap_or(0),
            payload.as_ptr() as i32,
            i32::try_from(payload.len()).unwrap_or(0),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (topic, payload);
    }
}

/// `ainb_event_subscribe(topic)`.
pub fn event_subscribe(topic: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        ainb_plugin_api::host::ainb_event_subscribe(
            topic.as_ptr() as i32,
            i32::try_from(topic.len()).unwrap_or(0),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = topic;
    }
}

/// `ainb_publish_reply(corr_id, payload)`. Host parks the bytes in the
/// reply ledger so the requester's blocking `ainb_request_data` call
/// wakes up. Best-effort: failures (e.g. invalid bytes) are logged via
/// the host but never panic the plugin.
pub fn publish_reply(correlation_id: u64, payload: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        let _ = ainb_plugin_api::host::ainb_publish_reply(
            correlation_id,
            payload.as_ptr() as i32,
            i32::try_from(payload.len()).unwrap_or(0),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (correlation_id, payload);
    }
}

/// Drive a host fn that writes bytes into a caller-allocated buffer.
/// Doubles the buffer on `BufferTooSmall` until either the call
/// succeeds or [`MAX_RESPONSE_BYTES`] is exceeded.
fn call_with_growable_buffer<F>(mut call: F) -> Result<Vec<u8>>
where
    F: FnMut(i32, i32) -> i32,
{
    let mut cap = INITIAL_BUFFER_BYTES;
    loop {
        let mut buf = vec![0_u8; cap];
        let rc = call(
            buf.as_mut_ptr() as i32,
            i32::try_from(cap).unwrap_or(i32::MAX),
        );
        if rc >= 0 {
            let written = usize::try_from(rc).unwrap_or(0);
            buf.truncate(written);
            return Ok(buf);
        }
        let status = HostStatus::from_i32(rc);
        if status == HostStatus::BufferTooSmall {
            // Double and retry up to the cap.
            if cap >= MAX_RESPONSE_BYTES {
                return Err(HostError::ResponseTooLarge);
            }
            cap = (cap * 2).min(MAX_RESPONSE_BYTES);
            continue;
        }
        return Err(HostError::from_status(status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_growth_caps_at_max_response_bytes() {
        // Always returns BufferTooSmall — should error out at the cap.
        let mut calls = 0_u32;
        let result = call_with_growable_buffer(|_p, _l| {
            calls += 1;
            HostStatus::BufferTooSmall as i32
        });
        assert_eq!(result, Err(HostError::ResponseTooLarge));
        // Initial 32 KiB → doubles to MAX (8 MiB) over a bounded
        // number of iterations. The exact count isn't load-bearing,
        // but it should be bounded and reasonable (< 20).
        assert!((1..20).contains(&calls), "got {calls} retries");
    }

    #[test]
    fn first_call_success_returns_truncated_bytes() {
        let result = call_with_growable_buffer(|_p, _l| 5);
        assert!(matches!(result, Ok(ref v) if v.len() == 5));
    }

    #[test]
    fn not_permitted_propagates() {
        let result = call_with_growable_buffer(|_p, _l| HostStatus::NotPermitted as i32);
        assert_eq!(result, Err(HostError::NotPermitted));
    }

    #[test]
    fn invalid_argument_propagates() {
        let result = call_with_growable_buffer(|_p, _l| HostStatus::InvalidArgument as i32);
        assert_eq!(result, Err(HostError::InvalidArgument));
    }
}
