// Axis 12a: publishes on `cts.ping` every tick.
//
// Pair-b subscribes to the same topic in its `_init` and asserts the
// payload arrives. The two canaries together prove the host's event
// bus delivers cross-plugin events deterministically.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

extern "C" {
    fn ainb_log(level: i32, ptr: i32, len: i32);
    fn ainb_event_publish(topic_ptr: i32, topic_len: i32, payload_ptr: i32, payload_len: i32);
}

const INIT_MARKER: &[u8] = b"cts-9d4f88a0-pair-a-init-OK";
const TOPIC: &[u8] = b"cts.ping";
const PAYLOAD: &[u8] = b"hello-from-cts-9d4f88a0";

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    unsafe { ainb_log(2, INIT_MARKER.as_ptr() as i32, INIT_MARKER.len() as i32) }
    0
}

#[no_mangle]
pub extern "C" fn _tick() -> i32 {
    unsafe {
        ainb_event_publish(
            TOPIC.as_ptr() as i32,
            TOPIC.len() as i32,
            PAYLOAD.as_ptr() as i32,
            PAYLOAD.len() as i32,
        );
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {}
