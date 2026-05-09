//! WASM ABI exports for the host (host_version 1.2.0).
//!
//! | export             | sig                    | semantics                                   |
//! |--------------------|------------------------|---------------------------------------------|
//! | `_init`            | `() -> i32`            | scan + cache + publish initial event        |
//! | `_render`          | `() -> i32`            | no-op (plugin has no UI)                    |
//! | `_handle_event`    | `(i32, i32) -> i32`    | (ptr,len) = msgpack PluginEvent             |
//! | `_shutdown`        | `() -> ()`             | best-effort cleanup                         |
//! | `_alloc`           | `i32 -> i32`           | host-allocator: returns guest-memory ptr    |
//!
//! Topics:
//!
//! * `sessions.usage_data` — published, payload = msgpack
//!   [`UsageDataEvent`]. Also subscribed: when a CLI consumer
//!   (burndown) calls `ainb_request_data("sessions.usage_data", …)`
//!   the host pump delivers a [`PluginEvent::Request`] here and we
//!   reply with the latest snapshot via `ainb_publish_reply`.
//! * `sessions.refresh_request` — subscribed; forces a re-scan +
//!   re-publish on receipt.
//!
//! The plugin doesn't yet consult `[paths]` from its manifest at
//! runtime — the host has no fn for surfacing manifest data back into
//! the plugin. Defaults for all four providers are hard-coded below
//! and the path_guard accepts `~/.../` form, so that's enough to walk
//! a real install. A future host fn (e.g. `ainb_paths_get(name)`)
//! would let users override paths declaratively; tracked as a
//! Phase 6+ refinement.

use ainb_plugin_api::{host::LogLevel, PluginEvent};
use ainb_plugin_types_sessions::{UsageData, UsageDataEvent, WIRE_VERSION};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::host;
use crate::scanner::{self, ProviderRoots};

/// Topic published whenever a fresh snapshot is built.
const TOPIC_USAGE_DATA: &str = "sessions.usage_data";
/// Topic the plugin subscribes to so consumers (burndown) can force a
/// re-scan after the user pressed `f`.
const TOPIC_REFRESH_REQUEST: &str = "sessions.refresh_request";

/// Cache key for the latest serialised `UsageDataEvent`. Versioned so
/// a `WIRE_VERSION` bump invalidates stale entries automatically.
const CACHE_KEY_SNAPSHOT: &str = "usage_v1.snapshot";
/// 1h TTL on the cached snapshot — matches the spec's freshness
/// requirement and the in-tree cache's user-visible refresh cadence.
const CACHE_TTL_SECS: i64 = 3600;

static READY: AtomicBool = AtomicBool::new(false);
static mut STATE: Option<PluginState> = None;

#[derive(Default)]
struct PluginState {
    /// Most recent published snapshot (kept in-memory so a tick can
    /// republish without re-running the scan).
    last_event: Option<UsageDataEvent>,
}

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe {
        STATE = Some(PluginState::default());
    }

    // Subscribe before publishing so consumers' refresh requests are
    // wired the moment the plugin reaches steady state.
    host::event_subscribe(TOPIC_REFRESH_REQUEST);
    // Also subscribe to our own publish topic so the request/response
    // pump delivers `req:sessions.usage_data` events here for the
    // synchronous CLI fetch path. The host's pub/sub side never
    // delivers a publisher's own publishes back, so this doesn't echo.
    host::event_subscribe(TOPIC_USAGE_DATA);

    // Try cache first. A warm `UsageDataEvent` can be republished
    // immediately while a background tick rescans.
    if let Ok(Some(cached)) = host::cache_get(CACHE_KEY_SNAPSHOT) {
        if let Ok(event) = rmp_serde::from_slice::<UsageDataEvent>(&cached) {
            if event.version == WIRE_VERSION {
                publish(&event);
                set_state(event);
                READY.store(true, Ordering::Release);
                return 0;
            }
        }
    }

    // Cold start: scan + publish.
    let event = build_event(false);
    publish(&event);
    cache_put_event(&event);
    set_state(event);

    READY.store(true, Ordering::Release);
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    // No UI surface. Always succeed.
    0
}

