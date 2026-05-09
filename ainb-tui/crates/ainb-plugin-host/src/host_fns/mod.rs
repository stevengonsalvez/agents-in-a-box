//! Wire concrete host-fn implementations into a wasmi [`Linker`].
//!
//! Phase 1 ships:
//! * Real implementations for the three baseline fns (`ainb_log`,
//!   `ainb_now_ms`, `ainb_render_buffer`).
//! * Capability-gated stubs for the rest. Stubs match the locked ABI
//!   signatures so plugins built today keep working when the real impls land
//!   in Phase 2+. Stubs return [`HostStatus::HostError`] so plugin authors get
//!   a fast signal instead of silent zero data.
//!
//! Phase 6 adds three sub-modules wired by [`link_capabilities`]:
//! * [`fs`] — `ainb_fs_read_dir` + `ainb_fs_read_file`, scoped to the
//!   `read_claude_logs` / `read_codex_logs` allowlist roots.
//! * [`cache`] — generic plugin-scoped KV with TTL + per-plugin quota and
//!   global LRU eviction.
//! * [`request`] — synchronous `ainb_request_data(topic, payload, timeout)`
//!   over the existing event bus.

pub mod cache;
pub mod fs;
pub mod request;

use ainb_plugin_api::host::HOST_MODULE;
use ainb_plugin_api::CapabilitySet;
use anyhow::Context;
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
        |mut caller: Caller<'_, HostState>, target: i32, ptr: i32, len: i32| {
            // Plugin has serialised its WireBuffer (msgpack) into its memory
            // at [ptr, ptr+len). Decode + stash under (plugin_id, target);
            // ainb-core drains it on the next render tick.
            let Some(target) = ainb_plugin_api::RenderTarget::from_i32(target) else {
                caller.data_mut().last_error =
                    Some(format!("ainb_render_buffer: invalid target {target}"));
                return;
            };
            let bytes = match read_bytes(&mut caller, ptr, len) {
                Ok(b) => b,
                Err(e) => {
                    caller.data_mut().last_error = Some(format!("ainb_render_buffer: {e}"));
                    return;
                }
            };
            let buf: ainb_plugin_api::WireBuffer = match rmp_serde::from_slice(&bytes) {
                Ok(b) => b,
                Err(e) => {
                    caller.data_mut().last_error =
                        Some(format!("ainb_render_buffer decode: {e}"));
                    return;
                }
            };
            if !buf.is_consistent() {
                caller.data_mut().last_error = Some(format!(
                    "ainb_render_buffer: width*height={} but cells={}",
                    usize::from(buf.width) * usize::from(buf.height),
                    buf.cells.len()
                ));
                return;
            }
            let plugin_id = caller.data().plugin_id.clone();
            caller.data().shared.store_render(&plugin_id, target, buf);
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
    // For fd 1/2 (stdout/stderr) capture into HostState so the host can
    // harvest plugin CLI output via PluginHost::dispatch_cli. For other
    // fds, sum + report-as-written so panic handlers / unrelated writers
    // don't retry-loop.
    linker.func_wrap(
        WASI,
        "fd_write",
        |mut caller: Caller<'_, HostState>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nwritten_ptr: i32|
         -> i32 {
            let bytes = read_iovecs(&mut caller, iovs_ptr, iovs_len).unwrap_or_default();
            let total = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            match fd {
                1 => caller.data_mut().captured_stdout.extend_from_slice(&bytes),
                2 => caller.data_mut().captured_stderr.extend_from_slice(&bytes),
                _ => {}
            }
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

    // Plugins that link rusqlite or other std-fs paths import a wider
    // wasi-preview1 surface even if the runtime path doesn't actually
    // open files (DCE keeps the imports if the symbols are referenced).
    // Stub the rest so instantiation succeeds; calls at runtime return
    // ENOSYS-style errno (52). path_open *traps* via a panic so a plugin
    // that genuinely wants raw fs surfaces a clear error.

    // random_get(buf, len) -> errno. Fill with zeros for determinism.
    linker.func_wrap(
        WASI,
        "random_get",
        |mut caller: Caller<'_, HostState>, buf_ptr: i32, buf_len: i32| -> i32 {
            let memory = match caller.get_export("memory").and_then(wasmi::Extern::into_memory) {
                Some(m) => m,
                None => return 8, // EBADF — no memory exported
            };
            let len = match usize::try_from(buf_len) {
                Ok(n) => n,
                Err(_) => return 28, // EINVAL
            };
            let off = match usize::try_from(buf_ptr) {
                Ok(n) => n,
                Err(_) => return 28,
            };
            let zeros = vec![0_u8; len];
            let _ = memory.write(&mut caller, off, &zeros);
            0
        },
    )?;

    // clock_time_get(clock_id, precision, time_ptr) -> errno.
    // Writes wall-clock nanoseconds since UNIX epoch. The plugin uses this
    // through std's `SystemTime::now()` (chrono::Local::now() chains through)
    // — without a real reading the burndown UI would render fixed dates
    // (1970) and break the tripwire. CLOCK_REALTIME is the only id we
    // actually need; CLOCK_MONOTONIC also flows here and gets the same
    // (close-enough) reading.
    linker.func_wrap(
        WASI,
        "clock_time_get",
        |mut caller: Caller<'_, HostState>,
         _clock_id: i32,
         _precision: i64,
         time_ptr: i32|
         -> i32 {
            let memory = match caller.get_export("memory").and_then(wasmi::Extern::into_memory) {
                Some(m) => m,
                None => return 8,
            };
            let off = match usize::try_from(time_ptr) {
                Ok(n) => n,
                Err(_) => return 28,
            };
            let nanos: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
            let _ = memory.write(&mut caller, off, &nanos.to_le_bytes());
            0
        },
    )?;

    // fd_* / path_* fns: return ENOSYS (52). A plugin that actually
    // exercises these (rather than just having them imported by
    // dead-code-prone std paths) should switch to ainb_fs_read /
    // ainb_data_read.
    fn errno_nosys_4(_: Caller<'_, HostState>, _: i32, _: i32, _: i32, _: i32) -> i32 { 52 }
    fn errno_nosys_5(_: Caller<'_, HostState>, _: i32, _: i32, _: i32, _: i32, _: i32) -> i32 { 52 }
    fn errno_nosys_6(_: Caller<'_, HostState>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32) -> i32 { 52 }
    fn errno_nosys_2(_: Caller<'_, HostState>, _: i32, _: i32) -> i32 { 52 }
    fn errno_nosys_3(_: Caller<'_, HostState>, _: i32, _: i32, _: i32) -> i32 { 52 }
    fn errno_nosys_1(_: Caller<'_, HostState>, _: i32) -> i32 { 52 }

    linker.func_wrap(WASI, "fd_close", errno_nosys_1)?;
    linker.func_wrap(WASI, "fd_fdstat_get", errno_nosys_2)?;
    linker.func_wrap(WASI, "fd_filestat_get", errno_nosys_2)?;
    linker.func_wrap(WASI, "fd_prestat_get", errno_nosys_2)?;
    linker.func_wrap(WASI, "fd_prestat_dir_name", errno_nosys_3)?;
    linker.func_wrap(WASI, "fd_read", errno_nosys_4)?;
    linker.func_wrap(WASI, "fd_seek", |_: Caller<'_, HostState>, _: i32, _: i64, _: i32, _: i32| -> i32 { 52 })?;
    linker.func_wrap(WASI, "path_filestat_get", errno_nosys_5)?;
    // path_open: 9 args. Plugin shouldn't call it; return ENOSYS so it
    // surfaces as an io error instead of trapping the whole instance.
    linker.func_wrap(
        WASI,
        "path_open",
        |_: Caller<'_, HostState>,
         _: i32, _: i32, _: i32, _: i32, _: i32,
         _: i64, _: i64, _: i32, _: i32|
         -> i32 { 52 },
    )?;
    linker.func_wrap(WASI, "path_readlink", errno_nosys_6)?;

    Ok(())
}

fn write_le_u32(caller: &mut Caller<'_, HostState>, ptr: i32, value: u32) -> Option<()> {
    let memory = caller.get_export("memory").and_then(wasmi::Extern::into_memory)?;
    let off = usize::try_from(ptr).ok()?;
    memory.write(caller, off, &value.to_le_bytes()).ok()
}

/// Walk an iovec array (each entry: `(buf_ptr: u32, buf_len: u32)` little-endian
/// in linear memory) and copy out the bytes the plugin asked to write.
/// Returns the concatenated bytes; returns `None` on bad memory access.
fn read_iovecs(
    caller: &mut Caller<'_, HostState>,
    iovs_ptr: i32,
    iovs_len: i32,
) -> Option<Vec<u8>> {
    let memory = caller.get_export("memory").and_then(wasmi::Extern::into_memory)?;
    let count = u32::try_from(iovs_len).ok()?;
    let base = usize::try_from(iovs_ptr).ok()?;
    let mut out = Vec::new();
    for i in 0..count {
        let off = base.checked_add((i as usize).checked_mul(8)?)?;
        let mut entry = [0_u8; 8];
        memory.read(&caller, off, &mut entry).ok()?;
        let buf_ptr = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
        let buf_len = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]) as usize;
        let mut chunk = vec![0_u8; buf_len];
        memory.read(&caller, buf_ptr, &mut chunk).ok()?;
        out.extend_from_slice(&chunk);
    }
    Some(out)
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
            |mut caller: Caller<'_, HostState>,
             key_ptr: i32,
             key_len: i32,
             out_ptr: i32,
             out_len: i32|
             -> i32 {
                data_read_impl(&mut caller, key_ptr, key_len, out_ptr, out_len)
            },
        )?;
        linker.func_wrap(
            HOST_MODULE,
            "ainb_data_write",
            |mut caller: Caller<'_, HostState>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32|
             -> i32 {
                data_write_impl(&mut caller, key_ptr, key_len, val_ptr, val_len)
            },
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

    // Phase 6 fs/cache/request_data wiring. Each is internally
    // capability-gated and a no-op when its caps are absent.
    fs::link(linker, caps).context("link Phase 6 fs host-fns")?;
    cache::link(linker).context("link Phase 6 cache host-fns")?;
    request::link(linker, caps).context("link Phase 6 request_data host-fn")?;

    let allowlist = build_fs_allowlist(caps);
    if !allowlist.is_empty() {
        let allow_for_read = allowlist.clone();
        linker.func_wrap(
            HOST_MODULE,
            "ainb_fs_read",
            move |mut caller: Caller<'_, HostState>,
                  path_ptr: i32,
                  path_len: i32,
                  out_ptr: i32,
                  out_len: i32|
                  -> i32 {
                fs_read_impl(&mut caller, &allow_for_read, path_ptr, path_len, out_ptr, out_len)
            },
        )?;
        let allow_for_glob = allowlist.clone();
        linker.func_wrap(
            HOST_MODULE,
            "ainb_fs_glob",
            move |mut caller: Caller<'_, HostState>,
                  pat_ptr: i32,
                  pat_len: i32,
                  out_ptr: i32,
                  out_len: i32|
                  -> i32 {
                fs_glob_impl(&mut caller, &allow_for_glob, pat_ptr, pat_len, out_ptr, out_len)
            },
        )?;
    }

    Ok(())
}

/// Resolve which filesystem roots a plugin's capabilities grant. Returns
/// canonicalised `PathBuf`s rooted at the user's home directory; unresolvable
/// entries (no `$HOME`, missing dir) are silently dropped.
///
/// Capability → root mapping:
/// * `read_claude_logs`  → `$HOME/.claude`
/// * `read_codex_logs`   → `$HOME/.codex`
/// * `filesystem = [...]` → each entry, with `~` expanded and trailing
///   `/**` / `/*` stripped (treat as a prefix root).
pub(crate) fn build_fs_allowlist(caps: &CapabilitySet) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut roots: Vec<PathBuf> = Vec::new();

    let push_under_home = |roots: &mut Vec<PathBuf>, sub: &str| {
        if let Some(home) = dirs::home_dir() {
            let p = home.join(sub);
            if let Ok(canon) = p.canonicalize() {
                roots.push(canon);
            } else {
                // Fall back to the un-canonicalised path so the allowlist
                // still applies even if the dir doesn't exist yet.
                roots.push(p);
            }
        }
    };

    if caps.read_claude_logs {
        push_under_home(&mut roots, ".claude");
    }
    if caps.read_codex_logs {
        push_under_home(&mut roots, ".codex");
    }
    for entry in &caps.filesystem {
        // Strip trailing glob suffixes — we model the allowlist as path
        // prefixes (the glob match itself is the plugin's responsibility).
        let trimmed = entry.trim_end_matches("/**").trim_end_matches("/*");
        let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
            dirs::home_dir().map(|h| h.join(rest))
        } else if trimmed == "~" {
            dirs::home_dir()
        } else {
            Some(std::path::PathBuf::from(trimmed))
        };
        if let Some(p) = expanded {
            if let Ok(canon) = p.canonicalize() {
                roots.push(canon);
            } else {
                roots.push(p);
            }
        }
    }
    roots
}

/// Real impl of the `ainb_fs_read` host-fn. Validates the requested path is
/// under one of the plugin's allowlisted roots, reads the file, and copies it
/// into the plugin's pre-allocated output buffer.
fn fs_read_impl(
    caller: &mut Caller<'_, HostState>,
    allowlist: &[std::path::PathBuf],
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    use ainb_plugin_api::host::HostStatus;

    let path_str = match read_string(caller, path_ptr, path_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_fs_read: bad path utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };

    // Expand a leading `~/` so plugins can write portable paths.
    let raw = std::path::PathBuf::from(if let Some(rest) = path_str.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().into_owned())
            .unwrap_or(path_str.clone())
    } else {
        path_str.clone()
    });
    // Canonicalise the requested path so `..` traversal can't escape the
    // allowlisted root. Files that don't exist canonicalise via parent +
    // file_name, mirroring std::fs::canonicalize semantics for missing tails.
    let canonical = raw.canonicalize().unwrap_or(raw);
    let permitted = allowlist.iter().any(|root| canonical.starts_with(root));
    if !permitted {
        caller.data_mut().last_error =
            Some(format!("ainb_fs_read denied: {} not under allowlist", canonical.display()));
        return HostStatus::NotPermitted as i32;
    }

    let bytes = match std::fs::read(&canonical) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read: {} ({e})", canonical.display()));
            return HostStatus::HostError as i32;
        }
    };

    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        // Tell the caller exactly how big a buffer they need by returning
        // BufferTooSmall; the plugin re-allocs and retries. We deliberately
        // do NOT write a partial payload — keeps the read atomic.
        return HostStatus::BufferTooSmall as i32;
    }

    let memory = match caller.get_export("memory").and_then(wasmi::Extern::into_memory) {
        Some(m) => m,
        None => return HostStatus::HostError as i32,
    };
    let off = match usize::try_from(out_ptr) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if memory.write(&mut *caller, off, &bytes).is_err() {
        return HostStatus::HostError as i32;
    }
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
}

