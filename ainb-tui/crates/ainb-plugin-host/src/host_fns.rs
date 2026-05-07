//! Wire concrete host-fn implementations into a wasmi [`Linker`].
//!
//! Phase 1 ships:
//! * Real implementations for the three baseline fns (`ainb_log`,
//!   `ainb_now_ms`, `ainb_render_buffer`).
//! * Capability-gated stubs for the rest. Stubs match the locked ABI
//!   signatures so plugins built today keep working when the real impls land
//!   in Phase 2+. Stubs return [`HostStatus::HostError`] so plugin authors get
//!   a fast signal instead of silent zero data.

use ainb_plugin_api::host::HOST_MODULE;
use ainb_plugin_api::CapabilitySet;
use wasmi::{Caller, Linker};

use crate::runtime::HostState;

/// Link the always-on host-fns. Must be called before [`link_capabilities`].
pub fn link_baseline(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    linker.func_wrap(
        HOST_MODULE,
        "ainb_log",
        |mut caller: Caller<'_, HostState>, level: i32, ptr: i32, len: i32| {
            let msg = read_string(&mut caller, ptr, len)
                .unwrap_or_else(|e| format!("<bad log payload: {e}>"));
            caller.data_mut().push_log(level, msg);
        },
    )?;

    linker.func_wrap(HOST_MODULE, "ainb_now_ms", |_: Caller<'_, HostState>| -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_u64, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    })?;

    linker.func_wrap(
        HOST_MODULE,
        "ainb_render_buffer",
        |_caller: Caller<'_, HostState>, _target: i32, _ptr: i32, _len: i32| {
            // Phase 1 stub: render channel real impl lands with the screen
            // registry in Phase 2a. Discarding the payload is safe — the
            // host-side registries are still empty.
        },
    )?;

    link_wasi_preview1_stubs(linker)?;

    Ok(())
}

/// Link minimal `wasi_snapshot_preview1` stubs so any plugin built against
/// `wasm32-wasip1` (i.e. anything with a Rust std dep — the panic handler
/// pulls in `fd_write`/`environ_*`/`proc_exit`) can satisfy its imports.
///
/// Phase 3 reality: wasmi 0.40 ships no wasi-preview1 host. Without these
/// stubs every linked-against-std plugin fails at instantiation time. The
/// real plugin host gets a wasi-preview1 backend later (filesystem +
/// preopens etc.); until then these stubs are the floor that lets a plugin
/// load at all.
///
/// Semantics:
/// * `environ_*`: report zero env vars and zero buffer bytes — plugins see
///   an empty environment.
/// * `fd_write`: pretend the entire iovec was written without actually
///   doing IO. Keeps `println!` / panic output silent rather than trapping.
/// * `proc_exit`: trap with a descriptive error so a plugin invoking it
///   degrades cleanly instead of taking down the host.
fn link_wasi_preview1_stubs(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    const WASI: &str = "wasi_snapshot_preview1";

    // i32 environ_count_ptr, i32 environ_buf_size_ptr -> i32 errno.
    // Write zeros so the caller thinks the environment is empty.
    linker.func_wrap(
        WASI,
        "environ_sizes_get",
        |mut caller: Caller<'_, HostState>, count_ptr: i32, buf_size_ptr: i32| -> i32 {
            let _ = write_le_u32(&mut caller, count_ptr, 0);
            let _ = write_le_u32(&mut caller, buf_size_ptr, 0);
            0
        },
    )?;

    // i32 environ_ptr_ptr, i32 environ_buf_ptr -> i32 errno. No-op success.
    linker.func_wrap(
        WASI,
        "environ_get",
        |_caller: Caller<'_, HostState>, _environ: i32, _buf: i32| -> i32 { 0 },
    )?;

    // i32 fd, i32 iovs_ptr, i32 iovs_len, i32 nwritten_ptr -> i32 errno.
    // Sum the iovec lengths and tell the plugin we wrote them all. Avoids
    // panic-handler retry loops without performing real IO.
    linker.func_wrap(
        WASI,
        "fd_write",
        |mut caller: Caller<'_, HostState>,
         _fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nwritten_ptr: i32|
         -> i32 {
            let total = sum_iovec_len(&mut caller, iovs_ptr, iovs_len).unwrap_or(0);
            let _ = write_le_u32(&mut caller, nwritten_ptr, total);
            0
        },
    )?;

    // i32 exit_code -> (). We trap (via panic, which wasmi turns into a
    // host trap) so the offending plugin gets marked degraded by the host
    // instead of std::process::exit-ing the whole ainb process.
    fn proc_exit_stub(_caller: Caller<'_, HostState>, code: i32) {
        panic!("plugin called proc_exit({code}) — degraded");
    }
    linker.func_wrap(WASI, "proc_exit", proc_exit_stub)?;

    Ok(())
}