#[no_mangle]
pub extern "C" fn _handle_event(ptr: i32, len: i32) -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    let bytes = unsafe {
        if ptr <= 0 || len <= 0 {
            return 0;
        }
        let p = ptr as usize as *const u8;
        let n = len as usize;
        core::slice::from_raw_parts(p, n)
    };
    let ev: PluginEvent = match rmp_serde::from_slice(bytes) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    match ev {
        PluginEvent::Custom { topic, .. } if topic == TOPIC_REFRESH_REQUEST => {
            host::log(LogLevel::Info, "session-reader: refresh requested");
            let event = build_event(false);
            publish(&event);
            cache_put_event(&event);
            set_state(event);
            0
        }
        PluginEvent::Request {
            topic,
            correlation_id,
            ..
        } if topic == TOPIC_USAGE_DATA => {
            // Synchronous CLI fetch. Reply with the most recent snapshot
            // (or scan-on-demand if the plugin somehow has no state yet,
            // which only happens before _init has finished — guarded by
            // READY above). msgpack-encoded `UsageDataEvent`.
            let event = unsafe {
                STATE
                    .as_ref()
                    .and_then(|s| s.last_event.clone())
                    .unwrap_or_else(|| build_event(false))
            };
            match rmp_serde::to_vec_named(&event) {
                Ok(bytes) => host::publish_reply(correlation_id, &bytes),
                Err(_) => host::log(
                    LogLevel::Error,
                    "session-reader: failed to encode reply for sessions.usage_data",
                ),
            }
            0
        }
        PluginEvent::Tick { .. } => 0,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn _shutdown() {
    READY.store(false, Ordering::Release);
    unsafe {
        STATE = None;
    }
}

#[no_mangle]
pub extern "C" fn _alloc(size: i32) -> i32 {
    let n = match usize::try_from(size) {
        Ok(n) if n > 0 => n,
        _ => return 0,
    };
    let buf = vec![0_u8; n].into_boxed_slice();
    let ptr = Box::into_raw(buf) as *mut u8 as i32;
    ptr
}

fn build_event(partial: bool) -> UsageDataEvent {
    let roots = default_roots();
    let data: UsageData = scanner::scan(&roots);
    UsageDataEvent {
        version: WIRE_VERSION,
        published_ns: host::now_ms().saturating_mul(1_000_000),
        partial,
        data,
    }
}

fn default_roots() -> ProviderRoots {
    // Per the module-level note: paths are hard-coded for Phase 6b.
    // The host's path_guard handles the `~/` expansion.
    ProviderRoots {
        claude_projects: Some("~/.claude/projects".to_string()),
        codex_sessions: Some("~/.codex/sessions".to_string()),
        gemini_sessions: Some("~/.gemini/sessions".to_string()),
        copilot_sessions: Some("~/.config/github-copilot/sessions".to_string()),
    }
}

fn publish(event: &UsageDataEvent) {
    match rmp_serde::to_vec_named(event) {
        Ok(bytes) => host::event_publish(TOPIC_USAGE_DATA, &bytes),
        Err(_) => host::log(
            LogLevel::Error,
            "session-reader: failed to encode UsageDataEvent",
        ),
    }
}

fn cache_put_event(event: &UsageDataEvent) {
    let Ok(bytes) = rmp_serde::to_vec_named(event) else {
        return;
    };
    if let Err(e) = host::cache_put(CACHE_KEY_SNAPSHOT, &bytes, CACHE_TTL_SECS) {
        host::log(
            LogLevel::Warn,
            &format!("session-reader: cache_put failed: {e:?}"),
        );
    }
}

fn set_state(event: UsageDataEvent) {
    unsafe {
        if let Some(state) = STATE.as_mut() {
            state.last_event = Some(event);
        }
    }
}
