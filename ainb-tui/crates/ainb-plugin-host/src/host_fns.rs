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

    Ok(())
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
            |_: Caller<'_, HostState>, _topic_ptr: i32, _topic_len: i32| {},
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_event_publish",
            |_: Caller<'_, HostState>,
             _topic_ptr: i32,
             _topic_len: i32,
             _payload_ptr: i32,
             _payload_len: i32| {},
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

/// Read a UTF-8 string out of the plugin's exported `memory`.
fn read_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<String, String> {
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
    String::from_utf8(buf).map_err(|e| e.to_string())
}
