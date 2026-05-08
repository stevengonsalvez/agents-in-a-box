// Axis 10 canary: emits a WireBuffer whose cells.len() != width*height.
//
// Probes the host's WireBuffer validator. The msgpack payload below
// declares width=2, height=2 (so cells must contain 4 cells), but the
// cells array is empty. The host must reject this buffer with a soft
// error rather than render garbage or trap.
//
// Hand-rolled msgpack:
//   fixmap of 3 entries (0x83):
//     "width"  -> 2 (positive fixint)
//     "height" -> 2
//     "cells"  -> empty fixarray (0x90)
//
// Anti-cheat sentinels: per-canary UUID logged from _init proves this
// exact wasm executed; a render sentinel proves _render fired before
// the host inspected the buffer.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_render_buffer(target: i32, ptr: i32, len: i32);
}

const INIT_MARKER: &[u8] = b"cts-a07c4b91-malformed-buffer-init-OK";
const RENDER_MARKER: &[u8] = b"cts-a07c4b91-malformed-buffer-render-OK";

#[rustfmt::skip]
const BAD_BUF: &[u8] = &[
    0x83,
    0xa5, b'w', b'i', b'd', b't', b'h', 0x02,
    0xa6, b'h', b'e', b'i', b'g', b'h', b't', 0x02,
    0xa5, b'c', b'e', b'l', b'l', b's', 0x90,
];

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    unsafe {
        ainb_log(
            2,
            RENDER_MARKER.as_ptr() as i32,
            RENDER_MARKER.len() as i32,
        );
        // RenderTarget::Sidebar = 1.
        ainb_render_buffer(1, BAD_BUF.as_ptr() as i32, BAD_BUF.len() as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