fn write_le_u32(caller: &mut Caller<'_, HostState>, ptr: i32, value: u32) -> Option<()> {
    let memory = caller.get_export("memory").and_then(wasmi::Extern::into_memory)?;
    let off = usize::try_from(ptr).ok()?;
    memory.write(caller, off, &value.to_le_bytes()).ok()
}

/// Walk an iovec array (each entry: `(buf_ptr: u32, buf_len: u32)` little-endian
/// in linear memory) and sum the lengths. Returns `None` on bad memory access.
fn sum_iovec_len(
    caller: &mut Caller<'_, HostState>,
    iovs_ptr: i32,
    iovs_len: i32,
) -> Option<u32> {
    let memory = caller.get_export("memory").and_then(wasmi::Extern::into_memory)?;
    let count = u32::try_from(iovs_len).ok()?;
    let base = usize::try_from(iovs_ptr).ok()?;
    let mut total: u32 = 0;
    for i in 0..count {
        let off = base.checked_add((i as usize).checked_mul(8)?)?;
        let mut entry = [0_u8; 8];
        memory.read(&caller, off, &mut entry).ok()?;
        let len = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        total = total.checked_add(len)?;
    }
    Some(total)
}

/// Link host-fns the plugin's manifest grants. Must be called *after*
/// [`link_baseline`]. Imports for refused capabilities are deliberately *not*
/// linked, so wasmi rejects them at instantiation.
pub fn link_capabilities(
    linker: &mut Linker<HostState>,
    caps: &CapabilitySet,
) -> anyhow::Result<()> {
    use ainb_plugin_api::host::HostStatus;

    if caps.read_sessions {
        linker.func_wrap(
            HOST_MODULE,
            "ainb_sessions_list",
            |_: Caller<'_, HostState>, _out_ptr: i32, _out_len: i32| -> i32 {
                HostStatus::HostError as i32
            },
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_session_get",
            |_: Caller<'_, HostState>,
             _id_ptr: i32,
             _id_len: i32,
             _out_ptr: i32,
             _out_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
    }

    if caps.write_plugin_data {
        linker.func_wrap(
            HOST_MODULE,
            "ainb_data_read",
            |_: Caller<'_, HostState>,
             _key_ptr: i32,
             _key_len: i32,
             _out_ptr: i32,
             _out_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_data_write",
            |_: Caller<'_, HostState>,
             _key_ptr: i32,
             _key_len: i32,
             _val_ptr: i32,
             _val_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
    }

    if caps.event_bus {
        linker.func_wrap(
            HOST_MODULE,
            "ainb_event_subscribe",
            |mut caller: Caller<'_, HostState>, topic_ptr: i32, topic_len: i32| {
                let topic = read_string(&mut caller, topic_ptr, topic_len)
                    .unwrap_or_default();
                let plugin_id = caller.data().plugin_id.clone();
                caller.data().shared.subscribe(&plugin_id, topic);
            },
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_event_publish",
            |mut caller: Caller<'_, HostState>,
             topic_ptr: i32,
             topic_len: i32,
             payload_ptr: i32,
             payload_len: i32| {
                let topic = read_string(&mut caller, topic_ptr, topic_len)
                    .unwrap_or_default();
                let payload = read_bytes(&mut caller, payload_ptr, payload_len)
                    .unwrap_or_default();
                let plugin_id = caller.data().plugin_id.clone();
                caller.data().shared.publish(&plugin_id, topic, payload);
            },
        )?;
    }

    if !caps.network.is_empty() {
        linker.func_wrap(
            HOST_MODULE,
            "ainb_http_request",
            |_: Caller<'_, HostState>,
             _req_ptr: i32,
             _req_len: i32,
             _out_ptr: i32,
             _out_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
    }

    if !caps.filesystem.is_empty() {
        linker.func_wrap(
            HOST_MODULE,
            "ainb_fs_read",
            |_: Caller<'_, HostState>,
             _path_ptr: i32,
             _path_len: i32,
             _out_ptr: i32,
             _out_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_fs_glob",
            |_: Caller<'_, HostState>,
             _pat_ptr: i32,
             _pat_len: i32,
             _out_ptr: i32,
             _out_len: i32|
             -> i32 { HostStatus::HostError as i32 },
        )?;
    }

    Ok(())
}

/// Read raw bytes out of the plugin's exported `memory`.
pub(crate) fn read_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, String> {
    let memory = caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
        .ok_or_else(|| "plugin has no exported memory".to_string())?;
    let len_usize = usize::try_from(len).map_err(|e| e.to_string())?;
    let off_usize = usize::try_from(ptr).map_err(|e| e.to_string())?;
    let mut buf = vec![0_u8; len_usize];
    memory
        .read(&caller, off_usize, &mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Read a UTF-8 string out of the plugin's exported `memory`.
fn read_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<String, String> {
    String::from_utf8(read_bytes(caller, ptr, len)?).map_err(|e| e.to_string())
}
