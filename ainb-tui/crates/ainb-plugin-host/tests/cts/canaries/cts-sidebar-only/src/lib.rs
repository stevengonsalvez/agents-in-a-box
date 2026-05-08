// Axis 2 canary: sidebar-only plugin (no screen).
//
// Probes that the host happily loads a plugin whose [provides] declares
// only a sidebar entry. The plugin's _render targets RenderTarget::Sidebar
// (1) with a (width=0, height=0) buffer — the spec treats that as a
// no-op render that the host must accept without crashing, even though
// no top-level screen is provided.
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
    fn ainb_render_buffer(target: i32, ptr: i32, len: i32);
}

const INIT_MARKER: &[u8] = b"cts-2f08b9c1-sidebar-only-init-OK";

// Hand-rolled msgpack: WireBuffer { width: 0, height: 0, cells: [] }
// Layout: fixmap(3) → "width": 0 → "height": 0 → "cells": fixarray(0)
#[rustfmt::skip]
const EMPTY_BUF: &[u8] = &[
    0x83,                                        // fixmap(3)
    0xa5, b'w', b'i', b'd', b't', b'h', 0x00,    // "width": 0
    0xa6, b'h', b'e', b'i', b'g', b'h', b't', 0x00, // "height": 0
    0xa5, b'c', b'e', b'l', b'l', b's', 0x90,    // "cells": fixarray(0)
];

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    // RenderTarget::Sidebar = 1.
    unsafe { ainb_render_buffer(1, EMPTY_BUF.as_ptr() as i32, EMPTY_BUF.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
