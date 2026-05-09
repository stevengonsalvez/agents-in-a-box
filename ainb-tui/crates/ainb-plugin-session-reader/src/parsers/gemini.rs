//! Gemini Code Assist session parser — best-effort scaffold.
//!
//! Phase 6b ships the integration surface (capability gating, host fns,
//! event publish) before locking in the Gemini JSONL schema. Until a
//! real Gemini session log is wired up, [`parse_dir`] returns an empty
//! `Vec` after one `fs_read_dir` probe — graceful when the directory
//! doesn't exist or isn't readable, and indistinguishable from "user
//! has no Gemini logs" downstream.
//!
//! TODO(phase-6+): replace this stub with a real parser once the
//! Gemini Code Assist client surfaces a stable on-disk log format.
//! When that lands the per-call shape should match Claude/Codex
//! parsers — produce [`ProviderCall`] with `provider = Provider::Gemini`.

use ainb_plugin_api::host::LogLevel;
use ainb_plugin_types_sessions::ProviderCall;

use crate::host;

/// Walk `<sessions_root>/...` for Gemini session logs. Currently a
/// best-effort no-op: we probe the root via `fs_read_dir` so a
/// permission failure or missing dir is logged once at WARN, then
/// return an empty vec. Real implementation deferred — see module
/// docs.
pub fn parse_dir(sessions_root: &str) -> Vec<ProviderCall> {
    match host::fs_read_dir(sessions_root) {
        Ok(entries) => {
            // Touch the listing so a future implementation can take
            // over without changing the calling convention. For now,
            // log a debug crumb and return empty.
            let _ = entries;
            host::log(
                LogLevel::Debug,
                "session-reader/gemini: parser stub — no rows emitted",
            );
            Vec::new()
        }
        Err(host::HostError::NotPermitted) => {
            host::log(
                LogLevel::Warn,
                "session-reader/gemini: not permitted to read sessions dir",
            );
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}
