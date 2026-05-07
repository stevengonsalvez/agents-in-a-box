//! WASM ABI exports for the host (host_version 1.1.0).
//!
//! Phase 3 minimum viable surface — matches the host_version 1.1.0
//! ABI shape `ainb-plugin-host` calls into:
//!
//! | export             | sig                    | semantics                                   |
//! |--------------------|------------------------|---------------------------------------------|
//! | `_init`            | `() -> i32`            | 0 = success, ≠0 = degrade plugin            |
//! | `_render`          | `() -> i32`            | host calls per frame; plugin paints via     |
//! |                    |                        | `ainb_render_buffer` host-fn (Phase 1.5     |
//! |                    |                        | stub on host side — no-op)                  |
//! | `_handle_event`    | `(i32, i32) -> i32`    | (ptr,len) = msgpack PluginEvent; 0 = ok     |
//! | `_shutdown`        | `() -> ()`             | best-effort cleanup                         |
//! | `_alloc`           | `i32 -> i32`           | host-allocator: returns guest-memory ptr    |
//! |                    |                        | for the host to write event payloads into   |
//!
//! Real rendering / event handling lands when the host wires
//! `ainb_render_buffer` and the WASI runtime needed for SQLite / fs IO
//! (current wasmi setup has no wasi-preview1 — see Phase 3 status).

use core::sync::atomic::{AtomicBool, Ordering};

use crate::data::usage::UsageData;
use crate::ui::UsageViewState;

/// Singleton plugin state. Initialised on `_init`; touched on every
/// other call. Wrapped in static-mut + a "ready" flag rather than
/// OnceLock to keep the wasm32-wasip1 binary small (no std::sync
/// thread-locals; the plugin runs single-threaded inside wasmi).
static READY: AtomicBool = AtomicBool::new(false);
static mut STATE: Option<PluginState> = None;

#[derive(Default)]
struct PluginState {
    /// In-memory UI state — populated from host events / cache reads.
    ui: UsageViewState,
    /// Most recent UsageData snapshot the host pushed in via
    /// `_handle_event(UsageDataLoaded)`. `None` until first load.
    data: Option<UsageData>,
}

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    // Construct the in-memory plugin state. No filesystem touched —
    // wasmi 0.40 (the host runtime) has no wasi-preview1 support yet,
    // so any std::fs / std::env call would trap. SQLite migration +
    // cache open are deferred until the host wires WASI; until then
    // the plugin renders against host-provided UsageData snapshots.
    unsafe {
        STATE = Some(PluginState::default());
    }
    READY.store(true, Ordering::Release);
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    // Phase 1.5: ainb_render_buffer is still a host-side stub (no-op),
    // and the WireBuffer round-trip lands when the host wires the
    // render channel. Until then, _render is a successful no-op so the
    // host's per-frame call doesn't degrade the plugin.
    0
}

#[no_mangle]
pub extern "C" fn _handle_event(_ptr: i32, _len: i32) -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    // Event payload decoding (msgpack -> PluginEvent) lands when
    // ainb-core's event bus wires us in. For now, accept all events
    // as a no-op so the host's pump_events loop doesn't see traps.
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {
    READY.store(false, Ordering::Release);
    unsafe {
        STATE = None;
    }
}

/// Host-allocator: gives the host a buffer in guest memory it can
/// write event payloads into before calling `_handle_event(ptr, len)`.
///
/// `Box::leak` is intentional — the host owns the lifetime; the plugin
/// receives the pointer back through `_handle_event` and must drop it
/// (today: implicit via the pre-allocated arena once we add one).
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
