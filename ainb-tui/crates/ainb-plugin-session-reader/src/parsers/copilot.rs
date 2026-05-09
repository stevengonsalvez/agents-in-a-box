//! GitHub Copilot Chat session parser — best-effort scaffold.
//!
//! Phase 6b ships the integration surface before locking in the
//! Copilot session log schema; the parser returns an empty vec after
//! one `fs_read_dir` probe. See [`crate::parsers::gemini`] for the
//! same trade-off and the deferred-implementation rationale.
//!
//! TODO(phase-6+): replace with a real parser once the Copilot Chat
//! client surfaces a stable on-disk log format. The per-call shape
//! must match Claude/Codex — produce [`ProviderCall`] with
//! `provider = Provider::Copilot`.

use ainb_plugin_api::host::LogLevel;
use ainb_plugin_types_sessions::ProviderCall;

use crate::host;

/// Walk `<sessions_root>/...` for Copilot session logs. Currently a
/// best-effort no-op — see module docs for why and `gemini.rs` for
/// the parallel scaffold.
pub fn parse_dir(sessions_root: &str) -> Vec<ProviderCall> {
    match host::fs_read_dir(sessions_root) {
        Ok(entries) => {
            let _ = entries;
            host::log(
                LogLevel::Debug,
                "session-reader/copilot: parser stub — no rows emitted",
            );
            Vec::new()
        }
        Err(host::HostError::NotPermitted) => {
            host::log(
                LogLevel::Warn,
                "session-reader/copilot: not permitted to read sessions dir",
            );
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}