/// Per-plugin scoped data path: `~/.agents-in-a-box/plugins/data/<id>/<id>.db`.
/// Created on first write; missing source on read returns `HostStatus::Ok`
/// with zero bytes written so plugins can probe for existence cheaply.
fn plugin_data_db_path(plugin_id: &str) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let dir = home
        .join(".agents-in-a-box")
        .join("plugins")
        .join("data")
        .join(plugin_id);
    Some(dir.join(format!("{plugin_id}.db")))
}

/// Open the plugin's KV DB, creating the parent dir + `kv` table on first
/// touch. The KV store is intentionally simple (TEXT key, BLOB value) —
/// plugins that need richer queries can layer on top using their own keys.
fn open_plugin_kv(plugin_id: &str) -> rusqlite::Result<rusqlite::Connection> {
    let path = plugin_data_db_path(plugin_id)
        .ok_or_else(|| rusqlite::Error::InvalidPath("$HOME unresolvable".into()))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v BLOB NOT NULL)",
        [],
    )?;
    Ok(conn)
}

/// `ainb_data_read(key, out)` — look up `key` in the plugin's KV DB.
/// Returns the byte count on success, `0` when the key isn't set,
/// `BufferTooSmall` when `out_len` is too small for the value.
fn data_read_impl(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    use ainb_plugin_api::host::HostStatus;

    let key = match read_string(caller, key_ptr, key_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_data_read: bad key utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let plugin_id = caller.data().plugin_id.clone();
    let conn = match open_plugin_kv(&plugin_id) {
        Ok(c) => c,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_data_read: open db: {e}"));
            return HostStatus::HostError as i32;
        }
    };
    let bytes: Option<Vec<u8>> = match conn.query_row(
        "SELECT v FROM kv WHERE k = ?1",
        [&key],
        |row| row.get::<_, Vec<u8>>(0),
    ) {
        Ok(b) => Some(b),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_data_read: select: {e}"));
            return HostStatus::HostError as i32;
        }
    };
    let Some(bytes) = bytes else {
        // Key not present — convention: return 0 bytes (Ok) so the plugin
        // can distinguish "no value" from "denied" without an extra probe.
        return 0;
    };
    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        return HostStatus::BufferTooSmall as i32;
    }
    let memory = match caller.get_export("memory").and_then(wasmi::Extern::into_memory) {
        Some(m) => m,
        None => return HostStatus::HostError as i32,
    };
    let off = match usize::try_from(out_ptr) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if memory.write(&mut *caller, off, &bytes).is_err() {
        return HostStatus::HostError as i32;
    }
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
}

