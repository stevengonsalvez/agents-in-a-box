// Axis 6 canary: capability refusal is *static*.
//
// The plugin manifest declares no `network` allowlist, yet this code
// imports `ainb_http_request`. The host must not link the import →
// wasmi rejects the module at instantiation. The plugin never runs;
// `_init` is never called; the sentinel below NEVER appears in any log.
//
// Anti-cheat: if the host secretly stubbed the host-fn instead of
// refusing it, `_init` would run and emit the sentinel. The runner
// asserts the sentinel does NOT appear in any plugin's log ring.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_http_request(req_ptr: i32, req_len: i32, out_ptr: i32, out_len: i32) -> i32;
}

const SHOULD_NOT_FIRE: &[u8] = b"cts-2a91f0b2-cap-refused-network-init-RAN";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe {
        ainb_log(4, SHOULD_NOT_FIRE.as_ptr() as i32, SHOULD_NOT_FIRE.len() as i32);
        let _ = ainb_http_request(0, 0, 0, 0);
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
