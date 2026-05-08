// Axis 1 canary: minimal screen registration + render dispatch.
//
// Probes that the host can load a plugin, run `_init`, and call `_render`
// without traps. Anti-cheat marker: the UUID-shaped sentinel string
// emitted via `ainb_log` proves this exact wasm executed (the host
// can't fake the marker without parsing the wasm and replaying _init).
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
}

const MARKER: &[u8] = b"cts-7c8c4f01-minimal-screen-init-OK";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, MARKER.as_ptr() as i32, MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
