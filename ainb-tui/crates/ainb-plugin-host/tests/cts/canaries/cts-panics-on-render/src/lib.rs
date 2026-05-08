// Axis 8 canary: panic during `_render` is contained.
//
// `_init` succeeds and emits the anti-cheat sentinel; `_render` traps.
// The host must mark the plugin degraded and stop calling it; subsequent
// `render_plugin` calls return `Ok(())` with no further trap.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
}

const MARKER: &[u8] = b"cts-50f7c819-panics-on-render-init-OK";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, MARKER.as_ptr() as i32, MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    core::arch::wasm32::unreachable()
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
