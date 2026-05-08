// Axis 9 canary: _tick spins forever.
//
// Probes the host's per-call fuel budget. With fuel enabled, the host
// must trap with OutOfFuel and surface a soft error rather than wedge
// the TUI thread. The plugin's _init logs a sentinel so the suite can
// distinguish "load failed" (no init log) from "fuel-budget tripped"
// (init log present, then OutOfFuel on _tick).
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

const INIT_MARKER: &[u8] = b"cts-9e6f2b30-infinite-tick-init-OK";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _tick() -> i32 {
    // Spin in a way the optimiser can't elide: read+write a volatile
    // counter so the loop body has observable side-effects.
    let mut n: u32 = 0;
    loop {
        unsafe {
            core::ptr::write_volatile(&mut n as *mut u32, n.wrapping_add(1));
        }
    }
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
