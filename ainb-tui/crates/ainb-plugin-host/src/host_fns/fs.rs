//! Phase 6 fs host fns: directory listing + file read scoped to the
//! plugin's *log* capabilities (`read_claude_logs` / `read_codex_logs`).
//!
//! These are *separate* from the older general `ainb_fs_read` /
//! `ainb_fs_glob` pair (gated by the `filesystem = […]` allowlist
//! capability) and *narrower*:
//!
//! * `read_claude_logs` ⇒ paths under `~/.claude/projects/`
//! * `read_codex_logs`  ⇒ paths under `~/.codex/sessions/`
//!
//! That subset is what the session-reader plugin (Phase 6b) needs to
//! walk its provider stores. Anything else — the general allowlist
//! capability — keeps using the older fns. Restricting the new fns to
//! these specific subdirs means a plugin that legitimately needed to
//! read other Claude config files (e.g. `~/.claude/settings.json`)
//! gets a clean refusal instead of accidental access.
//!
//! ## Static refusal
//!
//! If neither capability is granted, neither host fn is registered in
//! the wasmi `Linker`. A plugin that imports them without declaring a
//! cap fails at instantiation with an unresolved-import error — the
//! same static-refusal pattern axes 6 + 14 of the CTS exercise.

use ainb_plugin_api::host::{HostStatus, HOST_MODULE};
use ainb_plugin_api::CapabilitySet;
use anyhow::Result;
use wasmi::{Caller, Linker};

use crate::path_guard::{check_under_allowlist, AllowlistRoot, PathGuardError};
use crate::runtime::HostState;

use super::{read_bytes, read_string};

/// Roots that the per-cap allowlist resolves to. These are the *sub-dirs*
/// — `.claude/projects/` and `.codex/sessions/` — not the umbrella `.claude`
/// directory the older `ainb_fs_read` allowlist allows.
pub fn build_logs_allowlist(caps: &CapabilitySet) -> Vec<AllowlistRoot> {
    let mut out = Vec::new();
    let mut push = |sub: &str| {
        if let Some(home) = dirs::home_dir() {
            if let Some(r) = AllowlistRoot::new(home.join(sub)) {
                out.push(r);
            }
        }
    };
    if caps.read_claude_logs {
        push(".claude/projects");
    }
    if caps.read_codex_logs {
        push(".codex/sessions");
    }
    out
}

/// Wire `ainb_fs_read_dir` + `ainb_fs_read_file` into the linker if the
/// caller has at least one of the log-read capabilities. The closures
/// own a clone of the allowlist so the wasmi store doesn't need to
/// re-query it per call.
pub fn link(linker: &mut Linker<HostState>, caps: &CapabilitySet) -> Result<()> {
    let allowlist = build_logs_allowlist(caps);
    if allowlist.is_empty() {
        // Caps absent — leave the imports unresolved. wasmi will fail
        // instantiation at load, surfaced via the existing axis-6 path.
        return Ok(());
    }

    let allow_dir = allowlist.clone();
    linker.func_wrap(
        HOST_MODULE,
        "ainb_fs_read_dir",
        move |mut caller: Caller<'_, HostState>,
              path_ptr: i32,
              path_len: i32,
              out_ptr: i32,
              out_len: i32|
              -> i32 {
            fs_read_dir_impl(&mut caller, &allow_dir, path_ptr, path_len, out_ptr, out_len)
        },
    )?;

    let allow_file = allowlist;
    linker.func_wrap(
        HOST_MODULE,
        "ainb_fs_read_file",
        move |mut caller: Caller<'_, HostState>,
              path_ptr: i32,
              path_len: i32,
              out_ptr: i32,
              out_len: i32|
              -> i32 {
            fs_read_file_impl(&mut caller, &allow_file, path_ptr, path_len, out_ptr, out_len)
        },
    )?;
    Ok(())
}

/// `ainb_fs_read_dir(path) -> Vec<String>` host-fn impl. Wire shape: the
/// host writes a `\n`-joined list of canonical absolute paths into the
/// caller's pre-allocated `out` buffer and returns the byte count, or a
/// negative `HostStatus` on error. Empty directory → 0 bytes written
/// (still `Ok`).
///
/// Only directly-contained entries are listed; the host does NOT recurse.
/// The session-reader plugin walks the resulting list and re-calls
/// `ainb_fs_read_dir` on each subdir if it needs depth.
fn fs_read_dir_impl(
    caller: &mut Caller<'_, HostState>,
    allowlist: &[AllowlistRoot],
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    let path_str = match read_string(caller, path_ptr, path_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_dir: bad path utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };

    let canonical = match check_under_allowlist(&path_str, allowlist) {
        Ok(p) => p,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_dir denied {path_str:?}: {e}"));
            return path_guard_status(&e) as i32;
        }
    };

    let entries = match std::fs::read_dir(&canonical) {
        Ok(it) => it,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_dir: read_dir {} ({e})", canonical.display()));
            return HostStatus::HostError as i32;
        }
    };

    let mut joined = Vec::<u8>::new();
    let mut sorted: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .collect();
    sorted.sort(); // deterministic order for byte-identical fixtures
    for p in sorted {
        if !joined.is_empty() {
            joined.push(b'\n');
        }
        joined.extend_from_slice(p.to_string_lossy().as_bytes());
    }

    write_bytes_to_caller(caller, out_ptr, out_len, &joined)
}

