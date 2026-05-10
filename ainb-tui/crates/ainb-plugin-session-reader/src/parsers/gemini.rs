//! Gemini Code Assist session parser — best-effort scaffold.
//!
//! Phase 7c port: still a stub. We probe the root with [`std::fs::read_dir`]
//! so a permission failure or missing directory is logged once and we
//! return an empty vec. Real parser deferred until the Gemini Code
//! Assist client surfaces a stable on-disk log format.

use std::path::Path;

use ainb_plugin_types_sessions::ProviderCall;

/// Walk `<sessions_root>/...` for Gemini session logs. Best-effort no-op.
pub fn parse_dir(sessions_root: &Path) -> Vec<ProviderCall> {
    match std::fs::read_dir(sessions_root) {
        Ok(_entries) => {
            tracing::debug!(
                path = %sessions_root.display(),
                "session-reader/gemini: parser stub — no rows emitted"
            );
            Vec::new()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            tracing::warn!(
                path = %sessions_root.display(),
                error = %err,
                "session-reader/gemini: read sessions dir failed"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_root_returns_empty_without_panic() {
        let calls = parse_dir(Path::new("/this/does/not/exist"));
        assert!(calls.is_empty());
    }
}
