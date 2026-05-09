// Axis 13 canary: fs_read_dir is linked when read_claude_logs is granted.
//
// Behavioural anti-cheat: _init calls ainb_fs_read_dir with a clearly
// non-existent path (so the host returns NotPermitted/HostError, not
// real bytes), then logs the canary sentinel. The host runner only
// asserts that *_init ran*, i.e. that instantiation succeeded — the
// fact that the host fn returned an error code is fine; what matters
// is that the import resolved. If the host had refused the cap, _init
// would never run.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_fs_read_dir(path_ptr: i32, path_len: i32, out_ptr: i32, out_len: i32) -> i32;
}

const SENTINEL: &[u8] = b"cts-13a8e0c4-fs-read-with-cap-init-OK";
const PROBE_PATH: &[u8] = b"~/.claude/projects/cts-canary-probe-not-real";
static mut OUT_BUF: [u8; 64] = [0; 64];

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe {
        // Call the gated host fn. We don't care about the return code:
        // the import resolving is the only thing this canary proves.
        let _ = ainb_fs_read_dir(
            PROBE_PATH.as_ptr() as i32,
            PROBE_PATH.len() as i32,
            OUT_BUF.as_mut_ptr() as i32,
            OUT_BUF.len() as i32,
        );
        ainb_log(2, SENTINEL.as_ptr() as i32, SENTINEL.len() as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
