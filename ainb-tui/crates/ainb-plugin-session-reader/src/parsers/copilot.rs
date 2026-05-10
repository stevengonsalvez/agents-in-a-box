//! GitHub Copilot Chat session parser — best-effort scaffold.
//!
//! Phase 7c port: still a stub. See [`super::gemini`] for the
//! deferred-implementation rationale.

use std::path::Path;

use ainb_plugin_types_sessions::ProviderCall;

/// Walk `<sessions_root>/...` for Copilot session logs. Best-effort no-op.
pub fn parse_dir(sessions_root: &Path) -> Vec<ProviderCall> {
    match std::fs::read_dir(sessions_root) {
        Ok(_entries) => {
            tracing::debug!(
                path = %sessions_root.display(),
                "session-reader/copilot: parser stub — no rows emitted"
            );
            Vec::new()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            tracing::warn!(
                path = %sessions_root.display(),
                error = %err,
                "session-reader/copilot: read sessions dir failed"
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