/// `ainb_fs_read_file(path) -> Vec<u8>` host-fn impl. Returns the file's
/// raw bytes, or a negative `HostStatus`. The plugin owns a buffer of
/// `out_len`; oversize files come back as `BufferTooSmall` so the
/// plugin can re-alloc and retry.
fn fs_read_file_impl(
    caller: &mut Caller<'_, HostState>,
    allowlist: &[AllowlistRoot],
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    let path_str = match read_string(caller, path_ptr, path_len) {
        Ok(s) => s,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_file: bad path utf8: {e}"));
            return HostStatus::InvalidArgument as i32;
        }
    };
    let canonical = match check_under_allowlist(&path_str, allowlist) {
        Ok(p) => p,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_file denied {path_str:?}: {e}"));
            return path_guard_status(&e) as i32;
        }
    };
    let bytes = match std::fs::read(&canonical) {
        Ok(b) => b,
        Err(e) => {
            caller.data_mut().last_error =
                Some(format!("ainb_fs_read_file: read {} ({e})", canonical.display()));
            return HostStatus::HostError as i32;
        }
    };
    write_bytes_to_caller(caller, out_ptr, out_len, &bytes)
}

fn path_guard_status(e: &PathGuardError) -> HostStatus {
    match e {
        PathGuardError::NulByte | PathGuardError::Empty | PathGuardError::NoHome => {
            HostStatus::InvalidArgument
        }
        PathGuardError::PathDenied(_) | PathGuardError::NoParent(_) => HostStatus::NotPermitted,
    }
}

fn write_bytes_to_caller(
    caller: &mut Caller<'_, HostState>,
    out_ptr: i32,
    out_len: i32,
    bytes: &[u8],
) -> i32 {
    let cap = match usize::try_from(out_len) {
        Ok(n) => n,
        Err(_) => return HostStatus::InvalidArgument as i32,
    };
    if bytes.len() > cap {
        return HostStatus::BufferTooSmall as i32;
    }
    let memory = match caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
    {
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
    // Read attempts may legitimately return zero bytes (empty file /
    // empty dir) — that's `Ok(0)`, not an error.
    i32::try_from(bytes.len()).unwrap_or(HostStatus::HostError as i32)
}

/// Read the bytes a previous host-fn call wrote at offset `off` of the
/// plugin's exported memory. Used by tests that drive the host-fn
/// through inline WAT and want to read back results without going
/// through wasmi's `Caller`.
///
/// This is on `fs.rs` (not in `runtime.rs`) because it's only useful
/// to the fs/cache test surfaces — the rest of the host has no need
/// for raw memory peeks.
#[cfg(test)]
pub(crate) fn read_plugin_memory(
    plugin: &crate::loader::LoadedPlugin,
    off: usize,
    len: usize,
) -> Option<Vec<u8>> {
    let memory = plugin
        .instance
        .get_export(&plugin.store, "memory")
        .and_then(wasmi::Extern::into_memory)?;
    let mut buf = vec![0_u8; len];
    memory.read(&plugin.store, off, &mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_api::CapabilitySet;

    #[test]
    fn empty_caps_yield_empty_allowlist() {
        let caps = CapabilitySet::default();
        assert!(build_logs_allowlist(&caps).is_empty());
    }

    #[test]
    fn read_claude_logs_resolves_projects_subdir() {
        let caps = CapabilitySet {
            read_claude_logs: true,
            ..Default::default()
        };
        let roots = build_logs_allowlist(&caps);
        assert_eq!(roots.len(), 1);
        // Canonical path may differ from raw on macOS (/private/var skew),
        // but both ends should reference `.claude/projects` literally.
        let s = roots[0].canonical().to_string_lossy().into_owned();
        assert!(s.ends_with(".claude/projects"), "got {s}");
    }

    #[test]
    fn both_log_caps_yield_two_roots() {
        let caps = CapabilitySet {
            read_claude_logs: true,
            read_codex_logs: true,
            ..Default::default()
        };
        let roots = build_logs_allowlist(&caps);
        assert_eq!(roots.len(), 2);
    }
}
