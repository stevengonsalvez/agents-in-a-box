// Axis 12b: subscribes to `cts.ping`, logs each delivery.
//
// `_alloc` is a tiny bump allocator the host calls to reserve a buffer
// in this plugin's linear memory before writing the event payload.
// Resetting the bump pointer to zero on shutdown is unnecessary —
// pair-b lives only as long as the test does.
#![no_std]

use core::sync::atomic::{AtomicI32, Ordering};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_event_subscribe(topic_ptr: i32, topic_len: i32);
}

const INIT_MARKER: &[u8] = b"cts-eb102a73-pair-b-subscribed-cts.ping";
const RECEIVED_MARKER: &[u8] = b"cts-eb102a73-pair-b-received-event";
const TOPIC: &[u8] = b"cts.ping";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe {
        ainb_event_subscribe(TOPIC.as_ptr() as i32, TOPIC.len() as i32);
        ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn _handle_event(_ptr: i32, len: i32) -> i32 {
    if len > 0 {
        unsafe {
            ainb_log(
                2,
                RECEIVED_MARKER.as_ptr() as i32,
                RECEIVED_MARKER.len() as i32,
            );
        }
    }
    0
}

// Tiny bump allocator for the host's `_alloc` calls. A 4 KiB scratch
// buffer is plenty for Phase 1.5 event payloads.
static BUMP: [u8; 4096] = [0; 4096];
static OFFSET: AtomicI32 = AtomicI32::new(0);

#[no_mangle]
pub extern "C" fn _alloc(size: i32) -> i32 {
    let offset = OFFSET.fetch_add(size, Ordering::Relaxed);
    let cap = BUMP.len() as i32;
    if offset + size > cap {
        return 0;
    }
    BUMP.as_ptr() as i32 + offset
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
