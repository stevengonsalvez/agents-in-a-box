// Axis 5 canary: declares a `provides.providers` entry.
//
// Probes that the host's manifest loader accepts the `providers` field
// and surfaces it through the registry without requiring a screen,
// statusline, or command surface. The plugin itself does nothing —
// receipt of the init log proves the manifest parsed and the wasm
// loaded.
//
// Anti-cheat sentinel: per-canary UUID logged from _init proves this
// exact wasm executed.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
}

const INIT_MARKER: &[u8] = b"cts-5d2c81f6-provider-only-init-OK";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