/// `ainb_data_write(key, value)` — upsert into the plugin's KV DB.
fn data_write_impl(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    val_ptr: i32,
    val_len: i32,
) -> i32 {
    use ainb_plugin_api::host::HostStatus;

    let key = match read_string(caller, key_ptr, key_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_data_write: bad key utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let val = match read_bytes(caller, val_ptr, val_len) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_data_write: read value: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let plugin_id = caller.data().plugin_id.clone();
    let conn = match open_plugin_kv(&plugin_id) {
        Ok(c) => c,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_data_write: open db: {e}"));
            return HostStatus::HostError as i32;
        }
    };
    if let Err(e) = conn.execute(
        "INSERT INTO kv (k, v) VALUES (?1, ?2) \
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        rusqlite::params![&key, &val],
    ) {
        caller.data_mut().last_error = Some(format!("ainb_data_write: upsert: {e}"));
        return HostStatus::HostError as i32;
    }
    0
}

/// Real impl of `ainb_fs_glob`. Plugin hands a glob pattern as a UTF-8
/// string; host expands it (with `~/` resolution), drops every match that
/// isn't under the plugin's allowlist, joins the rest with `\n`, and writes
/// the bytes back into plugin memory.
///
/// The wire shape is intentionally ASCII-newline-delimited so plugins
/// don't have to ship a serde dep just to read directory listings.
fn fs_glob_impl(
    caller: &mut Caller<'_, HostState>,
    allowlist: &[std::path::PathBuf],
    pat_ptr: i32,
    pat_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    use ainb_plugin_api::host::HostStatus;

    let pat = match read_string(caller, pat_ptr, pat_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error = Some(format!("ainb_fs_glob: bad pattern utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let expanded_pat = if let Some(rest) = pat.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().into_owned())
            .unwrap_or(pat.clone())
    } else {
        pat.clone()
    };

    let entries = match glob::glob(&expanded_pat) {
        Ok(it) => it,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_glob: bad pattern {expanded_pat}: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };

    // Filter to entries inside the plugin's allowlist; bad entries are
    // skipped, not surfaced (a glob with permission errors elsewhere
    // shouldn't fail the whole call).
    let mut paths: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let canonical = entry.canonicalize().unwrap_or(entry.clone());
        if allowlist.iter().any(|root| canonical.starts_with(root)) {
            paths.push(canonical.to_string_lossy().into_owned());
        }
    }
    let joined = paths.join("\n");
    let bytes = joined.as_bytes();

    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        return HostStatus::BufferTooSmall as i32;
    }

    let memory = match caller.get_export("memory").and_then(wasmi::Extern::into_memory) {
        Some(m) => m,
        None => return HostStatus::HostError as i32,
    };
    let off = match usize::try_from(out_ptr) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if memory.write(&mut *caller, off, bytes).is_err() {
        return HostStatus::HostError as i32;
    }
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
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
pub(crate) fn read_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<String, String> {
    String::from_utf8(read_bytes(caller, ptr, len)?).map_err(|e| e.to_string())
}
