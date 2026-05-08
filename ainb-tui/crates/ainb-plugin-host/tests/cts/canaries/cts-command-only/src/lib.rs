// Axis 4 canary: registers a slash command, no UI surface.
//
// Probes that the host's CLI dispatcher serialises a
// PluginEvent::Command into msgpack, calls _alloc to reserve a buffer
// in this plugin's linear memory, writes the payload, then invokes
// _handle_event with the buffer pointer + length. The plugin does not
// inspect payload bytes — receipt of any non-zero-length payload is
// treated as proof the round-trip works.
//
// Anti-cheat sentinels: per-canary UUID logged from _init proves this
// exact wasm executed; RECEIVED_MARKER proves _handle_event ran with a
// real payload (so the host's dispatch path is exercised end-to-end).
#![no_std]

use core::sync::atomic::{AtomicI32, Ordering};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
}

const INIT_MARKER: &[u8] = b"cts-4b5e0d10-command-only-init-OK";
const RECEIVED_MARKER: &[u8] = b"cts-4b5e0d10-command-only-received-command";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
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

// Bump allocator the host calls before _handle_event to reserve space
// for the serialised PluginEvent. 4 KiB is more than enough for a
// Command { name, args } payload.
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
