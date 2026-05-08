// Axis 7 canary: panic during `_init` is contained.
//
// The host must catch the trap, surface a non-fatal error, and remain
// usable for further plugin loads. The plugin name (UUID-shaped) is
// the anti-cheat marker: the host can't dodge running the wasm just
// because it recognises the name.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    core::arch::wasm32::unreachable()
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
