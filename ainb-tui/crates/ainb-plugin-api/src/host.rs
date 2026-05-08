//! Host-fn ABI declarations.
//!
//! This module is the **plugin-side view** of the host's exported functions.
//! Every signature here is a load-bearing ABI commitment — bumping any one of
//! them is a breaking change for every plugin already in the wild and forces a
//! `ainb_min_version` bump in their manifests.
//!
//! # Conventions
//!
//! * Pointers and lengths are `i32` because `wasm32-wasip1` uses 32-bit linear
//!   memory addressing.
//! * `_in` parameters are pointers + length pairs into plugin memory the host
//!   reads.
//! * `_out` parameters are pointers + capacity pairs the host fills. The return
//!   value is the number of bytes actually written, or a negative error code
//!   from [`HostStatus`].
//! * Functions that don't return data return `()` or one of [`HostStatus`].
//! * Capability-gated host-fns are commented as such; the host links them only
//!   if the plugin declared the matching capability. Imports the host doesn't
//!   provide cause a wasmi link error at instantiation — refused statically.

#![allow(clippy::missing_safety_doc)]

/// Host-fn return code.
///
/// Conventions:
/// * `Ok` → operation succeeded; for read fns `>= 0` is the number of bytes
///   written into `out`.
/// * Negative values are error codes the host returns directly from a host-fn.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStatus {
    Ok = 0,
    /// The provided output buffer was too small. Caller should `_alloc` a
    /// bigger buffer and retry.
    BufferTooSmall = -1,
    /// The plugin attempted to access a key/path/host outside its allowlist.
    NotPermitted = -2,
    /// Argument failed validation (e.g. non-utf8 string, oversized payload).
    InvalidArgument = -3,
    /// Generic host-side failure; details go through `ainb_log`.
    HostError = -4,
}

impl HostStatus {
    #[must_use]
    pub fn from_i32(v: i32) -> Self {
        match v {
            0.. => Self::Ok,
            -1 => Self::BufferTooSmall,
            -2 => Self::NotPermitted,
            -3 => Self::InvalidArgument,
            _ => Self::HostError,
        }
    }
}

/// Log levels accepted by `ainb_log`. Stable numeric ABI.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

/// Module name imported by every plugin: `env`. Wasmi defaults to this name and
/// it matches the convention emitted by `wasm-bindgen`/`cdylib` builds.
pub const HOST_MODULE: &str = "env";

// ──────────────────────────────────────────────────────────────────────────
// extern "C" import declarations — visible to plugin code only.
//
// On the host build of this crate these resolve to plain Rust function-pointer
// types via the cfg gate so that anyone depending on `ainb-plugin-api` for the
// types alone (the host, marketplace tooling, conformance harness) still
// compiles.
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
extern "C" {
    // Always-on (no capability required) ────────────────────────────────────
    pub fn ainb_log(level: i32, msg_ptr: i32, msg_len: i32);
    pub fn ainb_now_ms() -> u64;

    // write_plugin_data ─────────────────────────────────────────────────────
    pub fn ainb_data_read(
        key_ptr: i32,
        key_len: i32,
        out_ptr: i32,
        out_len: i32,
    ) -> i32;
    pub fn ainb_data_write(
        key_ptr: i32,
        key_len: i32,
        val_ptr: i32,
        val_len: i32,
    ) -> i32;

    // event_bus ─────────────────────────────────────────────────────────────
    pub fn ainb_event_subscribe(topic_ptr: i32, topic_len: i32);
    pub fn ainb_event_publish(
        topic_ptr: i32,
        topic_len: i32,
        payload_ptr: i32,
        payload_len: i32,
    );

    // read_sessions ─────────────────────────────────────────────────────────
    pub fn ainb_sessions_list(out_ptr: i32, out_len: i32) -> i32;
    pub fn ainb_session_get(
        id_ptr: i32,
        id_len: i32,
        out_ptr: i32,
        out_len: i32,
    ) -> i32;

    // Always-on render channel ──────────────────────────────────────────────
    pub fn ainb_render_buffer(target_id: i32, payload_ptr: i32, payload_len: i32);

    // network ───────────────────────────────────────────────────────────────
    pub fn ainb_http_request(
        req_ptr: i32,
        req_len: i32,
        out_ptr: i32,
        out_len: i32,
    ) -> i32;

    // filesystem ────────────────────────────────────────────────────────────
    pub fn ainb_fs_read(
        path_ptr: i32,
        path_len: i32,
        out_ptr: i32,
        out_len: i32,
    ) -> i32;
    pub fn ainb_fs_glob(
        pattern_ptr: i32,
        pattern_len: i32,
        out_ptr: i32,
        out_len: i32,
    ) -> i32;
}

