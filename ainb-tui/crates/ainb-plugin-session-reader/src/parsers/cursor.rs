//! Cursor IDE chat session parser — best-effort scaffold.
//!
//! Cursor stores chat history in per-workspace SQLite databases under
//! `~/Library/Application Support/Cursor/User/workspaceStorage/<hash>/`
//! on macOS and the XDG equivalent on linux. The on-disk schema is not
//! stable across Cursor releases, so this v1 stub probes for the
//! directory and returns `Vec::new()` so the host detects "Cursor data
//! present, parser pending" without panicking.
//!
//! See [`super::gemini`] / [`super::copilot`] for the same scaffold
//! pattern. Real parsing lands when the Cursor sqlite schema stabilises
//! across the supported version range — see the SC3 follow-up tracker.

use std::path::Path;

use ainb_plugin_types_sessions::ProviderCall;

/// Walk `<sessions_root>` for Cursor session blobs. Best-effort no-op.
pub fn parse_dir(sessions_root: &Path) -> Vec<ProviderCall> {
    match std::fs::read_dir(sessions_root) {
        Ok(_entries) => {
            tracing::debug!(
                path = %sessions_root.display(),
                "session-reader/cursor: parser stub — no rows emitted"
            );
            Vec::new()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            tracing::warn!(
                path = %sessions_root.display(),
                error = %err,
                "session-reader/cursor: read sessions dir failed"
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

    #[test]
    fn empty_dir_returns_empty_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let calls = parse_dir(dir.path());
        assert!(calls.is_empty());
    }
}
