// Axis 14 canary: capability refusal is *static* for the new fs fns.
//
// The plugin manifest declares no read_claude_logs / read_codex_logs,
// yet imports ainb_fs_read_dir. The host must not link the import →
// wasmi rejects the module at instantiation. _init never runs.
//
// Anti-cheat: if the host secretly stubbed the host-fn instead of
// refusing it, _init would run and emit the SHOULD_NOT_FIRE sentinel.
// The runner asserts the sentinel does NOT appear in any log ring.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_fs_read_dir(path_ptr: i32, path_len: i32, out_ptr: i32, out_len: i32) -> i32;
}

const SHOULD_NOT_FIRE: &[u8] = b"cts-14d6b2e8-fs-read-without-cap-init-RAN";
static mut OUT_BUF: [u8; 16] = [0; 16];

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe {
        ainb_log(4, SHOULD_NOT_FIRE.as_ptr() as i32, SHOULD_NOT_FIRE.len() as i32);
        let _ = ainb_fs_read_dir(0, 0, OUT_BUF.as_mut_ptr() as i32, OUT_BUF.len() as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