/// Catalogue of host-fn import names, paired with the capability that gates
/// them. The host's capability gate uses this list to decide which imports to
/// link for a given plugin.
///
/// Keep this in sync with the `extern "C"` block above. The order matches the
/// ABI table in `research/2026-05-07_09-49-11_ainb-plugin-architecture.md`.
pub const HOST_FN_CATALOGUE: &[HostFn] = &[
    HostFn::baseline("ainb_log"),
    HostFn::baseline("ainb_now_ms"),
    HostFn::baseline("ainb_render_buffer"),
    HostFn::gated("ainb_data_read", GatedBy::WritePluginData),
    HostFn::gated("ainb_data_write", GatedBy::WritePluginData),
    HostFn::gated("ainb_event_subscribe", GatedBy::EventBus),
    HostFn::gated("ainb_event_publish", GatedBy::EventBus),
    HostFn::gated("ainb_sessions_list", GatedBy::ReadSessions),
    HostFn::gated("ainb_session_get", GatedBy::ReadSessions),
    HostFn::gated("ainb_http_request", GatedBy::Network),
    HostFn::gated("ainb_fs_read", GatedBy::Filesystem),
    HostFn::gated("ainb_fs_glob", GatedBy::Filesystem),
];

/// Catalogue entry: import name + which capability gates it (if any).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFn {
    pub name: &'static str,
    pub gate: Option<GatedBy>,
}

impl HostFn {
    #[must_use]
    pub const fn baseline(name: &'static str) -> Self {
        Self { name, gate: None }
    }
    #[must_use]
    pub const fn gated(name: &'static str, gate: GatedBy) -> Self {
        Self { name, gate: Some(gate) }
    }
}

/// Discriminator used by [`HostFn::gate`]. Mirrors the boolean flags in
/// [`crate::manifest::CapabilitiesTable`] for the simple capabilities, with
/// dedicated arms for the allowlisted ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedBy {
    ReadSessions,
    WritePluginData,
    EventBus,
    ReadClaudeLogs,
    ReadCodexLogs,
    SpawnSubprocess,
    Network,
    Filesystem,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_fn_catalogue_contains_no_duplicates() {
        let mut names: Vec<&str> = HOST_FN_CATALOGUE.iter().map(|f| f.name).collect();
        names.sort_unstable();
        let dup = names.windows(2).any(|w| w[0] == w[1]);
        assert!(!dup, "duplicate host-fn name in catalogue: {names:?}");
    }

    #[test]
    fn baseline_fns_are_gateless() {
        let baselines: Vec<&str> = HOST_FN_CATALOGUE
            .iter()
            .filter(|f| f.gate.is_none())
            .map(|f| f.name)
            .collect();
        assert_eq!(
            baselines,
            vec!["ainb_log", "ainb_now_ms", "ainb_render_buffer"]
        );
    }

    #[test]
    fn host_status_negative_codes_are_distinct() {
        for v in [-1, -2, -3, -4, -99] {
            // last one falls through to HostError
            let _ = HostStatus::from_i32(v);
        }
        assert_eq!(HostStatus::from_i32(-1), HostStatus::BufferTooSmall);
        assert_eq!(HostStatus::from_i32(-2), HostStatus::NotPermitted);
        assert_eq!(HostStatus::from_i32(-3), HostStatus::InvalidArgument);
        assert_eq!(HostStatus::from_i32(-4), HostStatus::HostError);
        assert_eq!(HostStatus::from_i32(-9999), HostStatus::HostError);
    }
}
