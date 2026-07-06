//! P5.6 — danger-full-access warning acknowledgements (host-side state).
//!
//! ainb runs agents with `danger-full-access` (full filesystem + network, no
//! container sandbox at v1 — see the build-plan "trust-the-user-with-warnings"
//! decision). The user is warned about that **twice**:
//!
//! 1. **first run ever** — once, the first time the Hangar TUI launches;
//! 2. **first provider invocation per session per provider** — once per
//!    `(provider, session)` pair, so a fresh session re-warns and a second
//!    dispatch in the same session stays quiet.
//!
//! A dismissed warning is recorded as a string key in the `warnings_ack` array
//! of `~/.agents-in-a-box/hangar/state.toml` — the same host state file the workspace
//! switch state lives in ([`crate::workspace_store`]). The two concerns share
//! the file but own disjoint keys: workspace owns `active_workspace` /
//! `default_workspace`, warnings owns `warnings_ack`. Each writer preserves
//! every foreign key/section on save, so they never clobber each other.
//!
//! The ack keys:
//!
//! | warning                       | ack key                              |
//! |-------------------------------|--------------------------------------|
//! | first run ever                | `first_run`                          |
//! | provider `<p>` in session `<s>` | `provider:<p>:session:<s>`         |
//!
//! ## Purity
//!
//! The *decision* (should-warn? which key?) is pure — [`should_warn_first_run`],
//! [`should_warn_provider`], [`provider_session_key`] take values and return
//! values, so they unit-test without touching disk. The IO half (read/write the
//! `warnings_ack` array) lives in [`read_acks_at`] / [`write_acks_at`], and the
//! [`WarningStore`] trait injects either the production `state.toml`-backed store
//! or an in-memory double.

use std::path::{Path, PathBuf};

// The pure ack-key shape + show/skip decision live in `ainb-hangar-core` so the
// daemon (which decides whether a provider invocation warrants a warning) and
// the host (which renders the modal + persists the ack) share one source of
// truth. Re-exported here so host-side callers have one import.
pub use ainb_hangar_core::warnings::{
    FIRST_RUN_KEY, is_provider_ack, provider_session_key, should_warn_first_run,
    should_warn_provider,
};

/// The `state.toml` array key holding dismissed-warning keys.
const WARNINGS_KEY: &str = "warnings_ack";

/// Resolve the default state-file path, identical to the workspace store's
/// (`{hangar_home}/hangar/state.toml`) so both concerns read/write one file.
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved.
pub fn default_state_path() -> std::io::Result<PathBuf> {
    crate::workspace_store::default_state_path()
}

/// Read the `warnings_ack` array from `path`, treating a missing file as empty.
///
/// Only the `warnings_ack` key is read; every foreign key/section is ignored
/// here and preserved by [`write_acks_at`]. A non-array `warnings_ack` value (or
/// a missing key) yields an empty list — a corrupt entry must never down a
/// warning decision.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or is not valid TOML.
pub fn read_acks_at(path: &Path) -> std::io::Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let acks = doc
        .get(WARNINGS_KEY)
        .and_then(toml::Value::as_array)
        .map(|arr| arr.iter().filter_map(toml::Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    Ok(acks)
}

/// Write `acks` as the `warnings_ack` array of `path`, preserving every foreign
/// key/section.
///
/// Reads the existing document (if any), upserts only `warnings_ack`, and writes
/// back atomically (temp sibling + rename) so a crash mid-write can never
/// truncate the file or clobber the workspace switch keys.
///
/// # Errors
///
/// Returns an error if the parent dir, the temp write, or the rename fails.
pub fn write_acks_at(path: &Path, acks: &[String]) -> std::io::Result<()> {
    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = doc.as_table_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state.toml root is not a table",
        )
    })?;
    let array = toml::Value::Array(acks.iter().map(|a| toml::Value::String(a.clone())).collect());
    table.insert(WARNINGS_KEY.to_string(), array);

    let serialised = toml::to_string_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp.warnings");
    std::fs::write(&tmp, serialised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Append `key` to `path`'s `warnings_ack` array if absent, returning whether a
/// write happened. Idempotent: acking an already-acked key is a no-op.
///
/// # Errors
///
/// Returns an error if reading or writing the state file fails.
pub fn ack_at(path: &Path, key: &str) -> std::io::Result<bool> {
    let mut acks = read_acks_at(path)?;
    if acks.iter().any(|a| a == key) {
        return Ok(false);
    }
    acks.push(key.to_string());
    write_acks_at(path, &acks)?;
    Ok(true)
}

/// Remove every ack matching `pred` from `path`'s `warnings_ack` array.
///
/// Returns how many were removed. A `provider` reset passes a predicate matching
/// `provider:<name>:session:*`; a full reset matches everything.
///
/// # Errors
///
/// Returns an error if reading or writing the state file fails.
pub fn reset_at(path: &Path, pred: impl Fn(&str) -> bool) -> std::io::Result<usize> {
    let acks = read_acks_at(path)?;
    let before = acks.len();
    let kept: Vec<String> = acks.into_iter().filter(|a| !pred(a)).collect();
    let removed = before - kept.len();
    if removed > 0 {
        write_acks_at(path, &kept)?;
    }
    Ok(removed)
}

/// The injected host warning store.
///
/// Production reads/writes `~/.agents-in-a-box/hangar/state.toml::warnings_ack`; tests use
/// an in-memory double. The trait keeps the *decision* callers (the plugin
/// first-run flow, the daemon emit seam) free of an `std::io` dependency.
pub trait WarningStore: Send + Sync {
    /// The currently recorded acks.
    fn acks(&self) -> Vec<String>;

    /// Record `key` as acknowledged (idempotent).
    ///
    /// # Errors
    /// Returns an error if the underlying state file cannot be written.
    fn ack(&self, key: &str) -> std::io::Result<()>;
}

/// The production `state.toml`-backed warning store.
pub struct StateTomlWarningStore {
    path: PathBuf,
}

impl StateTomlWarningStore {
    /// Construct a store backed by `path`.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The state-file path this store reads/writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl WarningStore for StateTomlWarningStore {
    fn acks(&self) -> Vec<String> {
        read_acks_at(&self.path).unwrap_or_default()
    }

    fn ack(&self, key: &str) -> std::io::Result<()> {
        ack_at(&self.path, key).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `read_acks_at` on a missing file is empty, not an error.
    #[test]
    fn read_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar").join("state.toml");
        assert!(read_acks_at(&path).unwrap().is_empty());
    }

    /// `ack_at` writes once and is idempotent on the second call.
    #[test]
    fn ack_at_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar").join("state.toml");
        assert!(ack_at(&path, FIRST_RUN_KEY).unwrap());
        assert!(!ack_at(&path, FIRST_RUN_KEY).unwrap());
        assert_eq!(
            read_acks_at(&path).unwrap(),
            vec![FIRST_RUN_KEY.to_string()]
        );
    }

    /// The `StateTomlWarningStore` trait double reads/writes the same file.
    #[test]
    fn store_double_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar").join("state.toml");
        let store = StateTomlWarningStore::new(path);
        assert!(store.acks().is_empty());
        store.ack(FIRST_RUN_KEY).unwrap();
        assert_eq!(store.acks(), vec![FIRST_RUN_KEY.to_string()]);
    }
}
