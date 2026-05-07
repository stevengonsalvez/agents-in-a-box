//! WASM ABI exports for the host (host_version 1.1.0).
//!
//! Phase 3 scaffold: stubs that return `0` (success) so the host loader
//! can validate the cdylib's symbol surface. Real wiring lands once the
//! data + UI layers move in.

#[no_mangle]
pub extern "C" fn _init(_manifest_ptr: u32, _manifest_len: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn _render(
    _target_id: u32,
    _area_ptr: u32,
    _area_len: u32,
    _out_ptr: u32,
    _out_len: u32,
) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn _handle_event(_ev_ptr: u32, _ev_len: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() -> u32 {
    0
}
