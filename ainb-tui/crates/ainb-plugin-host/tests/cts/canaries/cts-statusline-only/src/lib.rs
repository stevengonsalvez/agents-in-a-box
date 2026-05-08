// Axis 3 canary: smallest possible plugin — only a statusline segment.
//
// Probes that the host's statusline slot accepts ticks and renders a
// single chip without the plugin needing a screen. _tick is the cadence
// hook the host calls per ainb tick; the plugin paints
// RenderTarget::Statusline (2) on each call.
//
// Anti-cheat sentinel: per-canary UUID logged from _init proves this
// exact wasm executed; a tick sentinel proves _tick fired.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_render_buffer(target: i32, ptr: i32, len: i32);
}

const INIT_MARKER: &[u8] = b"cts-3a14d7e2-statusline-only-init-OK";
const TICK_MARKER: &[u8] = b"cts-3a14d7e2-statusline-only-tick-OK";

#[rustfmt::skip]
const EMPTY_BUF: &[u8] = &[
    0x83,
    0xa5, b'w', b'i', b'd', b't', b'h', 0x00,
    0xa6, b'h', b'e', b'i', b'g', b'h', b't', 0x00,
    0xa5, b'c', b'e', b'l', b'l', b's', 0x90,
];

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _tick() -> i32 {
    unsafe {
        ainb_log(2, TICK_MARKER.as_ptr() as i32, TICK_MARKER.len() as i32);
        // RenderTarget::Statusline = 2.
        ainb_render_buffer(2, EMPTY_BUF.as_ptr() as i32, EMPTY_BUF.len() as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
