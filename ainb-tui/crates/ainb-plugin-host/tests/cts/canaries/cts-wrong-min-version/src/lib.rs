// Axis 11 canary: manifest demands an impossibly new host.
//
// The wasm body is irrelevant — manifest validation must reject this
// plugin before instantiation, with an actionable error naming the
// version mismatch. The host stays usable for further loads.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    0
}
